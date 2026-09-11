mod aggregate;
pub mod bignum;
mod binary_scalar;
mod bit;
mod enumeration;
pub mod grouped;
pub(crate) mod nested;
pub mod operator;
mod scalar;
mod settings;
mod temporal;
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

/// Execution-owned encoding information for an already evaluated argument.
/// Constant means one physical value for the current input batch, not merely
/// equal observed values, a one-row relation, or a closed bound expression.
/// Unknown includes flat/dictionary inputs and frontends without this metadata.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ArgumentProvenance {
    Constant,
    #[default]
    Unknown,
}

/// Owned frontend combination metadata in the requested argument order. It
/// contains no values, borrowed expressions or ambient evaluation services.
#[derive(Debug, Clone)]
pub struct ArgumentCombination {
    pub data_type: DataType,
    pub cast_modes: Vec<crate::common::cast::CastMode>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ArgumentCombination {
    pub fn validate(
        &self,
        count: usize,
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<()> {
        if count == 0
            || self.cast_modes.len() != count
            || self
                .cast_modes
                .contains(&crate::common::cast::CastMode::Assignment)
        {
            return Err(Error::Internal(
                "invalid argument combination proposal".into(),
            ));
        }
        types.bind(&self.data_type)?;
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn validate_combination_indices<A: ScalarBindArguments + ?Sized>(
    arguments: &A,
    indices: &[usize],
) -> Result<()> {
    if indices.is_empty() || indices.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(Error::Bind(
            "combination arguments must be nonempty and in source order".into(),
        ));
    }
    for &index in indices {
        arguments.data_type(index)?;
    }
    Ok(())
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
    /// SQL string-literal identity is contextual binding information, not an
    /// implicit cast granted to every VARCHAR column or external frontend.
    fn is_string_literal(&self, index: usize) -> Result<bool> {
        self.data_type(index).map(|_| false)
    }
    /// A signed integer SQL literal, including a directly negated numeric
    /// token. Typed parameters, explicit casts and folded expressions are not
    /// literals. This is binding metadata, not a global narrowing conversion.
    fn integer_literal(&self, index: usize) -> Result<Option<i128>> {
        self.data_type(index).map(|_| None)
    }
    /// Infer a common type in increasing source-index order, normalizing each
    /// pair, and retain each selected Implicit-or-Explicit combination mode.
    /// This request evaluates nothing, including closed or effectful children.
    /// Consumers validate metadata/cardinality and retain the returned modes.
    fn combination(&self, indices: &[usize]) -> Result<ArgumentCombination> {
        validate_combination_indices(self, indices)?;
        Err(Error::Unsupported(
            "frontend does not support argument combination metadata".into(),
        ))
    }
    fn constant(&self, index: usize) -> Result<Value>;
    /// Classify a closed, effect-free argument without evaluating it, invoking
    /// a cast, or inferring its value. This metadata is not a promise about a
    /// later physical vector's encoding. Unsupported frontends reject it.
    fn is_closed(&self, index: usize) -> Result<bool> {
        self.data_type(index)?;
        Err(Error::Unsupported(
            "frontend does not support closed argument metadata".into(),
        ))
    }
    /// Optional closed/effect-free evaluation through the selected frontend.
    /// None means the expression is not eligible, never an evaluation failure.
    /// Unsupported frontends reject rather than guessing about dependencies.
    fn constant_if_closed(&self, index: usize) -> Result<Option<Value>> {
        self.data_type(index)?;
        Err(Error::Unsupported(
            "frontend does not support optional closed constants".into(),
        ))
    }
    /// Speculative NULL-template probe, distinct from required constant
    /// evaluation. Data-dependent errors may make this probe unsuccessful;
    /// infrastructure and selected logical validation failures remain fatal.
    fn is_provably_null(&self, index: usize) -> Result<bool> {
        self.data_type(index)?;
        Err(Error::Unsupported(
            "frontend does not support NULL-template probes".into(),
        ))
    }
    /// Request a closed, effect-free argument converted through the frontend's
    /// selected cast registry and evaluator. SQL literal privileges apply only
    /// to an Implicit request; Explicit and Assignment retain their exact mode.
    /// Validate the converted logical value and preserve every cast/evaluator
    /// failure. Frontends without this capability must reject it explicitly.
    fn constant_as(
        &self,
        index: usize,
        _target: &DataType,
        _mode: crate::common::cast::CastMode,
    ) -> Result<Value> {
        self.data_type(index)?;
        Err(Error::Unsupported(
            "frontend does not support selected typed constant arguments".into(),
        ))
    }
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
    /// Conversion policy for one argument of the selected specialization.
    /// Language binding still inserts and retains a checked cast from its
    /// selected registry; this does not authorize conversion inside evaluate.
    /// SQL literal privileges remain separate contextual information.
    /// Explicit or assignment policies retain their exact selected mode even
    /// for literals; contextual privilege applies only to implicit requests.
    /// Other frontends must honor this policy or reject unsupported binding.
    fn argument_cast_mode(&self, _index: usize) -> crate::common::cast::CastMode {
        crate::common::cast::CastMode::Implicit
    }
    /// Whether the frontend may apply its ordinary literal privilege after
    /// selecting this argument's mode. False preserves that mode exactly; it
    /// does not reject literal inputs. Combination specializations use false
    /// when the frontend has already supplied their final selected modes.
    fn argument_literal_coercion(&self, _index: usize) -> bool {
        true
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType>;
    fn evaluate(&self, arguments: &[Value], context: &QueryContext) -> Result<Value>;
    /// Execute with owned metadata for arguments already evaluated in ordinary
    /// child order. This does not authorize reevaluation, eager lazy branches,
    /// suppressed errors, or a lookup outside the retained selected adapter.
    /// Existing adapters keep their exact scalar callback through the default.
    fn evaluate_with_provenance(
        &self,
        arguments: &[Value],
        provenance: &[ArgumentProvenance],
        context: &QueryContext,
    ) -> Result<Value> {
        context.check()?;
        if arguments.len() != provenance.len() {
            return Err(Error::Internal(
                "scalar argument provenance differs from argument count".into(),
            ));
        }
        self.evaluate(arguments, context)
    }
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
        temporal::register(&mut registry);
        binary_scalar::register(&mut registry);
        bit::register(&mut registry);
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
