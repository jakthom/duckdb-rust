use super::*;

impl State<'_, '_> {
    pub(super) fn query(&mut self, query: &ast::Query) -> Result<LogicalPlan> {
        if query.fetch.is_some()
            || !query.locks.is_empty()
            || query.for_clause.is_some()
            || query.settings.is_some()
            || query.format_clause.is_some()
            || !query.pipe_operators.is_empty()
        {
            return Err(unsupported("query modifiers"));
        }
        let saved = self.ctes.clone();
        let result = (|| {
            if let Some(with) = &query.with {
                if with.recursive {
                    return Err(unsupported("recursive CTE"));
                }
                for cte in &with.cte_tables {
                    let mut plan = self.query(&cte.query)?;
                    alias(&mut plan, &cte.alias)?;
                    self.ctes.insert(
                        cte.alias.name.value.to_ascii_lowercase(),
                        CommonTable {
                            plan,
                            depth: self.outer.len(),
                        },
                    );
                }
            }
            let order = order_expressions(query.order_by.as_ref())?;
            let mut plan = if let ast::SetExpr::Select(select) = query.body.as_ref() {
                self.select(select, &order)?
            } else {
                let mut plan = self.set(&query.body)?;
                if !order.is_empty() {
                    let order = order
                        .iter()
                        .map(|o| self.order(o, &plan.schema, None))
                        .collect::<Result<_>>()?;
                    plan = LogicalPlan {
                        schema: plan.schema.clone(),
                        node: PlanNode::Sort {
                            input: Box::new(plan),
                            order,
                        },
                    };
                }
                plan
            };
            if let Some(clause) = &query.limit_clause {
                let (limit, offset) = match clause {
                    ast::LimitClause::LimitOffset {
                        limit,
                        offset,
                        limit_by,
                    } if limit_by.is_empty() => (
                        limit.as_ref().map(|e| self.nonnegative(e)).transpose()?,
                        offset
                            .as_ref()
                            .map(|o| self.nonnegative(&o.value))
                            .transpose()?
                            .unwrap_or(0),
                    ),
                    _ => return Err(unsupported(clause)),
                };
                plan = LogicalPlan {
                    schema: plan.schema.clone(),
                    node: PlanNode::Limit {
                        input: Box::new(plan),
                        limit,
                        offset,
                    },
                };
            }
            Ok(plan)
        })();
        self.ctes = saved;
        result
    }

