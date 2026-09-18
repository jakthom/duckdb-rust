use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn from(&mut self, from: &ast::TableWithJoins) -> Result<Relation> {
        let mut left = self.factor(&from.relation)?;
        for joined in &from.joins {
            let right = self.factor(&joined.relation)?;
            use ast::JoinOperator as J;
            let (kind, constraint) = match &joined.join_operator {
                J::Join(c) | J::Inner(c) | J::CrossJoin(c) => (JoinKind::Inner, c),
                J::Left(c) | J::LeftOuter(c) => (JoinKind::Left, c),
                J::Right(c) | J::RightOuter(c) => (JoinKind::Right, c),
                J::FullOuter(c) => (JoinKind::Full, c),
                J::Semi(c) | J::LeftSemi(c) => (JoinKind::Semi, c),
                J::Anti(c) | J::LeftAnti(c) => (JoinKind::Anti, c),
                _ => return Err(unsupported("join kind")),
            };
            left = self.join_relation(left, right, kind, constraint)?;
        }
        Ok(left)
    }

    fn join_relation(
        &self,
        left: Relation,
        right: Relation,
        kind: JoinKind,
        constraint: &ast::JoinConstraint,
    ) -> Result<Relation> {
        let offset = left.plan.schema.len();
        let mut scope = left.scope.combine(&right.scope);
        let names = match constraint {
            ast::JoinConstraint::Using(names) => names
                .iter()
                .map(|name| {
                    if name.0.len() != 1 {
                        return Err(Error::Bind(
                            "USING requires an unqualified column name".into(),
                        ));
                    }
                    name.0[0]
                        .as_ident()
                        .map(|name| name.value.clone())
                        .ok_or_else(|| unsupported(name))
                })
                .collect::<Result<Vec<_>>>()?,
            ast::JoinConstraint::Natural => {
                let names: Vec<_> = left
                    .scope
                    .visible
                    .iter()
                    .filter_map(|&index| {
                        let name = &left.scope[index].name;
                        right
                            .scope
                            .visible
                            .iter()
                            .any(|&r| right.scope[r].name.eq_ignore_ascii_case(name))
                            .then(|| name.clone())
                    })
                    .collect();
                if names.is_empty() {
                    return Err(Error::Bind("NATURAL join has no columns in common".into()));
                }
                names
            }
            _ => Vec::new(),
        };
        let mut condition = match constraint {
            ast::JoinConstraint::On(expr) => self.boolean(self.expr(expr, &scope, None)?)?,
            _ => BoundExpr::literal(Value::Boolean(true)),
        };
        let mut seen = HashSet::new();
        let mut merged = Vec::new();
        for name in names {
            if !seen.insert(name.to_ascii_lowercase()) {
                continue;
            }
            let l = left.scope.resolve(std::slice::from_ref(&name))?;
            let r = right.scope.resolve(std::slice::from_ref(&name))? + offset;
            let left_key = BoundExpr::column(l, scope[l].data_type.clone());
            let right_key = BoundExpr::column(r, scope[r].data_type.clone());
            let equality = self.binary(BinaryOp::Equal, left_key.clone(), right_key.clone())?;
            condition = if matches!(condition.kind, ExprKind::Literal(Value::Boolean(true))) {
                equality
            } else {
                self.binary(BinaryOp::And, condition, equality)?
            };
            let key = match kind {
                JoinKind::Right => r,
                JoinKind::Full => {
                    let common = self
                        .context
                        .query
                        .types()
                        .common_type(&left_key.data_type, &right_key.data_type)?;
                    let left_key = left_key.cast(
                        common.clone(),
                        CastMode::Implicit,
                        self.context.casts,
                        self.context.query.types(),
                    )?;
                    let right_key = right_key.cast(
                        common.clone(),
                        CastMode::Implicit,
                        self.context.casts,
                        self.context.query.types(),
                    )?;
                    let present = BoundExpr {
                        data_type: DataType::Boolean,
                        kind: ExprKind::Unary(UnaryOp::IsNotNull, Box::new(left_key.clone())),
                    };
                    merged.push(BoundExpr {
                        data_type: common.clone(),
                        kind: ExprKind::Case(vec![(present, left_key)], Box::new(right_key)),
                    });
                    scope.append(Field::new(&scope[l].name, common))
                }
                _ => l,
            };
            scope.merge_key(l, r, key);
        }
        if matches!(kind, JoinKind::Semi | JoinKind::Anti) {
            scope.truncate(offset);
        }
        let mut plan = join(left.plan, right.plan, kind, condition);
        if !merged.is_empty() {
            let mut expressions: Vec<_> = plan
                .schema
                .iter()
                .enumerate()
                .map(|(index, field)| BoundExpr::column(index, field.data_type.clone()))
                .collect();
            expressions.extend(merged);
            plan = LogicalPlan {
                schema: scope.to_vec(),
                node: PlanNode::Projection {
                    input: Box::new(plan),
                    expressions,
                },
            };
        }
        Ok(Relation { plan, scope })
    }

    pub(super) fn factor(&mut self, factor: &ast::TableFactor) -> Result<Relation> {
        if let ast::TableFactor::NestedJoin {
            table_with_joins,
            alias: table_alias,
        } = factor
        {
            let relation = self.from(table_with_joins)?;
            return if let Some(table_alias) = table_alias {
                let mut plan = relation.project_visible();
                alias(&mut plan, table_alias)?;
                Ok(plan.into())
            } else {
                Ok(relation)
            };
        }
        let mut namespace = None;
        let (mut plan, table_alias) = match factor {
            ast::TableFactor::Table {
                name,
                args,
                alias,
                version: None,
                with_ordinality: false,
                sample: None,
                partitions,
                ..
            } if partitions.is_empty() => {
                if let Some(args) = args {
                    let name = name.to_string().to_ascii_lowercase();
                    if name != "range" && name != "generate_series" {
                        return Err(unsupported(format!("table function {name}")));
                    }
                    let args = args
                        .args
                        .iter()
                        .map(function_arg)
                        .collect::<Result<Vec<_>>>()?;
                    let args = args
                        .iter()
                        .map(|e| {
                            self.literal(e)?.as_i128().and_then(|v| {
                                i64::try_from(v).map_err(|_| {
                                    Error::Conversion("range argument exceeds BIGINT".into())
                                })
                            })
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let (start, mut end, step) = match args.as_slice() {
                        [end] => (0, *end, 1),
                        [start, end] => (*start, *end, 1),
                        [start, end, step] => (*start, *end, *step),
                        _ => {
                            return Err(Error::Bind(
                                "range expects one to three integer arguments".into(),
                            ));
                        }
                    };
                    if step == 0 {
                        return Err(Error::Bind("range step cannot be zero".into()));
                    }
                    if name == "generate_series" {
                        end = end
                            .checked_add(step.signum())
                            .ok_or_else(|| Error::Conversion("range bound overflow".into()))?;
                    }
                    (
                        LogicalPlan {
                            schema: vec![Field {
                                qualifier: Some(name.clone()),
                                name,
                                data_type: DataType::BigInt,
                            }],
                            node: PlanNode::Range { start, end, step },
                        },
                        alias,
                    )
                } else {
                    let unresolved = self.unresolved_table_name(name)?;
                    if unresolved.is_unqualified() && self.ctes.contains_key(unresolved.table()) {
                        let cte = &self.ctes[unresolved.table()];
                        (
                            subquery::rebase_cte(
                                cte.plan.clone(),
                                self.outer.len() - cte.depth,
                                0,
                            )?,
                            alias,
                        )
                    } else {
                        let resolved =
                            self.resolve_existing_table(name, false)?.ok_or_else(|| {
                                Error::Internal("required table resolution is absent".into())
                            })?;
                        let (binding, definition) = resolved.into_parts();
                        namespace = Some(definition.name.clone());
                        (
                            LogicalPlan {
                                schema: schema(&definition),
                                node: PlanNode::Scan(binding),
                            },
                            alias,
                        )
                    }
                }
            }
            ast::TableFactor::Derived {
                lateral: false,
                subquery,
                alias,
                sample: None,
            } => (self.query(subquery)?, alias),
            _ => return Err(unsupported(factor)),
        };
        if let Some(table_alias) = table_alias {
            alias(&mut plan, table_alias)?;
        }
        let mut relation: Relation = plan.into();
        if table_alias.is_none()
            && let Some(table) = namespace
        {
            relation.scope.qualify_table(&table);
        }
        Ok(relation)
    }

    pub(super) fn order(
        &self,
        order: &ast::OrderByExpr,
        fields: &Scope,
        grouping: Option<&GroupScope>,
    ) -> Result<OrderExpr> {
        let expression = if let Some(index) = ordinal(&order.expr, fields.len())? {
            BoundExpr::column(index, fields[index].data_type.clone())
        } else {
            self.expr(&order.expr, fields, grouping)?
        };
        let (descending, nulls_first) = self.context.query.settings().ordering(
            order.options.asc,
            order.options.nulls_first,
            self.context.query,
        )?;
        Ok(OrderExpr {
            expression,
            descending,
            nulls_first,
        })
    }
}
