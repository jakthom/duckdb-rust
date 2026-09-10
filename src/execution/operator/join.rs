use std::{borrow::Cow, collections::HashMap, fmt::Debug};

use super::super::{
    DataSet, ExecutionContext,
    physical_plan::PhysicalOperator,
    stream::{self, Stream},
};
use crate::{
    common::{Result, Row, Value},
    planner::{BoundExpr, ExprKind, Schema, logical::JoinKind},
};

mod keys;
mod membership;
mod semi;
use keys::EqualityKeys;

/// Borrowed, validated join description. Both children observe the same query
/// snapshot. Semi/anti output has the left schema; other kinds concatenate them.
pub struct JoinPlan<'a> {
    pub left: &'a dyn PhysicalOperator,
    pub right: &'a dyn PhysicalOperator,
    pub kind: JoinKind,
    pub condition: &'a BoundExpr,
    pub schema: &'a Schema,
}

pub trait JoinAlgorithm: Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn supports(&self, condition: &BoundExpr, left_width: usize) -> bool;
    /// Open a fresh cursor without reading rows. Its next calls honor demand,
    /// cancellation, owned output and terminal errors as specified by BatchStream.
    /// Algorithms may retain a bounded build side and stream the probe side.
    /// The default explicitly collects both inputs through the same checked
    /// operator boundary and uses the materialized join implementation.
    fn open<'a>(
        &'a self,
        plan: JoinPlan<'a>,
        context: &'a ExecutionContext<'a>,
    ) -> Result<Stream<'a>> {
        Ok(materialized(self, plan, context))
    }
    fn join(
        &self,
        left: &DataSet,
        right: &DataSet,
        kind: JoinKind,
        condition: &BoundExpr,
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>>;
}

fn materialized<'a, T: JoinAlgorithm + ?Sized>(
    algorithm: &'a T,
    plan: JoinPlan<'a>,
    context: &'a ExecutionContext<'a>,
) -> Stream<'a> {
    stream::deferred(plan.schema, context, move || {
        algorithm.join(
            &stream::collect(plan.left, context)?,
            &stream::collect(plan.right, context)?,
            plan.kind,
            plan.condition,
            context,
        )
    })
}

#[derive(Debug, Default)]
pub struct NestedLoopJoin;

impl JoinAlgorithm for NestedLoopJoin {
    fn name(&self) -> &'static str {
        "nested-loop"
    }
    fn supports(&self, _condition: &BoundExpr, _left_width: usize) -> bool {
        true
    }
    fn join(
        &self,
        left: &DataSet,
        right: &DataSet,
        kind: JoinKind,
        condition: &BoundExpr,
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>> {
        join_candidates(left, right, kind, condition, context, |_| {
            Ok((0..right.rows.len()).collect())
        })
    }
}

#[derive(Debug, Default)]
pub struct HashJoin;

impl JoinAlgorithm for HashJoin {
    fn name(&self) -> &'static str {
        "hash"
    }
    fn supports(&self, condition: &BoundExpr, left_width: usize) -> bool {
        EqualityKeys::bind(condition, left_width).is_some()
    }
    fn open<'a>(
        &'a self,
        plan: JoinPlan<'a>,
        context: &'a ExecutionContext<'a>,
    ) -> Result<Stream<'a>> {
        if matches!(plan.kind, JoinKind::Semi | JoinKind::Anti)
            && let Some(keys) = EqualityKeys::bind(plan.condition, plan.left.schema().len())
        {
            semi::open(plan, keys, context)
        } else {
            Ok(materialized(self, plan, context))
        }
    }
    fn join(
        &self,
        left: &DataSet,
        right: &DataSet,
        kind: JoinKind,
        condition: &BoundExpr,
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>> {
        let Some(keys) = EqualityKeys::bind(condition, left.schema.len()) else {
            return Err(crate::Error::Unsupported(
                "hash join requires equal-typed pure total keys from each input".into(),
            ));
        };
        let data_type = &keys.data_type;
        let mut index: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
        for (i, row) in right.rows.iter().enumerate() {
            context.query.check()?;
            let value = key_value(&keys.right, row, context)?;
            if value.is_null() {
                continue;
            }
            let mut key = Vec::new();
            data_type.append_key(&value, &mut key, context.query)?;
            index.entry(key).or_default().push(i);
        }
        join_candidates(left, right, kind, condition, context, |row| {
            let value = key_value(&keys.left, row, context)?;
            if value.is_null() {
                return Ok(Vec::new());
            }
            let mut key = Vec::new();
            data_type.append_key(&value, &mut key, context.query)?;
            Ok(index.get(&key).cloned().unwrap_or_default())
        })
    }
}

fn key_value<'a>(
    expression: &BoundExpr,
    row: &'a Row,
    context: &ExecutionContext<'_>,
) -> Result<Cow<'a, Value>> {
    if let ExprKind::Column(index) = expression.kind {
        row.get(index)
            .map(Cow::Borrowed)
            .ok_or_else(|| crate::Error::Internal("join key outside input row".into()))
    } else {
        context
            .expressions
            .evaluate(expression, row, context)
            .map(Cow::Owned)
    }
}

fn join_candidates(
    left: &DataSet,
    right: &DataSet,
    kind: JoinKind,
    condition: &BoundExpr,
    context: &ExecutionContext<'_>,
    candidates: impl Fn(&Row) -> Result<Vec<usize>>,
) -> Result<Vec<Row>> {
    let condition = crate::execution::subquery::PreparedExpression::new(condition);
    let mut output = Vec::new();
    let mut matched_right = vec![false; right.rows.len()];
    let mut emit = |row| -> Result<()> {
        context.query.check_rows(output.len() + 1)?;
        output.push(row);
        Ok(())
    };
    for left_row in &left.rows {
        context.query.check()?;
        let mut matched = false;
        for index in candidates(left_row)? {
            context.query.check()?;
            let mut row = left_row.clone();
            row.extend(right.rows[index].clone());
            if condition.evaluate(&row, context)?.as_bool()? != Some(true) {
                continue;
            }
            matched = true;
            matched_right[index] = true;
            match kind {
                JoinKind::Anti => break,
                JoinKind::Semi => {
                    emit(left_row.clone())?;
                    break;
                }
                _ => emit(row)?,
            }
        }
        if !matched {
            match kind {
                JoinKind::Anti => emit(left_row.clone())?,
                JoinKind::Left | JoinKind::Full => {
                    let mut row = left_row.clone();
                    row.extend(vec![Value::Null; right.schema.len()]);
                    emit(row)?;
                }
                _ => {}
            }
        }
    }
    if matches!(kind, JoinKind::Right | JoinKind::Full) {
        for (index, row) in right.rows.iter().enumerate() {
            if !matched_right[index] {
                let mut output = vec![Value::Null; left.schema.len()];
                output.extend(row.clone());
                emit(output)?;
            }
        }
    }
    Ok(output)
}
