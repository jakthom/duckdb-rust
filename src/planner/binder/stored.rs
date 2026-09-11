//! Bind retained catalog nodes without parsing SQL or evaluating a default.
use super::*;
use crate::catalog::expression::{
    StoredArgumentStyle, StoredComparison, StoredConjunction, StoredExpression,
    StoredExpressionKind, StoredOperator,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn stored_expression(&self, expression: &StoredExpression) -> Result<BoundExpr> {
        self.context.query.check()?;
        match &expression.kind {
            StoredExpressionKind::Operator { kind, children } => {
                let children = children
                    .iter()
                    .map(|child| self.stored_expression(child))
                    .collect::<Result<Vec<_>>>()?;
                match kind {
                    StoredOperator::ListConstructor => self.scalar_call("list_value", children),
                    StoredOperator::Not => {
                        let [inner]: [BoundExpr; 1] = children
                            .try_into()
                            .map_err(|_| Error::Bind("invalid stored NOT arity".into()))?;
                        let inner = self.boolean(inner)?;
                        Ok(BoundExpr {
                            data_type: DataType::Boolean,
                            kind: ExprKind::Unary(UnaryOp::Not, Box::new(inner)),
                        })
                    }
                    StoredOperator::IsNull | StoredOperator::IsNotNull => {
                        let [inner]: [BoundExpr; 1] = children
                            .try_into()
                            .map_err(|_| Error::Bind("invalid stored NULL test arity".into()))?;
                        Ok(BoundExpr {
                            data_type: DataType::Boolean,
                            kind: ExprKind::Unary(
                                if *kind == StoredOperator::IsNull {
                                    UnaryOp::IsNull
                                } else {
                                    UnaryOp::IsNotNull
                                },
                                Box::new(inner),
                            ),
                        })
                    }
                    StoredOperator::In | StoredOperator::NotIn => {
                        self.stored_in(children, *kind == StoredOperator::NotIn)
                    }
                    StoredOperator::Index | StoredOperator::Field => {
                        let [value, key]: [BoundExpr; 2] = children
                            .try_into()
                            .map_err(|_| Error::Bind("invalid stored accessor arity".into()))?;
                        if *kind == StoredOperator::Field
                            && !matches!(
                                &value.data_type,
                                DataType::Nested(metadata) if matches!(metadata.as_ref(),
                                    crate::common::NestedType::Struct(_) | crate::common::NestedType::Union(_)
                                    | crate::common::NestedType::Map { .. } | crate::common::NestedType::Variant)
                            )
                        {
                            return Err(Error::Bind(
                                "field extraction requires a named nested value".into(),
                            ));
                        }
                        self.nested_access(value, key)
                    }
                }
            }
            StoredExpressionKind::Case { checks, otherwise } => {
                let branches = checks
                    .iter()
                    .map(|check| {
                        Ok((
                            self.boolean(self.stored_expression(&check.when_expression)?)?,
                            self.stored_expression(&check.then_expression)?,
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let otherwise = self.stored_expression(otherwise)?;
                self.bound_case(branches, otherwise)
            }
            StoredExpressionKind::Comparison { kind, left, right } => self.binary(
                match kind {
                    StoredComparison::Equal => BinaryOp::Equal,
                    StoredComparison::NotEqual => BinaryOp::NotEqual,
                    StoredComparison::LessThan => BinaryOp::Less,
                    StoredComparison::GreaterThan => BinaryOp::Greater,
                    StoredComparison::LessThanOrEqual => BinaryOp::LessEqual,
                    StoredComparison::GreaterThanOrEqual => BinaryOp::GreaterEqual,
                },
                self.stored_expression(left)?,
                self.stored_expression(right)?,
            ),
            StoredExpressionKind::Conjunction { kind, children } => {
                let mut children = children.iter();
                let first =
                    self.stored_expression(children.next().ok_or_else(|| {
                        Error::Bind("stored conjunction has no children".into())
                    })?)?;
                children.try_fold(first, |left, right| {
                    self.binary(
                        match kind {
                            StoredConjunction::And => BinaryOp::And,
                            StoredConjunction::Or => BinaryOp::Or,
                        },
                        left,
                        self.stored_expression(right)?,
                    )
                })
            }
            StoredExpressionKind::Between {
                input,
                lower,
                upper,
            } => self.bound_between(
                self.stored_expression(input)?,
                self.stored_expression(lower)?,
                self.stored_expression(upper)?,
            ),
            StoredExpressionKind::Literal { data_type, value } => Ok(BoundExpr {
                data_type: data_type.clone(),
                kind: ExprKind::Literal(value.clone()),
            }),
            StoredExpressionKind::Cast {
                expression,
                target,
                try_cast,
            } => {
                let inner = self.stored_expression(expression)?;
                // Even a same-type explicit cast retains its source provenance
                // and selected binding. Do not turn it back into a SQL literal.
                let cast = self.context.casts.bind(
                    &inner.data_type,
                    target,
                    CastMode::Explicit,
                    self.context.query.types(),
                )?;
                Ok(BoundExpr {
                    data_type: target.clone(),
                    kind: ExprKind::Cast(Box::new(inner), cast.into(), *try_cast),
                })
            }
            StoredExpressionKind::Function {
                name,
                arguments,
                is_operator,
                argument_style,
            } => {
                if *is_operator {
                    return self.stored_operator(name, arguments);
                }
                let name = match name.as_slice() {
                    [function] => function,
                    [schema, function] if schema.eq_ignore_ascii_case("main") => function,
                    _ => {
                        return Err(unsupported(format!(
                            "qualified stored function {}",
                            name.join(".")
                        )));
                    }
                };
                // Match ordinary SQL's catalog-before-argument resolution.
                let function = self.context.functions.scalar(name)?;
                let names = arguments
                    .iter()
                    .map(|argument| {
                        if *argument_style == StoredArgumentStyle::Named {
                            argument.name.clone()
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                let aliases = arguments
                    .iter()
                    .map(|argument| {
                        argument
                            .name
                            .clone()
                            .or_else(|| argument.expression.alias.clone())
                    })
                    .collect::<Vec<_>>();
                let arguments = arguments
                    .iter()
                    .map(|argument| self.stored_expression(&argument.expression))
                    .collect::<Result<Vec<_>>>()?;
                self.scalar_call_selected_named(function, arguments, Some(&names), Some(&aliases))
            }
        }
    }

    fn stored_operator(
        &self,
        name: &[String],
        arguments: &[crate::catalog::expression::StoredArgument],
    ) -> Result<BoundExpr> {
        let [name] = name else {
            return Err(unsupported("qualified stored operator"));
        };
        if arguments.iter().any(|argument| argument.name.is_some()) {
            return Err(Error::Bind(
                "stored operators do not accept named arguments".into(),
            ));
        }
        let arguments = arguments
            .iter()
            .map(|argument| self.stored_expression(&argument.expression))
            .collect::<Result<Vec<_>>>()?;
        let arity = arguments.len();
        match (name.as_str(), arity) {
            ("case_when", arity) if arity >= 3 && arity % 2 == 1 => {
                return self.stored_case(arguments, false);
            }
            ("case_operand", arity) if arity >= 4 && arity % 2 == 0 => {
                return self.stored_case(arguments, true);
            }
            ("is_null", 1) | ("is_not_null", 1) => {
                let [inner]: [BoundExpr; 1] = arguments
                    .try_into()
                    .map_err(|_| Error::Bind("invalid stored NULL test arity".into()))?;
                return Ok(BoundExpr {
                    data_type: DataType::Boolean,
                    kind: ExprKind::Unary(
                        if name == "is_null" {
                            UnaryOp::IsNull
                        } else {
                            UnaryOp::IsNotNull
                        },
                        Box::new(inner),
                    ),
                });
            }
            ("between", 3) | ("not_between", 3) => {
                let [value, low, high]: [BoundExpr; 3] = arguments
                    .try_into()
                    .map_err(|_| Error::Bind("invalid stored BETWEEN arity".into()))?;
                let bound = self.bound_between(value, low, high)?;
                return Ok(if name == "not_between" {
                    BoundExpr {
                        data_type: DataType::Boolean,
                        kind: ExprKind::Unary(UnaryOp::Not, Box::new(bound)),
                    }
                } else {
                    bound
                });
            }
            ("in", arity) | ("not_in", arity) if arity >= 2 => {
                return self.stored_in(arguments, name == "not_in");
            }
            _ => {}
        }
        use crate::function::operator::Operator as O;
        let selected = match (name.as_str(), arity) {
            ("+", 1) => Some(O::Plus),
            ("-", 1) => Some(O::Negate),
            ("~", 1) => Some(O::BitNot),
            ("+", 2) => Some(O::Add),
            ("-", 2) => Some(O::Subtract),
            ("*", 2) => Some(O::Multiply),
            ("/", 2) => Some(O::Divide),
            ("//", 2) => Some(O::IntegerDivide),
            ("%", 2) => Some(O::Modulo),
            ("||", 2) => Some(O::Concat),
            ("&", 2) => Some(O::BitAnd),
            ("|", 2) => Some(O::BitOr),
            ("<<", 2) => Some(O::ShiftLeft),
            (">>", 2) => Some(O::ShiftRight),
            ("~~", 2) => Some(O::Like),
            ("!~~", 2) => Some(O::NotLike),
            _ => None,
        };
        if let Some(selected) = selected {
            return self.operator(selected, arguments);
        }
        let binary = match (name.as_str(), arity) {
            ("=", 2) => Some(BinaryOp::Equal),
            ("!=", 2) => Some(BinaryOp::NotEqual),
            ("<", 2) => Some(BinaryOp::Less),
            ("<=", 2) => Some(BinaryOp::LessEqual),
            (">", 2) => Some(BinaryOp::Greater),
            (">=", 2) => Some(BinaryOp::GreaterEqual),
            ("and", 2) => Some(BinaryOp::And),
            ("or", 2) => Some(BinaryOp::Or),
            _ => None,
        };
        if let Some(binary) = binary {
            let [left, right]: [BoundExpr; 2] = arguments
                .try_into()
                .map_err(|_| Error::Bind("invalid stored binary operator arity".into()))?;
            return self.binary(binary, left, right);
        }
        if name == "not" && arity == 1 {
            let [inner]: [BoundExpr; 1] = arguments
                .try_into()
                .map_err(|_| Error::Bind("invalid stored unary operator arity".into()))?;
            let inner = self.boolean(inner)?;
            return Ok(BoundExpr {
                data_type: DataType::Boolean,
                kind: ExprKind::Unary(UnaryOp::Not, Box::new(inner)),
            });
        }
        Err(Error::Bind(format!(
            "unsupported stored operator {name} with {arity} arguments"
        )))
    }

    fn stored_case(&self, arguments: Vec<BoundExpr>, has_operand: bool) -> Result<BoundExpr> {
        let mut arguments = arguments.into_iter();
        let operand = has_operand
            .then(|| {
                arguments
                    .next()
                    .ok_or_else(|| Error::Bind("stored CASE has no operand".into()))
            })
            .transpose()?;
        let mut remaining = arguments.collect::<Vec<_>>();
        let otherwise = remaining
            .pop()
            .ok_or_else(|| Error::Bind("stored CASE has no ELSE expression".into()))?;
        if remaining.len() < 2 || remaining.len() % 2 != 0 {
            return Err(Error::Bind("invalid stored CASE arity".into()));
        }
        let mut branches = Vec::with_capacity(remaining.len() / 2);
        let mut remaining = remaining.into_iter();
        while let Some(condition) = remaining.next() {
            let value = remaining
                .next()
                .ok_or_else(|| Error::Bind("stored CASE has no result".into()))?;
            let predicate = if let Some(operand) = &operand {
                self.binary(BinaryOp::Equal, operand.clone(), condition)?
            } else {
                self.boolean(condition)?
            };
            branches.push((predicate, value));
        }
        self.bound_case(branches, otherwise)
    }

    fn stored_in(&self, arguments: Vec<BoundExpr>, negated: bool) -> Result<BoundExpr> {
        let mut arguments = arguments.into_iter();
        let value = arguments
            .next()
            .ok_or_else(|| Error::Bind("stored IN has no left operand".into()))?;
        let list = arguments.collect::<Vec<_>>();
        if list.is_empty() {
            return Err(Error::Bind("stored IN has no values".into()));
        }
        let mut data_type = value.data_type.clone();
        let mut all_literals = super::coercion::string_literal(&value);
        for item in &list {
            let literal = super::coercion::string_literal(item);
            data_type =
                self.comparison_type(&data_type, all_literals, &item.data_type, literal, true)?;
            all_literals &= literal;
        }
        Ok(BoundExpr {
            data_type: DataType::Boolean,
            kind: ExprKind::InList(
                Box::new(self.combination_cast(value, &data_type)?),
                list.into_iter()
                    .map(|expression| self.combination_cast(expression, &data_type))
                    .collect::<Result<Vec<_>>>()?,
                negated,
                self.context.query.types().bind(&data_type)?.into(),
            ),
        })
    }
}
