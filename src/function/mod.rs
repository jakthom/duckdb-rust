mod aggregate;
pub mod grouped;
pub mod operator;
mod scalar;
mod settings;

use std::{collections::BTreeMap, fmt::Debug, sync::Arc};

use crate::{
    common::{DataType, Error, Result, Value, vector::DataChunk},
    parallel::QueryContext,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct FunctionEffects {
    pub volatile: bool,
    pub external_access: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ArgumentEvaluation {
    Eager,
    FirstNonNull,
}

/// Language-owned argument metadata for contextual function binding. A
/// constant request may evaluate only a closed expression without effects,
/// through the selected evaluator, and must validate its logical result.
/// Requests are explicit so ordinary functions preserve lazy/error behavior.
pub trait ScalarBindArguments {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn data_type(&self, index: usize) -> Result<DataType>;
    fn constant(&self, index: usize) -> Result<Value>;
}

pub trait ScalarFunction: Debug + Send + Sync {
    fn name(&self) -> &str;
    fn effects(&self) -> FunctionEffects {
        FunctionEffects::default()
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        ArgumentEvaluation::Eager
    }
    /// Pure statement-local specialization. None retains this adapter. A
    /// returned adapter owns all retained state and uses the ordinary signature,
    /// evaluation, NULL/error and output-validation contracts. It cannot borrow
    /// the binding context or publish effects during construction.
    fn bind(
        &self,
        _arguments: &dyn ScalarBindArguments,
        _query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        Ok(None)
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType>;
    fn evaluate(&self, arguments: &[Value], context: &QueryContext) -> Result<Value>;
}

pub trait AggregateState: Send {
    /// Failed updates invalidate the state; callers must discard it.
    fn update(&mut self, arguments: &[Value], context: &QueryContext) -> Result<()>;
    /// Consume argument columns in row order, including NULLs. Zero columns
    /// retain cardinality for count(*). Adapters may process columns directly,
    /// preserving scalar update results/errors and cooperative cancellation.
    fn update_batch(&mut self, arguments: &DataChunk, context: &QueryContext) -> Result<()> {
        update_aggregate_rows(self, arguments, context)
    }
    fn finish(self: Box<Self>) -> Result<Value>;
}

pub(crate) fn update_aggregate_rows<S: AggregateState + ?Sized>(
    state: &mut S,
    arguments: &DataChunk,
    context: &QueryContext,
) -> Result<()> {
    let mut row = Vec::with_capacity(arguments.columns().len());
    for index in 0..arguments.len() {
        context.check()?;
        arguments.read_row(index, &mut row)?;
        state.update(&row, context)?;
    }
    context.check()
}

pub trait AggregateFunction: Debug + Send + Sync {
    fn name(&self) -> &str;
    fn return_type(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType>;
    fn create_state(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Box<dyn AggregateState>>;
    /// Optional pure grouped updates. Returning None retains ordered scalar
    /// states; callers must not infer this capability from a function's name.
    /// Construction has no effects. See GroupedAggregateState's full contract.
    fn create_grouped_state(
        &self,
        _arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Option<Box<dyn grouped::GroupedAggregateState>>> {
        Ok(None)
    }
}

#[derive(Clone, Default)]
pub struct FunctionRegistry {
    scalars: BTreeMap<String, Arc<dyn ScalarFunction>>,
    aggregates: BTreeMap<String, Arc<dyn AggregateFunction>>,
}

impl FunctionRegistry {
    pub fn builtins() -> Self {
        let mut registry = Self::default();
        scalar::register(&mut registry);
        aggregate::register(&mut registry);
        settings::register(&mut registry);
        registry
    }
    pub fn register_scalar(&mut self, function: Arc<dyn ScalarFunction>) -> Result<()> {
        let key = function.name().to_ascii_lowercase();
        if self.scalars.contains_key(&key) || self.aggregates.contains_key(&key) {
            return Err(Error::Catalog(format!("function {key} already exists")));
        }
        self.scalars.insert(key, function);
        Ok(())
    }
    pub fn register_aggregate(&mut self, function: Arc<dyn AggregateFunction>) -> Result<()> {
        let key = function.name().to_ascii_lowercase();
        if self.scalars.contains_key(&key) || self.aggregates.contains_key(&key) {
            return Err(Error::Catalog(format!("function {key} already exists")));
        }
        self.aggregates.insert(key, function);
        Ok(())
    }
    pub fn scalar(&self, name: &str) -> Result<Arc<dyn ScalarFunction>> {
        self.scalars
            .get(&name.to_ascii_lowercase())
            .cloned()
            .ok_or_else(|| {
                Error::Catalog(format!("Scalar Function with name {name} does not exist!"))
            })
    }
    pub fn aggregate(&self, name: &str) -> Option<Arc<dyn AggregateFunction>> {
        self.aggregates.get(&name.to_ascii_lowercase()).cloned()
    }
}
