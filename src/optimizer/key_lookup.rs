use std::collections::BTreeMap;

use super::*;
use crate::common::Error;
use crate::planner::expression::BinaryOp;

/// Selects exact equality keys while retaining the complete predicate. Only
/// total, effect-free column/constant comparisons are eligible: skipping rows
/// must not suppress errors or calls with effects in a residual expression.
pub struct UseKeyLookup;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl OptimizerPass for UseKeyLookup {
    fn name(&self) -> &'static str {
        "key-lookup"
    }
    fn rewrite(
        &self,
        mut plan: LogicalPlan,
        context: &OptimizerContext<'_>,
    ) -> Result<LogicalPlan> {
        if !context.storage.capabilities().key_lookup {
            return Ok(plan);
        }
        if let PlanNode::Filter { input, predicate } = &mut plan.node
            && let PlanNode::Scan(table) = &input.node
        {
            let mut equalities = BTreeMap::new();
            if !collect_equalities(predicate, &mut equalities, context.query) {
                return Ok(plan);
            }
            for columns in context.storage.key_columns(table)? {
                if columns.is_empty() {
                    continue;
                }
                let key: Option<Vec<_>> =
                    columns.iter().map(|i| equalities.get(i).cloned()).collect();
                if let Some(key) = key {
                    input.node = PlanNode::KeyLookup {
                        table: table.clone(),
                        columns,
                        key,
                    };
                    break;
                }
            }
        }
        Ok(plan)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn collect_equalities(
    expression: &BoundExpr,
    keys: &mut BTreeMap<usize, Value>,
    context: &QueryContext,
) -> bool {
    match &expression.kind {
        ExprKind::Binary(BinaryOp::And, left, right, _) => {
            collect_equalities(left, keys, context) && collect_equalities(right, keys, context)
        }
        ExprKind::Binary(BinaryOp::Equal, left, right, _) => {
            for (column, value) in [(left, right), (right, left)] {
                if let ExprKind::Column(index) = column.kind
                    && let Some(value) = constant(value, context)
                {
                    keys.entry(index).or_insert(value);
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn constant(expression: &BoundExpr, context: &QueryContext) -> Option<Value> {
    match &expression.kind {
        ExprKind::Literal(value) | ExprKind::Parameter(value) => value
            .fits_type(&expression.data_type)
            .then(|| value.clone()),
        ExprKind::Cast(inner, cast, try_cast) => {
            match cast.apply(&constant(inner, context)?, context) {
                Ok(value) => Some(value),
                Err(Error::Conversion(_)) if *try_cast => Some(Value::Null),
                Err(_) => None,
            }
        }
        _ => None,
    }
}