    pub(super) fn set(&mut self, set: &ast::SetExpr) -> Result<LogicalPlan> {
        match set {
            ast::SetExpr::Select(select) => self.select(select, &[]),
            ast::SetExpr::Query(query) => self.query(query),
            ast::SetExpr::Values(values) => {
                let mut rows = values
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|e| self.expr(e, &[], None))
                            .collect::<Result<Vec<_>>>()
                    })
                    .collect::<Result<Vec<_>>>()?;
                let width = rows.first().map_or(0, Vec::len);
                if rows.iter().any(|r| r.len() != width) {
                    return Err(Error::Bind("VALUES rows differ in width".into()));
                }
                let mut types = vec![DataType::Null; width];
                for row in &rows {
                    for (t, e) in types.iter_mut().zip(row) {
                        *t = self.context.query.types().common_type(t, &e.data_type)?;
                    }
                }
                for row in &mut rows {
                    for (e, t) in row.iter_mut().zip(&types) {
                        *e = e.clone().cast(
                            t.clone(),
                            CastMode::Implicit,
                            self.context.casts,
                            self.context.query.types(),
                        )?;
                    }
                }
                Ok(LogicalPlan {
                    schema: types
                        .into_iter()
                        .enumerate()
                        .map(|(i, t)| Field::new(format!("col{i}"), t))
                        .collect(),
                    node: PlanNode::Values(rows),
                })
            }
            ast::SetExpr::SetOperation {
                op: ast::SetOperator::Union,
                set_quantifier,
                left,
                right,
            } => {
                let mut left = self.set(left)?;
                let mut right = self.set(right)?;
                if left.schema.len() != right.schema.len() {
                    return Err(Error::Bind("UNION column counts differ".into()));
                }
                let types = left
                    .schema
                    .iter()
                    .zip(&right.schema)
                    .map(|(l, r)| {
                        self.context
                            .query
                            .types()
                            .common_type(&l.data_type, &r.data_type)
                    })
                    .collect::<Result<Vec<_>>>()?;
                left = self.coerce_plan(left, &types, CastMode::Implicit)?;
                right = self.coerce_plan(right, &types, CastMode::Implicit)?;
                let all = match set_quantifier {
                    ast::SetQuantifier::All => true,
                    ast::SetQuantifier::None | ast::SetQuantifier::Distinct => false,
                    _ => return Err(unsupported(set_quantifier)),
                };
                Ok(LogicalPlan {
                    schema: left.schema.clone(),
                    node: PlanNode::Union {
                        left: Box::new(left),
                        right: Box::new(right),
                        all,
                    },
                })
            }
            _ => Err(unsupported(set)),
        }
    }

    pub(super) fn select(
        &mut self,
        select: &ast::Select,
        order: &[ast::OrderByExpr],
    ) -> Result<LogicalPlan> {
        if select.top.is_some()
            || select.into.is_some()
            || select.qualify.is_some()
            || !select.named_window.is_empty()
            || !select.lateral_views.is_empty()
            || select.prewhere.is_some()
            || !select.connect_by.is_empty()
            || !select.cluster_by.is_empty()
            || !select.distribute_by.is_empty()
            || !select.sort_by.is_empty()
            || select.exclude.is_some()
        {
            return Err(unsupported("SELECT modifiers"));
        }
        let distinct = match &select.distinct {
            None => false,
            Some(ast::Distinct::Distinct) => true,
            _ => return Err(unsupported("DISTINCT ON")),
        };
        let mut input = LogicalPlan {
            schema: vec![],
            node: PlanNode::Values(vec![vec![]]),
        };
        for (index, from) in select.from.iter().enumerate() {
            let next = self.from(from)?;
            input = if index == 0 {
                next
            } else {
                join(
                    input,
                    next,
                    JoinKind::Inner,
                    BoundExpr::literal(Value::Boolean(true)),
                )
            };
        }
        if let Some(predicate) = &select.selection {
            let predicate = self.boolean(self.expr(predicate, &input.schema, None)?)?;
            input = LogicalPlan {
                schema: input.schema.clone(),
                node: PlanNode::Filter {
                    input: Box::new(input),
                    predicate,
                },
            };
        }
        let mut items = Vec::<(ast::Expr, String)>::new();
        for item in &select.projection {
            match item {
                ast::SelectItem::UnnamedExpr(e) => items.push((
                    e.clone(),
                    match e {
                        ast::Expr::Identifier(i) => i.value.clone(),
                        ast::Expr::CompoundIdentifier(i) => {
                            i.last().map(|v| v.value.clone()).unwrap_or_default()
                        }
                        _ => e.to_string(),
                    },
                )),
                ast::SelectItem::ExprWithAlias { expr, alias } => {
                    items.push((expr.clone(), alias.value.clone()))
                }
                ast::SelectItem::Wildcard(options) => {
                    check_wildcard(options)?;
                    expand_star(&input.schema, None, &mut items);
                }
                ast::SelectItem::QualifiedWildcard(
                    ast::SelectItemQualifiedWildcardKind::ObjectName(name),
                    options,
                ) => {
                    check_wildcard(options)?;
                    let name = name.to_string();
                    if !input.schema.iter().any(|f| {
                        f.qualifier
                            .as_ref()
                            .is_some_and(|q| q.eq_ignore_ascii_case(&name))
                    }) {
                        return Err(Error::Bind(format!("table {name} not found")));
                    }
                    expand_star(&input.schema, Some(&name), &mut items);
                }
                _ => return Err(unsupported(item)),
            }
        }
        if items.is_empty() {
            return Err(Error::Bind("SELECT has no columns".into()));
        }
        let group_exprs = match &select.group_by {
            ast::GroupByExpr::Expressions(expressions, modifiers) if modifiers.is_empty() => {
                expressions
            }
            _ => return Err(unsupported("GROUP BY ALL or grouping sets")),
        };
        let mut groups = Vec::new();
        for expr in group_exprs {
            let expr = if let Some(index) = ordinal(expr, items.len())? {
                items[index].0.clone()
            } else if let ast::Expr::Identifier(name) = expr {
                if resolve(&input.schema, std::slice::from_ref(&name.value)).is_ok() {
                    expr.clone()
                } else {
                    items
                        .iter()
                        .find(|(_, alias)| alias.eq_ignore_ascii_case(&name.value))
                        .map(|(e, _)| e.clone())
                        .unwrap_or_else(|| expr.clone())
                }
            } else {
                expr.clone()
            };
            groups.push((expr.clone(), self.expr(&expr, &input.schema, None)?));
        }
        let aggregate = !groups.is_empty()
            || items.iter().any(|(e, _)| self.has_aggregate(e))
            || select
                .having
                .as_ref()
                .is_some_and(|e| self.has_aggregate(e))
            || order.iter().any(|o| self.has_aggregate(&o.expr));
        let grouping = aggregate.then(|| GroupScope {
            groups,
            aggregates: RefCell::new(Vec::new()),
        });
        let mut expressions = items
            .iter()
            .map(|(e, _)| self.expr(e, &input.schema, grouping.as_ref()))
            .collect::<Result<Vec<_>>>()?;
        let mut fields: Schema = items
            .iter()
            .zip(&expressions)
            .map(|((_, name), e)| Field::new(name, e.data_type.clone()))
            .collect();
        let visible = fields.len();
        let having = select
            .having
            .as_ref()
            .map(|e| {
                self.expr(e, &input.schema, grouping.as_ref())
                    .and_then(|e| self.boolean(e))
            })
            .transpose()?;
        let mut bound_order = Vec::new();
        for item in order {
            let projected = if let Some(index) = ordinal(&item.expr, visible)? {
                Some(index)
            } else if let ast::Expr::Identifier(name) = &item.expr {
                resolve(&fields[..visible], std::slice::from_ref(&name.value)).ok()
            } else {
                items.iter().position(|(e, _)| *e == item.expr)
            };
            let index = if let Some(index) = projected {
                index
            } else {
                if distinct {
                    return Err(unsupported("DISTINCT ordering by an unselected expression"));
                }
                let expr = self.expr(&item.expr, &input.schema, grouping.as_ref())?;
                fields.push(Field::new(item.expr.to_string(), expr.data_type.clone()));
                expressions.push(expr);
                fields.len() - 1
            };
            bound_order.push(OrderExpr {
                expression: BoundExpr::column(index, fields[index].data_type.clone()),
                descending: item.options.asc == Some(false),
                nulls_first: item.options.nulls_first.unwrap_or(false),
            });
        }
        if let Some(grouping) = grouping {
            let aggregates = grouping.aggregates.into_inner();
            let mut schema: Schema = grouping
                .groups
                .iter()
                .map(|(e, b)| Field::new(e.to_string(), b.data_type.clone()))
                .collect();
            schema.extend(
                aggregates
                    .iter()
                    .map(|(e, a)| Field::new(e.to_string(), a.data_type.clone())),
            );
            input = LogicalPlan {
                schema,
                node: PlanNode::Aggregate {
                    input: Box::new(input),
                    groups: grouping.groups.into_iter().map(|(_, e)| e).collect(),
                    aggregates: aggregates.into_iter().map(|(_, a)| a).collect(),
                },
            };
        } else if having.is_some() {
            return Err(Error::Bind(
                "HAVING requires grouping or aggregation".into(),
            ));
        }
        if let Some(predicate) = having {
            input = LogicalPlan {
                schema: input.schema.clone(),
                node: PlanNode::Filter {
                    input: Box::new(input),
                    predicate,
                },
            };
        }
        let mut plan = LogicalPlan {
            schema: fields,
            node: PlanNode::Projection {
                input: Box::new(input),
                expressions,
            },
        };
        if distinct {
            plan = LogicalPlan {
                schema: plan.schema.clone(),
                node: PlanNode::Distinct(Box::new(plan)),
            };
        }
        if !bound_order.is_empty() {
            plan = LogicalPlan {
                schema: plan.schema.clone(),
                node: PlanNode::Sort {
                    input: Box::new(plan),
                    order: bound_order,
                },
            };
        }
        if plan.schema.len() > visible {
            let fields = plan.schema[..visible].to_vec();
            let expressions = fields
                .iter()
                .enumerate()
                .map(|(i, f)| BoundExpr::column(i, f.data_type.clone()))
                .collect();
            plan = LogicalPlan {
                schema: fields,
                node: PlanNode::Projection {
                    input: Box::new(plan),
                    expressions,
                },
            };
        }
        Ok(plan)
    }
}
