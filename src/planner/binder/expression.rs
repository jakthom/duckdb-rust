use super::*;

impl State<'_, '_> {
    /// Bind/type-check every branch first, then remove statically unreachable
    /// CASE dependencies before nested relational plans are prepared.
    fn prune_case(&self, mut expression: BoundExpr) -> BoundExpr {
        expression.kind = match expression.kind {
            ExprKind::Case(branches, otherwise) => {
                let mut retained = Vec::new();
                let mut otherwise = otherwise;
                for (predicate, value) in branches {
                    let constant = constant_expression(&predicate)
                        .then(|| {
                            self.context.expressions.evaluate(
                                &predicate,
                                &vec![],
                                self.context.query,
                            )
                        })
                        .transpose();
                    match constant {
                        Ok(Some(Value::Boolean(false) | Value::Null)) => (),
                        Ok(Some(Value::Boolean(true))) => {
                            otherwise = Box::new(value);
                            break;
                        }
                        _ => retained.push((predicate, value)),
                    }
                }
                if retained.is_empty() {
                    otherwise.kind
                } else {
                    ExprKind::Case(retained, otherwise)
                }
            }
            kind => kind,
        };
        expression
    }
    pub(super) fn has_aggregate(&self, expr: &ast::Expr) -> bool {
        match expr {
            ast::Expr::Function(f)
                if self
                    .context
                    .functions
                    .aggregate(&f.name.to_string())
                    .is_some() =>
            {
                true
            }
            ast::Expr::Function(f) => {
                function_arguments(f).is_ok_and(|args| args.iter().any(|e| self.has_aggregate(e)))
            }
            ast::Expr::BinaryOp { left, right, .. } => {
                self.has_aggregate(left) || self.has_aggregate(right)
            }
            ast::Expr::UnaryOp { expr, .. }
            | ast::Expr::Nested(expr)
            | ast::Expr::Cast { expr, .. }
            | ast::Expr::IsNull(expr)
            | ast::Expr::IsNotNull(expr) => self.has_aggregate(expr),
            ast::Expr::Case {
                operand,
                conditions,
                else_result,
                ..
            } => {
                operand.as_ref().is_some_and(|e| self.has_aggregate(e))
                    || conditions
                        .iter()
                        .any(|c| self.has_aggregate(&c.condition) || self.has_aggregate(&c.result))
                    || else_result.as_ref().is_some_and(|e| self.has_aggregate(e))
            }
            ast::Expr::Between {
                expr, low, high, ..
            } => self.has_aggregate(expr) || self.has_aggregate(low) || self.has_aggregate(high),
            ast::Expr::InList { expr, list, .. } => {
                self.has_aggregate(expr) || list.iter().any(|e| self.has_aggregate(e))
            }
            ast::Expr::InSubquery { expr, .. } => self.has_aggregate(expr),
            _ => false,
        }
    }

