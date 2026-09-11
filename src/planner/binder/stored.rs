//! Bind retained catalog nodes without parsing SQL or evaluating a default.
use super::*;
use crate::catalog::expression::{StoredExpression, StoredExpressionKind};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn stored_expression(&self, expression: &StoredExpression) -> Result<BoundExpr> {
        self.context.query.check()?;
        match &expression.kind {
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
                ..
            } => {
                if *is_operator {
                    return Err(unsupported("stored operator binding"));
                }
                let [name] = name.as_slice() else {
                    return Err(unsupported("qualified stored function binding"));
                };
                // Match ordinary SQL's catalog-before-argument resolution.
                let function = self.context.functions.scalar(name)?;
                if arguments.iter().any(|argument| argument.name.is_some()) {
                    return Err(unsupported("named stored function argument binding"));
                }
                let arguments = arguments
                    .iter()
                    .map(|argument| self.stored_expression(&argument.expression))
                    .collect::<Result<Vec<_>>>()?;
                self.scalar_call_selected(function, arguments)
            }
        }
    }
}
