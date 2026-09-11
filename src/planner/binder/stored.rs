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
                    return Err(unsupported("stored operator binding"));
                }
                let [name] = name.as_slice() else {
                    return Err(unsupported("qualified stored function binding"));
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
}
