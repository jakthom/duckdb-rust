use std::{collections::HashMap, fmt::Debug};

use super::super::{DataSet, ExecutionContext};
use crate::{
    common::{Result, Row, Value},
    planner::{BoundExpr, ExprKind, expression::BinaryOp, logical::JoinKind},
};

pub trait JoinAlgorithm: Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn supports(&self, condition: &BoundExpr, left_width: usize) -> bool;
    fn join(
        &self,
        left: &DataSet,
        right: &DataSet,
        kind: JoinKind,
        condition: &BoundExpr,
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>>;
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
        equality_keys(condition, left_width).is_some()
    }
    fn join(
        &self,
        left: &DataSet,
        right: &DataSet,
        kind: JoinKind,
        condition: &BoundExpr,
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>> {
        let Some((l, r)) = equality_keys(condition, left.schema.len()) else {
            return Err(crate::Error::Unsupported(
                "hash join requires equal-typed column equality".into(),
            ));
        };
        let data_type = context.query.types().bind(&left.schema[l].data_type)?;
        let mut index: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
        for (i, row) in right.rows.iter().enumerate() {
            context.query.check()?;
            if row[r].is_null() {
                continue;
            }
            let mut key = Vec::new();
            data_type.append_key(&row[r], &mut key, context.query)?;
            index.entry(key).or_default().push(i);
        }
        join_candidates(left, right, kind, condition, context, |row| {
            if row[l].is_null() {
                return Ok(Vec::new());
            }
            let mut key = Vec::new();
            data_type.append_key(&row[l], &mut key, context.query)?;
            Ok(index.get(&key).cloned().unwrap_or_default())
        })
    }
}

fn equality_keys(condition: &BoundExpr, left_width: usize) -> Option<(usize, usize)> {
    let ExprKind::Binary(BinaryOp::Equal, left, right, _) = &condition.kind else {
        return None;
    };
    let (ExprKind::Column(l), ExprKind::Column(r)) = (&left.kind, &right.kind) else {
        return None;
    };
    if left.data_type != right.data_type {
        return None;
    }
    if *l < left_width && *r >= left_width {
        Some((*l, *r - left_width))
    } else if *r < left_width && *l >= left_width {
        Some((*r, *l - left_width))
    } else {
        None
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
