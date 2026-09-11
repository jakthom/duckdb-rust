use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn query(&mut self, query: &ast::Query) -> Result<LogicalPlan> {
        self.query_with_value_types(query, None)
    }

    /// INSERT supplies destination types to a direct VALUES source (including
    /// parentheses), not to WITH/SELECT inputs or set-operation branches.
    /// Assigning each expression first preserves its text and avoids intermediate
    /// rounding. A source-level WITH clause makes VALUES a regular query in C++.
    pub(super) fn query_with_value_types(
        &mut self,
        query: &ast::Query,
        value_types: Option<&[DataType]>,
    ) -> Result<LogicalPlan> {
        let value_types = value_types.filter(|_| query.with.is_none());
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
                let mut names = HashSet::new();
                for cte in &with.cte_tables {
                    if !names.insert(cte.alias.name.value.to_ascii_lowercase()) {
                        return Err(Error::Bind("duplicate CTE name".into()));
                    }
                    let mut plan = self.common_table(cte, with.recursive)?;
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
            let mut plan = if let ast::SetExpr::Select(select) = query.body.as_ref() {
                self.select(select, query.order_by.as_ref())?
            } else {
                let mut plan = match query.body.as_ref() {
                    ast::SetExpr::Values(values) => self.values(values, value_types)?,
                    ast::SetExpr::Query(query) => {
                        self.query_with_value_types(query, value_types)?
                    }
                    body => self.set(body)?,
                };
                let order = order_expressions(query.order_by.as_ref(), plan.schema.len())?;
                if !order.is_empty() {
                    let order = order
                        .iter()
                        .map(|o| self.order(o, &Scope::from(plan.schema.clone()), None))
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
            ast::SetExpr::Select(select) => self.select(select, None),
            ast::SetExpr::Query(query) => self.query(query),
            ast::SetExpr::Values(values) => self.values(values, None),
            ast::SetExpr::SetOperation {
                op,
                set_quantifier,
                left,
                right,
            } => {
                let mut left = self.set(left)?;
                let mut right = self.set(right)?;
                if left.schema.len() != right.schema.len() {
                    return Err(Error::Bind(format!("{op} column counts differ")));
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
                left = self.combine_plan(left, &types)?;
                right = self.combine_plan(right, &types)?;
                let all = match set_quantifier {
                    ast::SetQuantifier::All => true,
                    ast::SetQuantifier::None | ast::SetQuantifier::Distinct => false,
                    _ => return Err(unsupported(set_quantifier)),
                };
                Ok(LogicalPlan {
                    schema: left.schema.clone(),
                    node: PlanNode::SetOperation {
                        kind: match op {
                            ast::SetOperator::Union => super::super::logical::SetOperation::Union,
                            ast::SetOperator::Intersect => {
                                super::super::logical::SetOperation::Intersect
                            }
                            ast::SetOperator::Except | ast::SetOperator::Minus => {
                                super::super::logical::SetOperation::Except
                            }
                        },
                        left: Box::new(left),
                        right: Box::new(right),
                        all,
                    },
                })
            }
            _ => Err(unsupported(set)),
        }
    }

    fn values(
        &self,
        values: &ast::Values,
        destinations: Option<&[DataType]>,
    ) -> Result<LogicalPlan> {
        let mut rows = values
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|e| self.expr(e, &Scope::default(), None))
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        let width = rows.first().map_or(0, Vec::len);
        if rows.iter().any(|row| row.len() != width) {
            return Err(Error::Bind("VALUES rows differ in width".into()));
        }
        let types = if let Some(types) = destinations {
            if types.len() != width {
                return Err(Error::Bind(
                    "INSERT column count does not match source".into(),
                ));
            }
            types.to_vec()
        } else {
            // Development's VALUES binder seeds each column with SQL NULL and
            // combines GetExpressionReturnType for every row, including first.
            // The seed normalizes that first literal; later literals retain
            // their own hints until the next selected pair is combined.
            let initial = BoundExpr::literal(Value::Null);
            (0..width)
                .map(|index| {
                    self.ordered_combination_type(
                        std::iter::once(&initial).chain(rows.iter().map(|row| &row[index])),
                        super::coercion::CombinationSequence::Values,
                    )
                })
                .collect::<Result<Vec<_>>>()?
        };
        for row in &mut rows {
            for (expression, data_type) in row.iter_mut().zip(&types) {
                *expression = if destinations.is_some() {
                    expression.clone().cast(
                        data_type.clone(),
                        CastMode::Assignment,
                        self.context.casts,
                        self.context.query.types(),
                    )?
                } else {
                    self.combination_cast(expression.clone(), data_type)?
                };
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

    pub(super) fn select(
        &mut self,
        select: &ast::Select,
        order: Option<&ast::OrderBy>,
    ) -> Result<LogicalPlan> {
        if select.top.is_some()
            || select.into.is_some()
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
        let mut scope = Scope::default();
        for (index, from) in select.from.iter().enumerate() {
            let next = self.from(from)?;
            scope = if index == 0 {
                next.scope
            } else {
                scope.combine(&next.scope)
            };
            input = if index == 0 {
                next.plan
            } else {
                join(
                    input,
                    next.plan,
                    JoinKind::Inner,
                    BoundExpr::literal(Value::Boolean(true)),
                )
            };
        }
        if let Some(predicate) = &select.selection {
            let predicate = self.boolean(self.expr(predicate, &scope, None)?)?;
            input = LogicalPlan {
                schema: input.schema.clone(),
                node: PlanNode::Filter {
                    input: Box::new(input),
                    predicate,
                },
            };
        }
        let mut items = Vec::<SelectItem>::new();
        for item in &select.projection {
            match item {
                ast::SelectItem::UnnamedExpr(e) => items.push(SelectItem::expression(
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
                    items.push(SelectItem::expression(expr.clone(), alias.value.clone()))
                }
                ast::SelectItem::Wildcard(options) => {
                    check_wildcard(options)?;
                    items.extend(scope.star(None)?);
                }
                ast::SelectItem::QualifiedWildcard(
                    ast::SelectItemQualifiedWildcardKind::ObjectName(name),
                    options,
                ) => {
                    check_wildcard(options)?;
                    if name.0.len() != 1 {
                        return Err(Error::Parse(
                            "Did not expect more than one column in front of a star expression"
                                .into(),
                        ));
                    }
                    let name = name.0[0].as_ident().ok_or_else(|| unsupported(name))?;
                    items.extend(scope.star(Some(&name.value))?);
                }
                _ => return Err(unsupported(item)),
            }
        }
        if items.is_empty() {
            return Err(Error::Bind("SELECT has no columns".into()));
        }
        let order = order_expressions(order, items.len())?;
        let mut windows = window::Windows::new(&select.named_window)?;
        let roots = items
            .iter()
            .map(|item| &item.expression)
            .chain(select.having.iter())
            .chain(select.qualify.iter())
            .chain(order.iter().map(|key| &key.expr))
            .collect::<Vec<_>>();
        for expression in &roots {
            windows.gather(expression)?;
        }
        let window_keys = windows
            .calls
            .iter()
            .flat_map(|(_, spec)| {
                spec.partition_by
                    .iter()
                    .chain(spec.order_by.iter().map(|key| &key.expr))
            })
            .collect::<Vec<_>>();
        let bound_groups = self.group_by(&select.group_by, &scope, &items)?;
        let groups = bound_groups.groups;
        let aggregate = bound_groups.explicit
            || roots
                .iter()
                .chain(&window_keys)
                .any(|expr| self.has_aggregate(expr));
        let grouping = aggregate.then(|| {
            let aliases = items
                .iter()
                .filter_map(|item| {
                    item.group_index(&scope, &groups)
                        .map(|index| (item.name.to_ascii_lowercase(), index))
                })
                .collect();
            GroupScope {
                groups,
                aliases,
                outputs: RefCell::new(Vec::new()),
            }
        });
        if !windows.calls.is_empty()
            && let Some(grouping) = &grouping
        {
            for expression in roots.iter().chain(&window_keys) {
                self.collect_aggregates(expression, &scope, grouping)?;
            }
        }
        let window_input_width = grouping.as_ref().map_or(input.schema.len(), |grouping| {
            grouping.groups.len() + grouping.outputs.borrow().len()
        });
        let mut projection_scope = scope.clone();
        let window_scope = std::rc::Rc::new(window::WindowScope::new(window_input_width, windows));
        projection_scope.windows = Some(window_scope.clone());
        let mut expressions = items
            .iter()
            .map(|item| item.bind(self, &projection_scope, grouping.as_ref()))
            .collect::<Result<Vec<_>>>()?;
        let mut fields: Schema = items
            .iter()
            .zip(&expressions)
            .map(|(item, e)| Field::new(&item.name, e.data_type.clone()))
            .collect();
        let visible = fields.len();
        let having = select
            .having
            .as_ref()
            .map(|e| {
                self.expr(e, &scope, grouping.as_ref())
                    .and_then(|e| self.boolean(e))
            })
            .transpose()?;
        let mut bound_order = Vec::new();
        for item in &order {
            let projected = if let Some(index) = ordinal(&item.expr, visible)? {
                Some(index)
            } else if let ast::Expr::Identifier(name) = &item.expr {
                resolve(&fields[..visible], std::slice::from_ref(&name.value)).ok()
            } else {
                items
                    .iter()
                    .position(|selected| selected.expression == item.expr)
            };
            let index = if let Some(index) = projected {
                index
            } else {
                if distinct {
                    return Err(unsupported("DISTINCT ordering by an unselected expression"));
                }
                let expr = self.expr(&item.expr, &projection_scope, grouping.as_ref())?;
                fields.push(Field::new(item.expr.to_string(), expr.data_type.clone()));
                expressions.push(expr);
                fields.len() - 1
            };
            let (descending, nulls_first) = self.context.query.settings().ordering(
                item.options.asc,
                item.options.nulls_first,
                self.context.query,
            )?;
            bound_order.push(OrderExpr {
                expression: BoundExpr::column(index, fields[index].data_type.clone()),
                descending,
                nulls_first,
            });
        }
        let qualify = if let Some(expression) = &select.qualify {
            let mut qualify_scope = projection_scope.clone();
            for (index, item) in items.iter().enumerate() {
                let name = item.name.to_ascii_lowercase();
                qualify_scope
                    .aliases
                    .entry(name)
                    .or_default()
                    .push(expressions[index].clone());
            }
            Some(self.boolean(self.expr(expression, &qualify_scope, grouping.as_ref())?)?)
        } else {
            None
        };
        if qualify.is_some() && window_scope.is_empty() {
            return Err(Error::Bind(
                "QUALIFY requires at least one window function".into(),
            ));
        }
        let bound_windows = window_scope.take();
        if let Some(grouping) = grouping {
            let outputs = grouping.outputs.into_inner();
            let mut schema: Schema = grouping
                .groups
                .iter()
                .map(|(e, b)| Field::new(e.to_string(), b.data_type.clone()))
                .collect();
            schema.extend(
                outputs
                    .iter()
                    .map(|(e, a)| Field::new(e.to_string(), a.data_type().clone())),
            );
            input = LogicalPlan {
                schema,
                node: PlanNode::Aggregate {
                    input: Box::new(input),
                    aggregation: Aggregation {
                        groups: grouping.groups.into_iter().map(|(_, e)| e).collect(),
                        sets: bound_groups.sets,
                        outputs: outputs.into_iter().map(|(_, a)| a).collect(),
                    },
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
        if !bound_windows.is_empty() {
            let mut schema = input.schema.clone();
            schema.extend(bound_windows.iter().map(|(expression, bound)| {
                Field::new(expression.to_string(), bound.data_type.clone())
            }));
            input = LogicalPlan {
                schema,
                node: PlanNode::Window {
                    input: Box::new(input),
                    expressions: bound_windows
                        .into_iter()
                        .map(|(_, window)| window)
                        .collect(),
                },
            };
        }
        if let Some(predicate) = qualify {
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
