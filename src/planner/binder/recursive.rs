use super::*;
use crate::planner::RecursiveId;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn common_table(&mut self, cte: &ast::Cte, recursive: bool) -> Result<LogicalPlan> {
        if cte.from.is_some() {
            return Err(unsupported("CTE FROM modifier"));
        }
        if !recursive {
            return self.query(&cte.query);
        }
        let ast::SetExpr::SetOperation {
            op: ast::SetOperator::Union,
            set_quantifier,
            left,
            right,
        } = cte.query.body.as_ref()
        else {
            return self.query(&cte.query);
        };
        // Clauses attached to the whole recursive union have different
        // semantics from clauses inside its parenthesized seed/step queries.
        if cte.query.with.is_some()
            || cte.query.order_by.is_some()
            || cte.query.limit_clause.is_some()
            || cte.query.fetch.is_some()
            || !cte.query.locks.is_empty()
            || cte.query.for_clause.is_some()
            || cte.query.settings.is_some()
            || cte.query.format_clause.is_some()
            || !cte.query.pipe_operators.is_empty()
        {
            return Err(unsupported("recursive UNION query modifiers"));
        }
        let all = match set_quantifier {
            ast::SetQuantifier::All => true,
            ast::SetQuantifier::None | ast::SetQuantifier::Distinct => false,
            _ => return Err(unsupported(set_quantifier)),
        };
        let mut seed = self.set(left)?;
        alias(&mut seed, &cte.alias)?;
        let id = RecursiveId::default();
        let name = cte.alias.name.value.to_ascii_lowercase();
        let saved = self.ctes.insert(
            name.clone(),
            CommonTable {
                plan: LogicalPlan {
                    schema: seed.schema.clone(),
                    node: PlanNode::RecursiveInput(id.clone()),
                },
                depth: self.outer.len(),
            },
        );
        let step = self.set(right);
        match saved {
            Some(previous) => {
                self.ctes.insert(name, previous);
            }
            None => {
                self.ctes.remove(&name);
            }
        }
        let mut step = step?;
        if seed.schema.len() != step.schema.len() {
            return Err(Error::Bind("UNION column counts differ".into()));
        }
        if !step.references_recursive(&id) {
            // WITH RECURSIVE also permits ordinary CTEs. A union without a
            // recursive reference executes each arm once with ordinary coercion.
            let types = seed
                .schema
                .iter()
                .zip(&step.schema)
                .map(|(a, b)| {
                    self.context
                        .query
                        .types()
                        .common_type(&a.data_type, &b.data_type)
                })
                .collect::<Result<Vec<_>>>()?;
            seed = self.coerce_plan(seed, &types, CastMode::Implicit)?;
            step = self.coerce_plan(step, &types, CastMode::Implicit)?;
            return Ok(LogicalPlan {
                schema: seed.schema.clone(),
                node: PlanNode::SetOperation {
                    kind: crate::planner::logical::SetOperation::Union,
                    left: Box::new(seed),
                    right: Box::new(step),
                    all,
                },
            });
        }
        let types = seed
            .schema
            .iter()
            .map(|field| field.data_type.clone())
            .collect::<Vec<_>>();
        step = self.coerce_plan(step, &types, CastMode::Assignment)?;
        Ok(LogicalPlan {
            schema: seed.schema.clone(),
            node: PlanNode::Recursive {
                id,
                seed: Box::new(seed),
                step: Box::new(step),
                all,
            },
        })
    }
}
