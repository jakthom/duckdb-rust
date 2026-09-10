use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
        let mut aggregate = false;
        let _ = window::visit_expression(expr, &mut |expr| {
            aggregate |= self.is_aggregate(expr);
            Ok(!aggregate)
        });
        aggregate
    }

    pub(super) fn expr(
        &self,
        expr: &ast::Expr,
        fields: &Scope,
        grouping: Option<&GroupScope>,
    ) -> Result<BoundExpr> {
        self.context.query.check()?;
        if matches!(expr, ast::Expr::Function(function) if function.over.is_some())
            && let Some(windows) = &fields.windows
        {
            return windows.bind(self, expr, fields, grouping);
        }
        if let ast::Expr::Identifier(name) = expr
            && let Some(aliases) = fields.aliases.get(&name.value.to_ascii_lowercase())
            && fields
                .resolve_optional(std::slice::from_ref(&name.value))?
                .is_none()
        {
            let [bound] = aliases.as_slice() else {
                return Err(Error::Bind(format!("ambiguous SELECT alias {name}")));
            };
            if window::has_effects(bound) {
                return Err(Error::Bind(
                    "referenced alias expression has side effects".into(),
                ));
            }
            return Ok(bound.clone());
        }
        if let Some(grouping) = grouping
            && let Some(index) = grouping.index(expr, fields)
        {
            return Ok(BoundExpr::column(
                index,
                grouping.groups[index].1.data_type.clone(),
            ));
        }
        let recurse = |e: &ast::Expr| self.expr(e, fields, grouping);
        match expr {
            ast::Expr::Value(value) if matches!(&value.value, ast::Value::Placeholder(_)) => {
                if !self.parameters_allowed {
                    return Err(unsupported("SET statements cannot have parameters"));
                }
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
            ast::Expr::Interval(interval)
                if interval.leading_field.is_none() && interval.last_field.is_none() =>
            {
                let literal = recurse(&interval.value)?;
                let cast = self.context.casts.bind(
                    &literal.data_type,
                    &DataType::Interval,
                    CastMode::Explicit,
                    self.context.query.types(),
                )?;
                Ok(BoundExpr {
                    data_type: DataType::Interval,
                    kind: ExprKind::Cast(Box::new(literal), cast.into(), false),
                })
            }
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
            ast::Expr::CompoundFieldAccess { root, access_chain } => {
                let mut value = recurse(root)?;
                for access in access_chain {
                    let key = match access {
                        ast::AccessExpr::Subscript(ast::Subscript::Index { index }) => {
                            recurse(index)?
                        }
                        ast::AccessExpr::Dot(ast::Expr::Identifier(name)) => {
                            BoundExpr::literal(Value::Varchar(name.value.clone()))
                        }
                        _ => return Err(unsupported("nested slice or accessor")),
                    };
                    value = self.nested_access(value, key)?;
                }
                Ok(value)
            }
            ast::Expr::Array(array) => self.nested_constructor(
                array.elem.iter().map(&recurse).collect::<Result<_>>()?,
                None,
            ),
            ast::Expr::Dictionary(fields) => self.nested_constructor(
                fields
                    .iter()
                    .map(|field| recurse(&field.value))
                    .collect::<Result<_>>()?,
                Some(fields.iter().map(|field| field.key.value.clone()).collect()),
            ),
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
                    return Err(Error::Bind(
                        "window or ordered function is not allowed in this clause".into(),
                    ));
                }
                let name = function.name.to_string();
                if name.eq_ignore_ascii_case("struct_pack")
                    || name.eq_ignore_ascii_case("union_value")
                {
                    let ast::FunctionArguments::List(list) = &function.args else {
                        return Err(Error::Bind("nested constructor requires arguments".into()));
                    };
                    if list.duplicate_treatment.is_some()
                        || !list.clauses.is_empty()
                        || function.filter.is_some()
                    {
                        return Err(Error::Bind("invalid nested constructor modifiers".into()));
                    }
                    let mut names = Vec::new();
                    let mut arguments = Vec::new();
                    for argument in &list.args {
                        let ast::FunctionArg::Named {
                            name,
                            arg: ast::FunctionArgExpr::Expr(value),
                            ..
                        } = argument
                        else {
                            return Err(Error::Bind(
                                "nested constructor requires named arguments".into(),
                            ));
                        };
                        names.push(name.value.clone());
                        arguments.push(recurse(value)?);
                    }
                    if name.eq_ignore_ascii_case("union_value") {
                        if arguments.len() != 1 {
                            return Err(Error::Bind(
                                "union_value requires one named argument".into(),
                            ));
                        }
                        let data_type = crate::common::NestedType::Union(vec![(
                            names.remove(0),
                            arguments[0].data_type.clone(),
                        )])
                        .data_type();
                        self.context.query.types().bind(&data_type)?;
                        return Ok(BoundExpr {
                            data_type: data_type.clone(),
                            kind: ExprKind::Scalar(
                                std::sync::Arc::new(crate::function::nested::Constructor(
                                    data_type,
                                )),
                                arguments,
                            ),
                        });
                    }
                    return self.nested_constructor(arguments, Some(names));
                }
                if name.eq_ignore_ascii_case("grouping") || name.eq_ignore_ascii_case("grouping_id")
                {
                    return self.grouping_function(expr, function, fields, grouping);
                }
                if let Some(aggregate) = self.context.functions.aggregate(&name) {
                    let grouping = grouping.ok_or_else(|| {
                        Error::Bind(format!("aggregate {name} is not allowed here"))
                    })?;
                    if let Some((index, (_, aggregate))) = grouping
                        .outputs
                        .borrow()
                        .iter()
                        .enumerate()
                        .find(|(_, (e, _))| e == expr)
                    {
                        return Ok(BoundExpr::column(
                            grouping.groups.len() + index,
                            aggregate.data_type().clone(),
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
                    let index = grouping.groups.len() + grouping.outputs.borrow().len();
                    grouping.outputs.borrow_mut().push((
                        expr.clone(),
                        AggregateOutput::Function(AggregateExpr {
                            function: aggregate,
                            arguments,
                            distinct,
                            filter,
                            data_type: data_type.clone(),
                        }),
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
                    let function_impl = function_impl
                        .bind(
                            &FunctionArguments {
                                arguments: &arguments,
                                context: self.context,
                            },
                            self.context.query,
                        )?
                        .unwrap_or(function_impl);
                    let argument_types = function_impl.argument_types(
                        &arguments
                            .iter()
                            .map(|e| e.data_type.clone())
                            .collect::<Vec<_>>(),
                        self.context.query.types(),
                    )?;
                    if argument_types.len() != arguments.len() {
                        return Err(Error::Internal("scalar argument type count".into()));
                    }
                    arguments = arguments
                        .into_iter()
                        .zip(argument_types.iter())
                        .map(|(e, target)| {
                            e.cast(
                                target.clone(),
                                CastMode::Implicit,
                                self.context.casts,
                                self.context.query.types(),
                            )
                        })
                        .collect::<Result<Vec<_>>>()?;
                    let data_type =
                        function_impl.return_type(&argument_types, self.context.query.types())?;
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

struct FunctionArguments<'a, 'b> {
    arguments: &'a [BoundExpr],
    context: &'a BindContext<'b>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl crate::function::ScalarBindArguments for FunctionArguments<'_, '_> {
    fn len(&self) -> usize {
        self.arguments.len()
    }
    fn data_type(&self, index: usize) -> Result<DataType> {
        self.arguments
            .get(index)
            .map(|a| a.data_type.clone())
            .ok_or_else(|| Error::Bind("function argument outside signature".into()))
    }
    fn constant(&self, index: usize) -> Result<Value> {
        let expression = self
            .arguments
            .get(index)
            .ok_or_else(|| Error::Bind("function argument outside signature".into()))?;
        if !constant_expression(expression) {
            return Err(Error::Bind(
                "function requires a constant argument without effects".into(),
            ));
        }
        let value =
            self.context
                .expressions
                .evaluate(expression, &Vec::new(), self.context.query)?;
        self.context
            .query
            .types()
            .bind(&expression.data_type)?
            .validate(&value, self.context.query)
            .map_err(|error| match error {
                Error::Conversion(_) => {
                    Error::Internal("constant evaluator returned an invalid logical value".into())
                }
                other => other,
            })?;
        Ok(value)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn aggregate_references(expression: &BoundExpr, local: &mut bool, outer: &mut bool) {
    match expression.kind {
        ExprKind::Column(_) | ExprKind::Subquery(_) => *local = true,
        ExprKind::OuterColumn { .. } => *outer = true,
        _ => expression.visit_children(&mut |child| aggregate_references(child, local, outer)),
    }
}
