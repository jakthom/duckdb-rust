pub mod aggregation;
mod binder;
pub mod expression;
pub mod logical;
mod recursive;
pub mod stored;
mod validation;

pub use binder::SqlBinder;
pub use expression::{BoundExpr, ExprKind};
pub use logical::{BoundStatement, Field, LogicalPlan, PlanNode, Schema};
pub use recursive::RecursiveId;

use crate::{
    catalog::Catalog,
    common::{Result, Value, cast::CastRegistry},
    function::FunctionRegistry,
    parser::Statement,
};

pub struct BindContext<'a> {
    pub catalog: &'a dyn Catalog,
    pub casts: &'a CastRegistry,
    pub operators: &'a crate::function::operator::OperatorRegistry,
    pub query: &'a crate::parallel::QueryContext,
    pub functions: &'a FunctionRegistry,
    pub expressions: &'a dyn crate::execution::expression_executor::ExpressionEvaluator,
    pub parameters: &'a [Value],
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub trait Binder: Send + Sync {
    fn name(&self) -> &'static str;
    fn bind(&self, statement: &Statement, context: &BindContext<'_>) -> Result<BoundStatement>;
    /// Bind an owned catalog expression using these exact selected services.
    /// No parser/stringification or ambient registry fallback is permitted.
    /// The result must be closed against row/subquery/parameter dependencies; a
    /// caller still validates its effects and evaluates in its statement context.
    /// An ordinary replacement binder grants no stored-expression capability.
    fn bind_stored_expression(
        &self,
        expression: &crate::catalog::expression::StoredExpression,
        context: &BindContext<'_>,
    ) -> Result<BoundExpr> {
        expression.validate(context.query)?;
        Err(crate::Error::Unsupported(
            "stored expressions on this binder".into(),
        ))
    }
}

pub mod window;
