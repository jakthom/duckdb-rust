use super::*;

pub(super) enum IntervalLowering {
    Cast,
    Unit {
        function: &'static str,
        target: DataType,
    },
}

pub(super) fn interval_lowering(interval: &ast::Interval) -> Result<IntervalLowering> {
    if interval.last_field.is_some()
        || interval.leading_precision.is_some()
        || interval.fractional_seconds_precision.is_some()
    {
        return Err(Error::Parse(
            "INTERVAL precision and TO qualifiers are not supported".into(),
        ));
    }
    let Some(field) = &interval.leading_field else {
        return Ok(IntervalLowering::Cast);
    };
    // Development's parser uses DOUBLE, truncation, then the unit's declared
    // integer width. This description is shared by ordinary and retained
    // lowering so both select the same casts and scalar function graph.
    let (function, target) = match field.to_string().to_ascii_lowercase().as_str() {
        "year" | "years" => ("to_years", DataType::Integer),
        "month" | "months" => ("to_months", DataType::Integer),
        "day" | "days" => ("to_days", DataType::Integer),
        "week" | "weeks" => ("to_weeks", DataType::Integer),
        "quarter" | "quarters" => ("to_quarters", DataType::Integer),
        "decade" | "decades" => ("to_decades", DataType::Integer),
        "century" | "centuries" => ("to_centuries", DataType::Integer),
        "millennium" | "millennia" => ("to_millennia", DataType::Integer),
        "hour" | "hours" => ("to_hours", DataType::BigInt),
        "minute" | "minutes" => ("to_minutes", DataType::BigInt),
        "microsecond" | "microseconds" => ("to_microseconds", DataType::BigInt),
        "millisecond" | "milliseconds" => ("to_milliseconds", DataType::Double),
        "second" | "seconds" => ("to_seconds", DataType::Double),
        _ => return Err(Error::Unsupported(format!("INTERVAL unit {field}"))),
    };
    Ok(IntervalLowering::Unit { function, target })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    /// Bind an already parsed scalar call through the selected catalog and
    /// retained argument/cast contracts. Syntax sugar shares ordinary calls.
    pub(super) fn scalar_call(&self, name: &str, arguments: Vec<BoundExpr>) -> Result<BoundExpr> {
        let function_impl = self.context.functions.scalar(name)?;
        self.scalar_call_selected(function_impl, arguments)
    }

    pub(super) fn scalar_call_selected(
        &self,
        function_impl: std::sync::Arc<dyn crate::function::ScalarFunction>,
        arguments: Vec<BoundExpr>,
    ) -> Result<BoundExpr> {
        self.scalar_call_selected_named(function_impl, arguments, None, None)
    }
    pub(super) fn scalar_call_selected_named(
        &self,
        function_impl: std::sync::Arc<dyn crate::function::ScalarFunction>,
        arguments: Vec<BoundExpr>,
        names: Option<&[Option<String>]>,
        aliases: Option<&[Option<String>]>,
    ) -> Result<BoundExpr> {
        self.scalar_call_with_metadata(function_impl, arguments, names, aliases, None)
    }

    fn scalar_slice_call(
        &self,
        name: &str,
        arguments: Vec<BoundExpr>,
        omitted_bounds: &[bool],
    ) -> Result<BoundExpr> {
        let function_impl = self.context.functions.scalar(name)?;
        self.scalar_call_with_metadata(function_impl, arguments, None, None, Some(omitted_bounds))
    }

    fn scalar_call_with_metadata(
        &self,
        function_impl: std::sync::Arc<dyn crate::function::ScalarFunction>,
        arguments: Vec<BoundExpr>,
        names: Option<&[Option<String>]>,
        aliases: Option<&[Option<String>]>,
        omitted_slice_bounds: Option<&[bool]>,
    ) -> Result<BoundExpr> {
        self.context.query.check()?;
        if names.is_some_and(|names| names.len() != arguments.len())
            || aliases.is_some_and(|aliases| aliases.len() != arguments.len())
            || omitted_slice_bounds.is_some_and(|bounds| bounds.len() != arguments.len())
        {
            return Err(Error::Internal("scalar argument metadata count".into()));
        }
        if names.is_some_and(|names| names.iter().any(Option::is_some))
            && !function_impl.accepts_named_arguments()
        {
            return Err(Error::Bind(format!(
                "{} does not accept named arguments",
                function_impl.name()
            )));
        }
        if let Some(expansion) = function_impl.expansion(
            &FunctionArguments {
                arguments: &arguments,
                context: self.context,
                names,
                aliases,
                omitted_slice_bounds,
            },
            self.context.query,
        )? {
            expansion.validate(arguments.len(), self.context.query)?;
            let effects = function_impl.effects();
            if effects.volatile || effects.external_access {
                return Err(Error::Unsupported(
                    "effectful scalar expansion metadata".into(),
                ));
            }
            super::scalar_expansion::validate_bound_expansion(
                &expansion,
                &arguments,
                self.context.query,
            )?;
            return self.expand_scalar(&expansion, expansion.nodes.len() - 1, &arguments);
        }
        let function_impl = function_impl
            .bind(
                &FunctionArguments {
                    arguments: &arguments,
                    context: self.context,
                    names,
                    aliases,
                    omitted_slice_bounds,
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
        let arguments = arguments
            .into_iter()
            .zip(argument_types.iter())
            .enumerate()
            .map(|(index, (e, target))| {
                let selected = function_impl.argument_cast_mode(index);
                let mode = if function_impl.argument_literal_coercion(index) {
                    super::coercion::scalar_argument_cast_mode(&e, target, selected)
                } else {
                    selected
                };
                e.cast(
                    target.clone(),
                    mode,
                    self.context.casts,
                    self.context.query.types(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let data_type = function_impl.return_type(&argument_types, self.context.query.types())?;
        Ok(BoundExpr {
            data_type,
            kind: ExprKind::Scalar(function_impl, arguments),
        })
    }
    fn expand_scalar(
        &self,
        expansion: &crate::function::ScalarExpansion,
        index: usize,
        arguments: &[BoundExpr],
    ) -> Result<BoundExpr> {
        use crate::function::ScalarExpansionNode;
        self.context.query.check()?;
        // Complete validation has already bounded every node, argument,
        // occurrence and recursive depth before any CASE pruning can execute.
        let child = |index| self.expand_scalar(expansion, index, arguments);
        match &expansion.nodes[index] {
            ScalarExpansionNode::Argument(index) => Ok(arguments[*index].clone()),
            ScalarExpansionNode::Null => Ok(BoundExpr::literal(Value::Null)),
            ScalarExpansionNode::Equal { left, right } => {
                self.binary(BinaryOp::Equal, child(*left)?, child(*right)?)
            }
            ScalarExpansionNode::Case {
                condition,
                then_value,
                otherwise,
            } => {
                let predicate = self.boolean(child(*condition)?)?;
                let value = child(*then_value)?;
                let otherwise = child(*otherwise)?;
                self.bound_case(vec![(predicate, value)], otherwise)
            }
        }
    }
    pub(super) fn bound_case(
        &self,
        branches: Vec<(BoundExpr, BoundExpr)>,
        otherwise: BoundExpr,
    ) -> Result<BoundExpr> {
        // Inference is ELSE-first; child binding/expansion remains source-order.
        let data_type = self.ordered_combination_type(
            std::iter::once(&otherwise).chain(branches.iter().map(|(_, value)| value)),
            super::coercion::CombinationSequence::Case,
        )?;
        Ok(self.prune_case(BoundExpr {
            kind: ExprKind::Case(
                branches
                    .into_iter()
                    .map(|(predicate, value)| {
                        Ok((predicate, self.combination_cast(value, &data_type)?))
                    })
                    .collect::<Result<Vec<_>>>()?,
                Box::new(self.combination_cast(otherwise, &data_type)?),
            ),
            data_type,
        }))
    }

    pub(super) fn bound_between(
        &self,
        input: BoundExpr,
        lower: BoundExpr,
        upper: BoundExpr,
    ) -> Result<BoundExpr> {
        let eager_bounds = window::has_effects(&input) || has_runtime_bound_input(&input);
        let input_literal = super::coercion::string_literal(&input);
        let lower_literal = super::coercion::string_literal(&lower);
        let upper_literal = super::coercion::string_literal(&upper);
        let mut data_type = self.comparison_type(
            &input.data_type,
            input_literal,
            &lower.data_type,
            lower_literal,
            false,
        )?;
        data_type = self.comparison_type(
            &data_type,
            input_literal && lower_literal,
            &upper.data_type,
            upper_literal,
            false,
        )?;
        Ok(BoundExpr {
            data_type: DataType::Boolean,
            kind: ExprKind::Between(
                Box::new(self.combination_cast(input, &data_type)?),
                Box::new(self.combination_cast(lower, &data_type)?),
                Box::new(self.combination_cast(upper, &data_type)?),
                self.context.query.types().bind(&data_type)?.into(),
                eager_bounds,
            ),
        })
    }
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
                // Keep expression provenance through outer overload binding.
                // Removing dead dependencies must not turn CASE into a SQL
                // literal merely because its remaining value is a literal.
                ExprKind::Case(retained, otherwise)
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
        let unqualified_name = match expr {
            ast::Expr::Identifier(name) => Some(&name.value),
            ast::Expr::Function(function) if bare_current_timestamp(expr) => function
                .name
                .0
                .first()
                .and_then(ast::ObjectNamePart::as_ident)
                .map(|name| &name.value),
            _ => None,
        };
        if let Some(name) = unqualified_name
            && let Some(aliases) = fields.aliases.get(&name.to_ascii_lowercase())
            && fields
                .resolve_optional(std::slice::from_ref(name))?
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
        if let Some(name) = unqualified_name.filter(|_| bare_current_timestamp(expr)) {
            let parts = std::slice::from_ref(name);
            if fields.resolve_optional(parts)?.is_some() {
                return self.column(parts, fields, grouping);
            }
            if let Some(columns) = fields.relation_columns_optional(name)? {
                let names = columns
                    .clone()
                    .map(|index| fields[index].name.clone())
                    .collect();
                let arguments = columns
                    .map(|index| self.resolved_column(index, &fields[index].name, fields, grouping))
                    .collect::<Result<Vec<_>>>()?;
                return self.nested_constructor(arguments, Some(names));
            }
            return self.scalar_call("get_current_timestamp", Vec::new());
        }
        let recurse = |e: &ast::Expr| self.expr(e, fields, grouping);
        match expr {
            ast::Expr::Wildcard(_) | ast::Expr::QualifiedWildcard(_, _) => Err(Error::Bind(
                "STAR expression is only allowed as the root element of an expression. Use COLUMNS(*) instead."
                    .into(),
            )),
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
                    .map(BoundExpr::parameter)
                    .ok_or_else(|| Error::Bind(format!("missing parameter {name}")))
            }
            ast::Expr::Value(_) => self.sql_literal(expr),
            ast::Expr::Extract { field, expr, .. } => self.scalar_call(
                "date_part",
                vec![
                    BoundExpr::literal(Value::Varchar(
                        field.to_string().trim_matches('\'').to_ascii_lowercase(),
                    )),
                    recurse(expr)?,
                ],
            ),
            ast::Expr::Interval(interval) => {
                let explicit_cast = |inner: BoundExpr, target: DataType| -> Result<BoundExpr> {
                    let cast = self.context.casts.bind(
                        &inner.data_type,
                        &target,
                        CastMode::Explicit,
                        self.context.query.types(),
                    )?;
                    Ok(BoundExpr {
                        data_type: target,
                        kind: ExprKind::Cast(Box::new(inner), cast.into(), false),
                    })
                };
                let inner = recurse(&interval.value)?;
                match interval_lowering(interval)? {
                    IntervalLowering::Cast => explicit_cast(inner, DataType::Interval),
                    IntervalLowering::Unit { function, target } => {
                        let mut inner = explicit_cast(inner, DataType::Double)?;
                        if target != DataType::Double {
                            inner = self.scalar_call("trunc", vec![inner])?;
                            inner = explicit_cast(inner, target)?;
                        }
                        self.scalar_call(function, vec![inner])
                    }
                }
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
                // sqlparser keeps a dotted name inside this node when a later
                // subscript follows it. Resolve that name before extracting
                // children: `t.xs[1]` names column xs in table t, whereas
                // `(t).xs[1]` explicitly extracts from the value t.
                let mut names = match root.as_ref() {
                    ast::Expr::Identifier(name) => vec![name.clone()],
                    ast::Expr::CompoundIdentifier(names) => names.clone(),
                    _ => Vec::new(),
                };
                let mut consumed = 0;
                if !names.is_empty() {
                    for access in access_chain {
                        let ast::AccessExpr::Dot(ast::Expr::Identifier(name)) = access else {
                            break;
                        };
                        names.push(name.clone());
                        consumed += 1;
                    }
                }
                let mut value = if consumed == 0 {
                    recurse(root)?
                } else {
                    recurse(&ast::Expr::CompoundIdentifier(names))?
                };
                for access in &access_chain[consumed..] {
                    match access {
                        ast::AccessExpr::Subscript(ast::Subscript::Index { index }) => {
                            value = self.nested_access(value, recurse(index)?)?;
                        }
                        ast::AccessExpr::Dot(ast::Expr::Identifier(name)) => {
                            value = self.nested_access(
                                value,
                                BoundExpr::literal(Value::Varchar(name.value.clone())),
                            )?;
                        }
                        ast::AccessExpr::Subscript(ast::Subscript::Slice {
                            lower_bound,
                            upper_bound,
                            stride,
                        }) => {
                            let mut arguments = vec![value];
                            let mut omitted_bounds = vec![false];
                            arguments.push(match lower_bound {
                                Some(bound) => recurse(bound)?,
                                None => BoundExpr::literal(Value::Null),
                            });
                            omitted_bounds.push(lower_bound.is_none());
                            arguments.push(match upper_bound {
                                Some(bound) => recurse(bound)?,
                                None => BoundExpr::literal(Value::Null),
                            });
                            omitted_bounds.push(upper_bound.is_none());
                            if let Some(stride) = stride {
                                arguments.push(recurse(stride)?);
                                omitted_bounds.push(false);
                            }
                            value = self.scalar_slice_call(
                                "array_slice",
                                arguments,
                                &omitted_bounds,
                            )?;
                        }
                        _ => return Err(unsupported("nested accessor")),
                    }
                }
                Ok(value)
            }
            ast::Expr::Array(array) => self.nested_constructor(
                array.elem.iter().map(&recurse).collect::<Result<_>>()?,
                None,
            ),
            ast::Expr::Tuple(values) => {
                self.scalar_call("row", values.iter().map(&recurse).collect::<Result<_>>()?)
            }
            ast::Expr::Map(map) => self.map_constructor(
                map.entries
                    .iter()
                    .map(|entry| Ok((recurse(&entry.key)?, recurse(&entry.value)?)))
                    .collect::<Result<_>>()?,
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
                    B::BitwiseAnd => Some(Operator::BitAnd),
                    B::BitwiseOr => Some(Operator::BitOr),
                    B::PGBitwiseShiftLeft => Some(Operator::ShiftLeft),
                    B::PGBitwiseShiftRight => Some(Operator::ShiftRight),
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
                    ast::UnaryOperator::BitwiseNot => {
                        return self.operator(Operator::BitNot, vec![inner]);
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
                let name = function_name(&function.name)?;
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
                    // Resolve the catalog entry before binding arguments. An
                    // absent function must not become an unsupported argument
                    // construct, and retain this same selection for binding.
                    let function_impl = self.context.functions.scalar(&name)?;
                    let parsed = super::nested::scalar_arguments(function)?;
                    let arguments = parsed.expressions
                        .iter()
                        .map(|expression| recurse(expression))
                        .collect::<Result<Vec<_>>>()?;
                    self.scalar_call_selected_named(function_impl, arguments, Some(&parsed.names), Some(&parsed.aliases))
                }
            }
            ast::Expr::Case {
                operand,
                conditions,
                else_result,
                ..
            } => {
                let mut branches = Vec::new();
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
                    branches.push((predicate, value));
                }
                let otherwise = else_result
                    .as_ref()
                    .map(|e| recurse(e))
                    .transpose()?
                    .unwrap_or_else(|| BoundExpr::literal(Value::Null));
                self.bound_case(branches,otherwise)
            }
            ast::Expr::Between {
                expr,
                negated,
                low,
                high,
            } => {
                let bound = self.bound_between(recurse(expr)?, recurse(low)?, recurse(high)?)?;
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
                let mut data_type = value.data_type.clone();
                let mut all_literals = super::coercion::string_literal(&value);
                for item in &list {
                    let literal = super::coercion::string_literal(item);
                    data_type = self.comparison_type(
                        &data_type,
                        all_literals,
                        &item.data_type,
                        literal,
                        true,
                    )?;
                    all_literals &= literal;
                }
                Ok(BoundExpr {
                    data_type: DataType::Boolean,
                    kind: ExprKind::InList(
                        Box::new(self.combination_cast(value, &data_type)?),
                        list.into_iter()
                            .map(|e| self.combination_cast(e, &data_type))
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
    names: Option<&'a [Option<String>]>,
    aliases: Option<&'a [Option<String>]>,
    omitted_slice_bounds: Option<&'a [bool]>,
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
    fn select_overload(
        &self,
        name: &str,
        candidates: &[crate::function::ScalarSignature],
    ) -> Result<usize> {
        super::overload::select(name, candidates, self.arguments, self.context)
    }
    fn argument_name(&self, index: usize) -> Result<Option<&str>> {
        self.data_type(index)?;
        Ok(self.names.and_then(|names| names[index].as_deref()))
    }
    fn argument_alias(&self, index: usize) -> Result<Option<&str>> {
        self.data_type(index)?;
        Ok(self.aliases.and_then(|aliases| aliases[index].as_deref()))
    }
    fn is_omitted_slice_bound(&self, index: usize) -> Result<bool> {
        self.data_type(index)?;
        Ok(self
            .omitted_slice_bounds
            .is_some_and(|bounds| bounds[index]))
    }
    fn is_string_literal(&self, index: usize) -> Result<bool> {
        self.arguments
            .get(index)
            .map(super::coercion::string_literal)
            .ok_or_else(|| Error::Bind("function argument outside signature".into()))
    }
    fn integer_literal(&self, index: usize) -> Result<Option<i128>> {
        self.arguments
            .get(index)
            .map(super::coercion::integer_literal)
            .ok_or_else(|| Error::Bind("function argument outside signature".into()))
    }
    fn full_integer_literal(
        &self,
        index: usize,
    ) -> Result<Option<crate::common::type_registry::IntegerLiteral>> {
        self.arguments
            .get(index)
            .map(super::coercion::full_integer_literal)
            .ok_or_else(|| Error::Bind("function argument outside signature".into()))
    }
    fn combination_cast_mode(&self, index: usize, target: &DataType) -> Result<CastMode> {
        let source = self.data_type(index)?;
        super::coercion::combination_cast_mode(self.context, &source, target)
    }
    fn combination(&self, indices: &[usize]) -> Result<crate::function::ArgumentCombination> {
        self.combination_in(indices, super::coercion::CombinationSequence::Ordered)
    }
    fn collection_combination(
        &self,
        indices: &[usize],
    ) -> Result<crate::function::ArgumentCombination> {
        self.combination_in(indices, super::coercion::CombinationSequence::Collection)
    }
    fn constant(&self, index: usize) -> Result<Value> {
        self.evaluate_constant(self.required_constant_argument(index)?)
    }
    fn is_closed(&self, index: usize) -> Result<bool> {
        Ok(self.closed_argument(index)?.is_some())
    }
    fn constant_if_closed(&self, index: usize) -> Result<Option<Value>> {
        self.closed_argument(index)?
            .map(|expression| self.evaluate_constant(expression))
            .transpose()
    }
    fn is_provably_null(&self, index: usize) -> Result<bool> {
        self.context.query.check()?;
        let expression = self
            .arguments
            .get(index)
            .ok_or_else(|| Error::Bind("function argument outside signature".into()))?;
        super::provably_null(expression, self.context)
    }
    fn constant_as(&self, index: usize, target: &DataType, mode: CastMode) -> Result<Value> {
        let expression = self.required_constant_argument(index)?;
        let mode = super::coercion::scalar_argument_cast_mode(expression, target, mode);
        let expression = expression.clone().cast(
            target.clone(),
            mode,
            self.context.casts,
            self.context.query.types(),
        )?;
        self.evaluate_constant(&expression)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FunctionArguments<'_, '_> {
    fn combination_in(
        &self,
        indices: &[usize],
        sequence: super::coercion::CombinationSequence,
    ) -> Result<crate::function::ArgumentCombination> {
        self.context.query.check()?;
        crate::function::validate_combination_indices(self, indices)?;
        let data_type = super::coercion::ordered_combination_type(
            self.context,
            indices.iter().map(|&index| &self.arguments[index]),
            sequence,
        )?;
        let cast_modes = indices
            .iter()
            .map(|&index| {
                self.context.query.check()?;
                super::coercion::combination_cast_mode(
                    self.context,
                    &self.arguments[index].data_type,
                    &data_type,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let proposal = crate::function::ArgumentCombination {
            data_type,
            cast_modes,
        };
        proposal.validate(indices.len(), self.context.query.types())?;
        Ok(proposal)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FunctionArguments<'_, '_> {
    fn required_constant_argument(&self, index: usize) -> Result<&BoundExpr> {
        self.closed_argument(index)?.ok_or_else(|| {
            Error::Bind("function requires a constant argument without effects".into())
        })
    }
    fn closed_argument(&self, index: usize) -> Result<Option<&BoundExpr>> {
        self.context.query.check()?;
        let expression = self
            .arguments
            .get(index)
            .ok_or_else(|| Error::Bind("function argument outside signature".into()))?;
        Ok(constant_expression(expression).then_some(expression))
    }
    fn evaluate_constant(&self, expression: &BoundExpr) -> Result<Value> {
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

fn has_runtime_bound_input(expression: &BoundExpr) -> bool {
    let mut found = matches!(
        expression.kind,
        ExprKind::Parameter(_) | ExprKind::Subquery(_)
    );
    expression.visit_children(&mut |child| found |= has_runtime_bound_input(child));
    found
}
