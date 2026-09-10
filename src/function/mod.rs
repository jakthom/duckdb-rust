mod aggregate;
mod binary_scalar;
mod enumeration;
pub mod grouped;
pub(crate) mod nested;
pub mod operator;
mod scalar;
mod settings;
pub mod window;

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
    /// Binding consumes only declared argument types. No argument expression
    /// is evaluated at execution; the bound function receives an empty slice.
    TypeOnly,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
    /// Required logical argument types. The language binder inserts casts
    /// through the selected cast registry, then checks the result signature.
    /// Return one type per argument; adapters cannot change call arity here.
    fn argument_types(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        Ok(arguments.to_vec())
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType>;
    fn evaluate(&self, arguments: &[Value], context: &QueryContext) -> Result<Value>;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub trait AggregateState: Send {
    /// Failed updates invalidate the state; callers must discard it.
    fn update(&mut self, arguments: &[Value], context: &QueryContext) -> Result<()>;
    /// Consume argument columns in row order, including NULLs. Zero columns
    /// retain cardinality for count(*). Adapters may process columns directly,
    /// preserving scalar update results/errors and cooperative cancellation.
    fn update_batch(&mut self, arguments: &DataChunk, context: &QueryContext) -> Result<()> {
        update_aggregate_rows(self, arguments, context)
    }
    /// Borrow one already validated argument column without constructing a
    /// temporary chunk. The default retains the selected batch implementation;
    /// overrides have exactly the same row, NULL, error and cancellation rules.
    fn update_column(
        &mut self,
        column: &crate::common::vector::Vector,
        context: &QueryContext,
    ) -> Result<()> {
        self.update_batch(
            &DataChunk::new(vec![column.clone()], column.len())?,
            context,
        )
    }
    fn finish(self: Box<Self>) -> Result<Value>;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
    /// Optional partition evaluation through the same window contract. None
    /// requests the generic frame evaluator; callers never inspect function names.
    fn evaluate_window(
        &self,
        _input: &window::WindowInput<'_>,
        _query: &QueryContext,
    ) -> Result<Option<Vec<Value>>> {
        Ok(None)
    }
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
    windows: BTreeMap<String, Arc<dyn window::WindowFunction>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FunctionRegistry {
    pub fn builtins() -> Self {
        let mut registry = Self::default();
        scalar::register(&mut registry);
        binary_scalar::register(&mut registry);
        enumeration::register(&mut registry);
        nested::register(&mut registry);
        aggregate::register(&mut registry);
        settings::register(&mut registry);
        window::register(&mut registry);
        registry
    }
    pub fn register_window(&mut self, function: Arc<dyn window::WindowFunction>) -> Result<()> {
        let key = function.name().to_ascii_lowercase();
        if self.windows.contains_key(&key) {
            return Err(Error::Catalog(format!(
                "window function {key} already exists"
            )));
        }
        self.windows.insert(key, function);
        Ok(())
    }
    pub fn window(&self, name: &str) -> Result<Arc<dyn window::WindowFunction>> {
        self.windows
            .get(&name.to_ascii_lowercase())
            .cloned()
            .or_else(|| {
                self.aggregate(name).map(|function| {
                    Arc::new(window::AggregateWindow(function)) as Arc<dyn window::WindowFunction>
                })
            })
            .ok_or_else(|| Error::Catalog(format!("window function {name} does not exist")))
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
