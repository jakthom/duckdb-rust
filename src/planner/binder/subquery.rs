use super::*;
use crate::planner::expression::{BoundSubquery, SubqueryKind};
use std::sync::Arc;

pub(super) enum SubqueryForm {
    Scalar,
    Exists { negated: bool },
    In { needle: BoundExpr, negated: bool },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn column(
        &self,
        parts: &[String],
        fields: &Scope,
        grouping: Option<&GroupScope>,
    ) -> Result<BoundExpr> {
        if let Some(index) = fields.resolve_optional(parts)? {
            if grouping.is_some() {
                return Err(Error::Bind(format!(
                    "column \"{}\" must appear in the GROUP BY clause or must be part of an aggregate function.",
                    parts.join(".")
                )));
            }
            return Ok(BoundExpr::column(index, fields[index].data_type.clone()));
        }
        for (depth, scope) in self.outer.iter().rev().enumerate() {
            if let Some(index) = scope.fields.resolve_optional(parts)? {
                let column = scope.columns[index].ok_or_else(|| {
                    Error::Bind(format!(
                        "outer column {} must appear in GROUP BY",
                        parts.join(".")
                    ))
                })?;
                return Ok(BoundExpr {
                    kind: ExprKind::OuterColumn {
                        depth: depth + 1,
                        column,
                    },
                    data_type: scope.fields[index].data_type.clone(),
                });
            }
        }
        for prefix in (1..parts.len()).rev() {
            if let Ok(mut value) = self.column(&parts[..prefix], fields, grouping)
                && matches!(value.data_type, DataType::Nested(_))
            {
                for field in &parts[prefix..] {
                    value = self
                        .nested_access(value, BoundExpr::literal(Value::Varchar(field.clone())))?;
                }
                return Ok(value);
            }
        }
        Err(Error::Bind(format!("column {} not found", parts.join("."))))
    }

    pub(super) fn subquery(
        &self,
        query: &ast::Query,
        fields: &Scope,
        grouping: Option<&GroupScope>,
        form: SubqueryForm,
    ) -> Result<BoundExpr> {
        if self.outer.len() >= 128 {
            return Err(Error::Resource("subquery nesting exceeds 128".into()));
        }
        let mut outer = self.outer.clone();
        outer.push(CorrelationScope {
            fields: fields.clone(),
            columns: (0..fields.len())
                .map(|column| match grouping {
                    None => Some(column),
                    Some(grouping) => grouping.groups.iter().position(
                        |(_, expr)| matches!(expr.kind, ExprKind::Column(i) if i == column),
                    ),
                })
                .collect(),
        });
        let mut nested = State {
            context: self.context,
            parameters_allowed: self.parameters_allowed,
            ctes: self.ctes.clone(),
            outer,
        };
        let mut plan = nested.query(query)?;
        let (kind, data_type) = if let SubqueryForm::Exists { negated } = form {
            plan = existence(plan);
            (SubqueryKind::Exists { negated }, DataType::Boolean)
        } else {
            if plan.schema.len() != 1 {
                return Err(Error::Bind(
                    "scalar and IN subqueries require exactly one column".into(),
                ));
            }
            if let SubqueryForm::In { needle, negated } = form {
                let common = self.comparison_type(
                    &needle.data_type,
                    super::coercion::string_literal(&needle),
                    &plan.schema[0].data_type,
                    false,
                    true,
                )?;
                let needle = self.combination_cast(needle, &common)?;
                let mode = self.combination_cast_mode(&plan.schema[0].data_type, &common)?;
                plan = self.coerce_plan(plan, std::slice::from_ref(&common), mode)?;
                (
                    SubqueryKind::In {
                        needle: Box::new(needle),
                        negated,
                        operand_type: self.context.query.types().bind(&common)?.into(),
                    },
                    DataType::Boolean,
                )
            } else {
                (SubqueryKind::Scalar, plan.schema[0].data_type.clone())
            }
        };
        Ok(BoundExpr {
            kind: ExprKind::Subquery(Arc::new(BoundSubquery {
                plan: Arc::new(plan),
                kind,
            })),
            data_type,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Remove unused value work while preserving row count, including under OFFSET.
/// Filters/distinct/joins retain their dependencies and full input expressions.
fn existence(plan: LogicalPlan) -> LogicalPlan {
    match plan.node {
        PlanNode::Projection { input, .. } | PlanNode::Sort { input, .. } => existence(*input),
        PlanNode::Limit {
            input,
            limit,
            offset,
        } => {
            let input = existence(*input);
            LogicalPlan {
                schema: input.schema.clone(),
                node: PlanNode::Limit {
                    input: Box::new(input),
                    limit,
                    offset,
                },
            }
        }
        PlanNode::Aggregate {
            input,
            mut aggregation,
        } => {
            aggregation.outputs.clear();
            LogicalPlan {
                schema: plan.schema[..aggregation.groups.len()].to_vec(),
                node: PlanNode::Aggregate { input, aggregation },
            }
        }
        _ => plan,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// An inlined CTE retains its defining lexical scope when used deeper inside
/// expression subqueries. References bound within the CTE itself do not shift.
pub(super) fn rebase_cte(
    plan: LogicalPlan,
    shift: usize,
    local_depth: usize,
) -> Result<LogicalPlan> {
    if shift == 0 {
        return Ok(plan);
    }
    plan.map_inputs(|input| rebase_cte(input, shift, local_depth))?
        .map_expressions(|expr| rebase_expression(expr, shift, local_depth))
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn rebase_expression(expr: BoundExpr, shift: usize, local_depth: usize) -> Result<BoundExpr> {
    let mut expr = expr.map_children(|child| rebase_expression(child, shift, local_depth))?;
    match &mut expr.kind {
        ExprKind::OuterColumn { depth, .. } if *depth > local_depth => *depth += shift,
        ExprKind::Subquery(query) => {
            let plan = &mut Arc::make_mut(query).plan;
            *plan = Arc::new(rebase_cte(plan.as_ref().clone(), shift, local_depth + 1)?)
        }
        _ => (),
    }
    Ok(expr)
}
