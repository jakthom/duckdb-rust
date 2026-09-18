//! Window functions consume one ordered partition with explicit peer/frame bounds.
use super::{AggregateFunction, FunctionRegistry};
use crate::{
    common::{DataType, Error, Result, Value, type_registry::TypeRegistry},
    parallel::QueryContext,
};
use std::{collections::HashSet, fmt::Debug, ops::Range, sync::Arc};

mod input;
pub use input::{NullTreatment, WindowBounds, WindowInput, WindowOptions, WindowRows};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Ordinary registered functions, independent of partitioning/sorting adapters.
/// Each call owns its state and returns one validated value per partition row.
/// Failure/cancellation returns no partial output. Implementations must check
/// cancellation during long loops and respect query resource limits.
pub trait WindowFunction: Debug + Send + Sync {
    /// True only when an unordered, unpartitioned call is independent of its
    /// frame and can emit every requested prefix without reading future rows.
    /// The engine uses this capability for unmodified calls with pure, total
    /// arguments; other calls retain ordered blocking evaluation.
    fn supports_streaming(&self) -> bool {
        false
    }
    fn start_stream(&self) -> Result<Box<dyn StreamingWindowState>> {
        Err(Error::Unsupported("streaming window function".into()))
    }
    fn name(&self) -> &str;
    fn effects(&self) -> super::FunctionEffects {
        super::FunctionEffects::default()
    }
    /// Argument coercions are selected by the function, before type validation.
    fn argument_types(&self, arguments: &[DataType]) -> Result<Vec<DataType>> {
        Ok(arguments.to_vec())
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        options: WindowOptions,
        types: &TypeRegistry,
    ) -> Result<DataType>;
    fn evaluate(&self, input: &WindowInput<'_>, query: &QueryContext) -> Result<Vec<Value>>;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// State belongs to one cursor. Output owns its values and has the argument
/// batch's cardinality; zero-column arguments still carry input row count.
pub trait StreamingWindowState {
    fn evaluate(
        &mut self,
        arguments: &crate::common::vector::DataChunk,
        query: &QueryContext,
    ) -> Result<crate::common::vector::Vector>;
}

mod builtin;
pub(super) use builtin::register;

#[derive(Debug)]
pub(super) struct AggregateWindow(pub Arc<dyn AggregateFunction>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl WindowFunction for AggregateWindow {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn return_type(
        &self,
        args: &[DataType],
        options: WindowOptions,
        types: &TypeRegistry,
    ) -> Result<DataType> {
        if options.null_treatment.is_some() {
            return Err(Error::Bind(
                "RESPECT/IGNORE NULLS is not supported for windowed aggregates".into(),
            ));
        }
        if options.distinct && args.is_empty() {
            return Err(Error::Bind("DISTINCT requires an argument".into()));
        }
        self.0.return_type(args, types)
    }
    fn evaluate(&self, input: &WindowInput<'_>, query: &QueryContext) -> Result<Vec<Value>> {
        if let Some(values) = self.0.evaluate_window(input, query)? {
            return Ok(values);
        }
        let types = input
            .argument_types
            .iter()
            .map(|t| query.types().bind(t))
            .collect::<Result<Vec<_>>>()?;
        let keys = if input.options.distinct {
            input
                .arguments
                .iter()
                .map(|row| {
                    let mut key = Vec::new();
                    for (value, data_type) in row.iter().zip(&types) {
                        data_type.append_key(value, &mut key, query)?;
                    }
                    Ok(key)
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        let mut result = Vec::with_capacity(input.arguments.len());
        let mut previous: Option<(Range<usize>, Value)> = None;
        for frame in input.frames.iter() {
            query.check()?;
            if let Some((bounds, value)) = &previous
                && bounds == frame
            {
                result.push(value.clone());
                continue;
            }
            let mut state = self.0.create_state(input.argument_types, query.types())?;
            let mut seen = HashSet::new();
            for index in frame.clone() {
                query.check()?;
                if input.filter[index] && (!input.options.distinct || seen.insert(&keys[index])) {
                    state.update(&input.arguments[index], query)?;
                }
            }
            let value = state.finish()?;
            previous = Some((frame.clone(), value.clone()));
            result.push(value);
        }
        Ok(result)
    }
}
