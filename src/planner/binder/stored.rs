//! Bind retained catalog nodes without parsing SQL or evaluating a default.
use super::*;
use crate::catalog::expression::{
    StoredArgumentStyle, StoredExpression, StoredExpressionKind, StoredOperator,
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
                let name = name.join(".");
                // Match ordinary SQL's catalog-before-argument resolution.
                let function = self.context.functions.scalar(&name)?;
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
}
