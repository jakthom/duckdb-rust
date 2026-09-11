//! Conditional scalar families retain frontend combination metadata. This is
//! not permission to cast or evaluate child expressions inside a function.
use std::sync::Arc;

use crate::{
    common::{DataType, Error, Result, Value, cast::CastMode, type_registry::TypeRegistry},
    function::{
        ArgumentCombination, ArgumentEvaluation, FunctionRegistry, ScalarBindArguments,
        ScalarFunction,
    },
    parallel::QueryContext,
};

#[derive(Debug)]
struct Coalesce(Option<ArgumentCombination>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    registry
        .register_scalar(Arc::new(Coalesce(None)))
        .expect("unique coalesce function");
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Coalesce {
    fn proposal(&self) -> Result<&ArgumentCombination> {
        self.0
            .as_ref()
            .ok_or_else(|| Error::Internal("unbound COALESCE function".into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Coalesce {
    fn name(&self) -> &str {
        "coalesce"
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        if arguments.is_empty() {
            return Err(Error::Bind(
                "COALESCE requires at least one argument".into(),
            ));
        }
        let indices = (0..arguments.len()).collect::<Vec<_>>();
        let proposal = arguments.combination(&indices)?;
        proposal.validate(arguments.len(), query.types())?;
        Ok(Some(Arc::new(Self(Some(proposal)))))
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        ArgumentEvaluation::FirstNonNull
    }
    fn argument_types(
        &self,
        arguments: &[DataType],
        types: &TypeRegistry,
    ) -> Result<Vec<DataType>> {
        let proposal = self.proposal()?;
        proposal.validate(arguments.len(), types)?;
        Ok(vec![proposal.data_type.clone(); arguments.len()])
    }
    fn argument_cast_mode(&self, index: usize) -> CastMode {
        // Signature validation rejects unbound/out-of-range calls before this
        // infallible metadata hook is consulted by an execution frontend.
        self.0
            .as_ref()
            .and_then(|proposal| proposal.cast_modes.get(index))
            .copied()
            .unwrap_or(CastMode::Implicit)
    }
    fn argument_literal_coercion(&self, _: usize) -> bool {
        false
    }
    fn return_type(&self, arguments: &[DataType], types: &TypeRegistry) -> Result<DataType> {
        let proposal = self.proposal()?;
        proposal.validate(arguments.len(), types)?;
        if arguments.iter().any(|ty| *ty != proposal.data_type) {
            return Err(Error::Bind(
                "COALESCE arguments differ from retained combination type".into(),
            ));
        }
        Ok(proposal.data_type.clone())
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let proposal = self.proposal()?;
        proposal.validate(arguments.len(), query.types())?;
        let output = arguments
            .iter()
            .find(|value| !value.is_null())
            .cloned()
            .unwrap_or(Value::Null);
        query
            .types()
            .bind(&proposal.data_type)?
            .validate(&output, query)?;
        Ok(output)
    }
}
