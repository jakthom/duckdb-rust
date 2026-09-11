//! Explicit service bundle for closed catalog expressions. No built-in services
//! are constructed here; frontends, casts and evaluation remain selected.
use super::{BindContext, Binder};
use crate::{
    catalog::{
        Catalog,
        expression::{StoredExpression, StoredExpressionEvaluator},
    },
    common::{
        DataType, Error, Result, Value,
        cast::{CastMode, CastRegistry},
    },
    execution::expression_executor::ExpressionEvaluator,
    function::{FunctionRegistry, operator::OperatorRegistry},
    parallel::QueryContext,
};
use std::sync::Arc;

pub struct SelectedStoredExpressions {
    binder: Arc<dyn Binder>,
    casts: CastRegistry,
    operators: OperatorRegistry,
    functions: FunctionRegistry,
    expressions: Arc<dyn ExpressionEvaluator>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SelectedStoredExpressions {
    pub fn new(
        binder: Arc<dyn Binder>,
        casts: CastRegistry,
        operators: OperatorRegistry,
        functions: FunctionRegistry,
        expressions: Arc<dyn ExpressionEvaluator>,
    ) -> Self {
        Self {
            binder,
            casts,
            operators,
            functions,
            expressions,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl StoredExpressionEvaluator for SelectedStoredExpressions {
    fn evaluate(
        &self,
        expression: &StoredExpression,
        target: &DataType,
        catalog: &dyn Catalog,
        query: &QueryContext,
    ) -> Result<Value> {
        expression.validate(query)?;
        let selected = query.types().bind(target)?;
        let bound = self.binder.bind_stored_expression(
            expression,
            &BindContext {
                catalog,
                casts: &self.casts,
                operators: &self.operators,
                query,
                functions: &self.functions,
                expressions: self.expressions.as_ref(),
                parameters: &[],
            },
        )?;
        // Validate a replacement binder before inserting even the target cast.
        bound.validate_closed(catalog, query)?;
        let bound = bound.cast(
            target.clone(),
            CastMode::Assignment,
            &self.casts,
            query.types(),
        )?;
        bound.validate_closed(catalog, query)?;
        let value = self.expressions.evaluate(&bound, &Vec::new(), query)?;
        selected
            .validate(&value, query)
            .map_err(|error| match error {
                Error::Conversion(_) => {
                    Error::Internal("stored expression evaluator returned an invalid value".into())
                }
                other => other,
            })?;
        query.check()?;
        Ok(value)
    }
}
