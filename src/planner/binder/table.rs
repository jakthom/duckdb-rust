use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn from(&mut self, from: &ast::TableWithJoins) -> Result<LogicalPlan> {
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
            let mut fields = left.schema.clone();
            fields.extend(right.schema.clone());
            let condition = match constraint {
                ast::JoinConstraint::None => BoundExpr::literal(Value::Boolean(true)),
                ast::JoinConstraint::On(e) => self.boolean(self.expr(e, &fields, None)?)?,
                _ => return Err(unsupported("NATURAL or USING join")),
            };
            left = join(left, right, kind, condition);
        }
        Ok(left)
    }

    pub(super) fn factor(&mut self, factor: &ast::TableFactor) -> Result<LogicalPlan> {
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
                    let table = table_name(name)?;
                    if name.0.len() == 1 && self.ctes.contains_key(&table.name.to_ascii_lowercase())
                    {
                        let cte = &self.ctes[&table.name.to_ascii_lowercase()];
                        (
                            subquery::rebase_cte(
                                cte.plan.clone(),
                                self.outer.len() - cte.depth,
                                0,
                            )?,
                            alias,
                        )
                    } else {
                        (
                            LogicalPlan {
                                schema: schema(&self.context.catalog.table(&table)?),
                                node: PlanNode::Scan(table),
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
        Ok(plan)
    }

    pub(super) fn order(
        &self,
        order: &ast::OrderByExpr,
        fields: &[Field],
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