    pub(super) fn expr(
        &self,
        expr: &ast::Expr,
        fields: &[Field],
        grouping: Option<&GroupScope>,
    ) -> Result<BoundExpr> {
        self.context.query.check()?;
        if let Some(grouping) = grouping
            && let Some((index, (_, bound))) = grouping
                .groups
                .iter()
                .enumerate()
                .find(|(_, (e, _))| e == expr)
        {
            return Ok(BoundExpr::column(index, bound.data_type.clone()));
        }
        let recurse = |e: &ast::Expr| self.expr(e, fields, grouping);
        match expr {
            ast::Expr::Value(value) if matches!(&value.value, ast::Value::Placeholder(_)) => {
                let ast::Value::Placeholder(name) = &value.value else {
                    unreachable!()
                };
                let index = name
                    .trim_start_matches(['$', '?'])
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .ok_or_else(|| Error::Bind("use numbered parameters such as $1".into()))?;
                self.context
                    .parameters
                    .get(index)
                    .cloned()
                    .map(BoundExpr::literal)
                    .ok_or_else(|| Error::Bind(format!("missing parameter {name}")))
            }
            ast::Expr::Value(_) => self.sql_literal(expr),
            ast::Expr::TypedString(typed) => {
                let literal =
                    BoundExpr::literal(self.literal(&ast::Expr::Value(typed.value.clone()))?);
                let target = self.data_type(&typed.data_type)?;
                let cast = self.context.casts.bind(
                    &literal.data_type,
                    &target,
                    CastMode::Explicit,
                    self.context.query.types(),
                )?;
                Ok(BoundExpr {
                    data_type: target,
                    kind: ExprKind::Cast(Box::new(literal), cast.into(), false),
                })
            }
            ast::Expr::Identifier(_) | ast::Expr::CompoundIdentifier(_) => {
                let parts = match expr {
                    ast::Expr::Identifier(i) => vec![i.value.clone()],
                    ast::Expr::CompoundIdentifier(i) => i.iter().map(|i| i.value.clone()).collect(),
                    _ => unreachable!(),
                };
                self.column(&parts, fields, grouping)
            }
            ast::Expr::Subquery(query) => {
                self.subquery(query, fields, grouping, subquery::SubqueryForm::Scalar)
            }
            ast::Expr::Exists { subquery, negated } => self.subquery(
                subquery,
                fields,
                grouping,
                subquery::SubqueryForm::Exists { negated: *negated },
            ),
            ast::Expr::InSubquery {
                expr,
                subquery,
                negated,
            } => self.subquery(
                subquery,
                fields,
                grouping,
                subquery::SubqueryForm::In {
                    needle: recurse(expr)?,
                    negated: *negated,
                },
            ),
            ast::Expr::Nested(e) => recurse(e),
            ast::Expr::BinaryOp { left, op, right } => {
                use ast::BinaryOperator as B;
                let overload = match op {
                    B::Plus => Some(Operator::Add),
                    B::Minus => Some(Operator::Subtract),
                    B::Multiply => Some(Operator::Multiply),
                    B::Divide => Some(Operator::Divide),
                    B::DuckIntegerDivide => Some(Operator::IntegerDivide),
                    B::Modulo => Some(Operator::Modulo),
                    B::StringConcat => Some(Operator::Concat),
                    _ => None,
                };
                if let Some(operator) = overload {
                    return self.operator(operator, vec![recurse(left)?, recurse(right)?]);
                }
                let op = match op {
                    B::Eq => BinaryOp::Equal,
                    B::NotEq => BinaryOp::NotEqual,
                    B::Lt => BinaryOp::Less,
                    B::LtEq => BinaryOp::LessEqual,
                    B::Gt => BinaryOp::Greater,
                    B::GtEq => BinaryOp::GreaterEqual,
                    B::And => BinaryOp::And,
                    B::Or => BinaryOp::Or,
                    _ => return Err(unsupported(op)),
                };
                self.binary(op, recurse(left)?, recurse(right)?)
            }
            ast::Expr::UnaryOp {
                op: ast::UnaryOperator::Plus,
                expr,
            } => self.operator(Operator::Plus, vec![recurse(expr)?]),
            ast::Expr::UnaryOp {
                op: ast::UnaryOperator::Minus,
                expr: inner,
            } if matches!(inner.as_ref(), ast::Expr::Value(v) if matches!(&v.value, ast::Value::Number(_, _))) => {
                self.sql_literal(expr)
            }
            ast::Expr::UnaryOp { op, expr } => {
                let inner = recurse(expr)?;
                let (op, inner) = match op {
                    ast::UnaryOperator::Not => (UnaryOp::Not, self.boolean(inner)?),
                    ast::UnaryOperator::Minus => {
                        return self.operator(Operator::Negate, vec![inner]);
                    }
                    _ => return Err(unsupported(op)),
                };
                Ok(BoundExpr {
                    data_type: inner.data_type.clone(),
                    kind: ExprKind::Unary(op, Box::new(inner)),
                })
            }
            ast::Expr::IsNull(e) | ast::Expr::IsNotNull(e) => Ok(BoundExpr {
                data_type: DataType::Boolean,
                kind: ExprKind::Unary(
                    if matches!(expr, ast::Expr::IsNull(_)) {
                        UnaryOp::IsNull
                    } else {
                        UnaryOp::IsNotNull
                    },
                    Box::new(recurse(e)?),
                ),
            }),
            ast::Expr::Cast {
                kind,
                expr,
                data_type: target,
                array: false,
                format: None,
            } => {
                let inner = recurse(expr)?;
                let target = self.data_type(target)?;
                let cast = self.context.casts.bind(
                    &inner.data_type,
                    &target,
                    CastMode::Explicit,
                    self.context.query.types(),
                )?;
                Ok(BoundExpr {
                    data_type: target,
                    kind: ExprKind::Cast(
                        Box::new(inner),
                        cast.into(),
                        matches!(kind, ast::CastKind::TryCast | ast::CastKind::SafeCast),
                    ),
                })
            }
            ast::Expr::Function(function) => {
                if function.over.is_some()
                    || !function.within_group.is_empty()
                    || function.null_treatment.is_some()
                {
                    return Err(unsupported("window or ordered function"));
                }
                let name = function.name.to_string();
                if let Some(aggregate) = self.context.functions.aggregate(&name) {
                    let grouping = grouping.ok_or_else(|| {
                        Error::Bind(format!("aggregate {name} is not allowed here"))
                    })?;
                    if let Some((index, (_, aggregate))) = grouping
                        .aggregates
                        .borrow()
                        .iter()
                        .enumerate()
                        .find(|(_, (e, _))| e == expr)
                    {
                        return Ok(BoundExpr::column(
                            grouping.groups.len() + index,
                            aggregate.data_type.clone(),
                        ));
                    }
                    let arguments = function_arguments(function)?
                        .iter()
                        .map(|e| self.expr(e, fields, None))
                        .collect::<Result<Vec<_>>>()?;
                    let data_type = aggregate.return_type(
                        &arguments
                            .iter()
                            .map(|e| e.data_type.clone())
                            .collect::<Vec<_>>(),
                        self.context.query.types(),
                    )?;
                    let distinct = matches!(&function.args, ast::FunctionArguments::List(a) if a.duplicate_treatment == Some(ast::DuplicateTreatment::Distinct));
                    if distinct && arguments.is_empty() {
                        return Err(Error::Bind(
                            "DISTINCT aggregate requires an argument".into(),
                        ));
                    }
                    let filter = function
                        .filter
                        .as_ref()
                        .map(|e| self.expr(e, fields, None).and_then(|e| self.boolean(e)))
                        .transpose()?;
                    let (mut local, mut outer) = (false, false);
                    for expression in arguments.iter().chain(filter.iter()) {
                        aggregate_references(expression, &mut local, &mut outer);
                    }
                    if outer && !local {
                        return Err(unsupported("aggregate binding to an outer query scope"));
                    }
                    let index = grouping.groups.len() + grouping.aggregates.borrow().len();
                    grouping.aggregates.borrow_mut().push((
                        expr.clone(),
                        AggregateExpr {
                            function: aggregate,
                            arguments,
                            distinct,
                            filter,
                            data_type: data_type.clone(),
                        },
                    ));
                    Ok(BoundExpr::column(index, data_type))
                } else {
                    if function.filter.is_some() {
                        return Err(Error::Bind("FILTER requires an aggregate".into()));
                    }
                    let function_impl = self.context.functions.scalar(&name)?;
                    let mut arguments = function_arguments(function)?
                        .iter()
                        .map(&recurse)
                        .collect::<Result<Vec<_>>>()?;
                    let data_type = function_impl.return_type(
                        &arguments
                            .iter()
                            .map(|e| e.data_type.clone())
                            .collect::<Vec<_>>(),
                        self.context.query.types(),
                    )?;
                    if name.eq_ignore_ascii_case("coalesce") || name.eq_ignore_ascii_case("nullif")
                    {
                        arguments = arguments
                            .into_iter()
                            .map(|e| {
                                e.cast(
                                    data_type.clone(),
                                    CastMode::Implicit,
                                    self.context.casts,
                                    self.context.query.types(),
                                )
                            })
                            .collect::<Result<Vec<_>>>()?;
                    }
                    Ok(BoundExpr {
                        data_type,
                        kind: ExprKind::Scalar(function_impl, arguments),
                    })
                }
            }
            ast::Expr::Case {
                operand,
                conditions,
                else_result,
                ..
            } => {
                let mut branches = Vec::new();
                let mut data_type = DataType::Null;
                for condition in conditions {
                    let predicate = if let Some(operand) = operand {
                        self.binary(
                            BinaryOp::Equal,
                            recurse(operand)?,
                            recurse(&condition.condition)?,
                        )?
                    } else {
                        self.boolean(recurse(&condition.condition)?)?
                    };
                    let value = recurse(&condition.result)?;
                    data_type = self
                        .context
                        .query
                        .types()
                        .common_type(&data_type, &value.data_type)?;
                    branches.push((predicate, value));
                }
                let otherwise = else_result
                    .as_ref()
                    .map(|e| recurse(e))
                    .transpose()?
                    .unwrap_or_else(|| BoundExpr::literal(Value::Null));
                data_type = self
                    .context
                    .query
                    .types()
                    .common_type(&data_type, &otherwise.data_type)?;
                Ok(self.prune_case(BoundExpr {
                    kind: ExprKind::Case(
                        branches
                            .into_iter()
                            .map(|(p, v)| {
                                Ok((
                                    p,
                                    v.cast(
                                        data_type.clone(),
                                        CastMode::Implicit,
                                        self.context.casts,
                                        self.context.query.types(),
                                    )?,
                                ))
                            })
                            .collect::<Result<Vec<_>>>()?,
                        Box::new(otherwise.cast(
                            data_type.clone(),
                            CastMode::Implicit,
                            self.context.casts,
                            self.context.query.types(),
                        )?),
                    ),
                    data_type,
                }))
            }
            ast::Expr::Between {
                expr,
                negated,
                low,
                high,
            } => {
                let bound = self.binary(
                    BinaryOp::And,
                    self.binary(BinaryOp::GreaterEqual, recurse(expr)?, recurse(low)?)?,
                    self.binary(BinaryOp::LessEqual, recurse(expr)?, recurse(high)?)?,
                )?;
                Ok(if *negated {
                    BoundExpr {
                        data_type: DataType::Boolean,
                        kind: ExprKind::Unary(UnaryOp::Not, Box::new(bound)),
                    }
                } else {
                    bound
                })
            }
            ast::Expr::InList {
                expr,
                list,
                negated,
            } => {
                let value = recurse(expr)?;
                let list = list.iter().map(recurse).collect::<Result<Vec<_>>>()?;
                let data_type = list.iter().try_fold(value.data_type.clone(), |t, e| {
                    self.context.query.types().common_type(&t, &e.data_type)
                })?;
                Ok(BoundExpr {
                    data_type: DataType::Boolean,
                    kind: ExprKind::InList(
                        Box::new(value.cast(
                            data_type.clone(),
                            CastMode::Implicit,
                            self.context.casts,
                            self.context.query.types(),
                        )?),
                        list.into_iter()
                            .map(|e| {
                                e.cast(
                                    data_type.clone(),
                                    CastMode::Implicit,
                                    self.context.casts,
                                    self.context.query.types(),
                                )
                            })
                            .collect::<Result<Vec<_>>>()?,
                        *negated,
                        self.context.query.types().bind(&data_type)?.into(),
                    ),
                })
            }
            ast::Expr::Like {
                negated,
                expr,
                pattern,
                escape_char: None,
                any: false,
            } => self.operator(
                if *negated {
                    Operator::NotLike
                } else {
                    Operator::Like
                },
                vec![recurse(expr)?, recurse(pattern)?],
            ),
            _ => Err(unsupported(expr)),
        }
    }
}

fn aggregate_references(expression: &BoundExpr, local: &mut bool, outer: &mut bool) {
    match expression.kind {
        ExprKind::Column(_) | ExprKind::Subquery(_) => *local = true,
        ExprKind::OuterColumn { .. } => *outer = true,
        _ => expression.visit_children(&mut |child| aggregate_references(child, local, outer)),
    }
}
