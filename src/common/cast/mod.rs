//! Selected, owned conversions between declared logical types. A bound cast
//! retains its adapter; changing a registry does not change existing plans.
pub mod bignum;
pub mod bit;
mod builtin;
mod date;
pub mod enumeration;
mod failure;
mod floating_text;
mod integer;
mod nested;
pub mod numeric;
pub mod scalar;
pub mod temporal;

pub use date::DateCast;
pub use failure::{CastBehavior, CastFailure, CastResult, CastSourceContext};

pub use integer::DigitIntegerCast;

use std::{
    collections::BTreeMap,
    fmt::Debug,
    sync::{Arc, OnceLock},
};

use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CastMode {
    Implicit,
    Assignment,
    Explicit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastNullHandling {
    /// Ordinary casts preserve NULL without calling the adapter.
    Propagate,
    /// Typed NULL is significant input, e.g. an active NULL UNION member.
    /// This does not grant permission to turn non-NULL input into NULL.
    Call,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CastSpec {
    pub source: DataType,
    pub target: DataType,
    pub mode: CastMode,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Pure, deterministic, synchronous conversion of a physical value. Ordinary
/// casts receive non-NULL input. A selected Call capability also receives NULL.
/// The input fits `spec.source`; output fits `spec.target`, and a non-NULL input
/// must remain non-NULL unless the selected `may_return_null` capability says
/// otherwise (for example, extracting a NULL active UNION child into VARIANT).
/// Invalid values normally return Conversion. Configuration/unsupported pairs are
/// rejected at binding. cast_attempt may explicitly distinguish other local
/// data errors from fatal failures. Cancellation, resource failures and invalid
/// adapter output must never be disguised as invalid input, even for TRY_CAST.
/// Adapters own retained configuration, support concurrent callers, do no I/O,
/// observe the query context during long work, and return owned values. A
/// replacement promises the same semantics for each registered specification.
pub trait CastFunction: Debug + Send + Sync {
    fn null_handling(&self, _spec: &CastSpec) -> CastNullHandling {
        CastNullHandling::Propagate
    }
    /// Whether valid non-NULL input may produce SQL NULL. This describes cast
    /// semantics, not error suppression: invalid output and fatal failures
    /// remain errors, including under TRY_CAST. Independent of input NULL
    /// handling; default casts must preserve non-NULL input validity.
    fn may_return_null(&self, _spec: &CastSpec) -> bool {
        false
    }
    fn name(&self) -> &'static str;
    fn supports(&self, spec: &CastSpec) -> bool;
    /// Bind composite conversions through the selected registries, retaining
    /// child casts instead of looking them up again during row execution.
    /// None keeps this adapter; errors reject the cast without a fallback.
    fn bind_cast(
        &self,
        _spec: &CastSpec,
        _casts: &CastRegistry,
        _types: &super::type_registry::TypeRegistry,
    ) -> Result<Option<Arc<dyn CastFunction>>> {
        Ok(None)
    }
    /// Proves absence of data-dependent errors for every valid source value.
    /// Cancellation and resource failures remain possible. False means unknown.
    fn is_total(&self, _spec: &CastSpec) -> bool {
        false
    }
    /// Opt-in value identity for an integer conversion. Together with totality
    /// and compatible physical widths this permits fusing a comparison without
    /// materializing the converted column. False makes no such promise.
    fn preserves_integer_value(&self, _spec: &CastSpec) -> bool {
        false
    }
    /// Opt in to transferring an owned value for a same-type identity cast.
    /// Every valid input must be returned unchanged without data-dependent
    /// failure. BoundCast still validates both logical boundaries and checks
    /// cancellation. Replacements default to the ordinary selected callback.
    fn permits_owned_identity(&self, _spec: &CastSpec) -> bool {
        false
    }
    /// Convert a validated column in logical row order, retaining the selected
    /// NULL handling. Call adapters receive typed NULLs as scalar inputs too.
    /// Output owns exactly the source cardinality and has the declared target
    /// type. The default retains this adapter's scalar conversion semantics.
    fn cast_batch(
        &self,
        input: &super::vector::Vector,
        spec: &CastSpec,
        context: &QueryContext,
    ) -> Result<super::vector::Vector> {
        let propagate_nulls = self.null_handling(spec) == CastNullHandling::Propagate;
        let cast = |value: &Value| {
            if value.is_null() && propagate_nulls {
                Ok(Value::Null)
            } else {
                self.cast(value, spec, context)
            }
        };
        if let Some(value) = input.constant_value() {
            return super::vector::Vector::constant(spec.target.clone(), cast(value)?, input.len());
        }
        if let Some((parent, selection)) = input.dictionary()
            && parent.len() <= input.len() / 4
        {
            let mut entries = vec![usize::MAX; parent.len()];
            let mut values = Vec::with_capacity(parent.len());
            let mut mapped = Vec::with_capacity(selection.len());
            for (offset, &source) in selection.iter().enumerate() {
                if offset % 1024 == 0 {
                    context.check()?;
                }
                if entries[source] == usize::MAX {
                    entries[source] = values.len();
                    let value = parent.get(source).expect("validated dictionary index");
                    values.push(cast(&value)?);
                }
                mapped.push(entries[source]);
            }
            context.check()?;
            return Arc::new(super::vector::Vector::flat(spec.target.clone(), values)?)
                .select(mapped);
        }
        let values = input
            .values()
            .enumerate()
            .map(|(index, value)| {
                if index % 1024 == 0 {
                    context.check()?;
                }
                cast(&value)
            })
            .collect::<Result<Vec<_>>>()?;
        context.check()?;
        super::vector::Vector::flat(spec.target.clone(), values)
    }
    /// Overload ranking only: this never grants an unavailable conversion.
    /// Identity has cost zero at the registry boundary. Replacements may
    /// explicitly supply a different resolution policy while preserving casts.
    fn coercion_cost(&self, spec: &CastSpec) -> u32 {
        match spec.target {
            DataType::BigInt => 101,
            DataType::Integer => 102,
            DataType::HugeInt => 103,
            DataType::Double => 104,
            DataType::Decimal { .. } => 105,
            DataType::TimestampNs => 119,
            DataType::Timestamp => 120,
            DataType::TimestampMs => 121,
            DataType::TimestampS => 122,
            DataType::TimestampTz => 123,
            DataType::TimestampTzNs => 124,
            DataType::Varchar => 149,
            _ => 110,
        }
    }
    /// Binding-only composite overload ranking. None declines a conversion
    /// whose required child casts are unavailable in this composition. Errors
    /// remain errors; callers must not silently fall back to another adapter.
    /// Composite factories should recurse through `coercion_cost_with_types`
    /// on their child signatures, not infer availability from shape alone.
    fn coercion_cost_with_registry(
        &self,
        spec: &CastSpec,
        _casts: &CastRegistry,
        _types: &super::type_registry::TypeRegistry,
    ) -> Result<Option<u32>> {
        Ok(Some(self.coercion_cost(spec)))
    }
    fn cast(&self, value: &Value, spec: &CastSpec, context: &QueryContext) -> Result<Value>;
    /// A selected conversion attempt, preserving failure origin. Leaf adapters
    /// keep their ordinary Conversion contract by default. A family may mark
    /// its own InvalidInput/OutOfRange conversion failures explicitly. It must
    /// not reclassify errors from child validation or infrastructure. Composite
    /// adapters call BoundCast::attempt and propagate CastFailure unchanged;
    /// behavior determines whether failed children become NULL or reject the
    /// entire enclosing value. Ordinary cast() still exposes the original Error.
    fn cast_attempt(
        &self,
        value: &Value,
        spec: &CastSpec,
        _behavior: CastBehavior,
        context: &QueryContext,
    ) -> CastResult<Value> {
        self.cast(value, spec, context).map_err(CastFailure::from)
    }
    /// Context-sensitive input policy on this retained selected adapter.
    /// Existing adapters preserve their ordinary conversion by default; an
    /// opt-in must retain failure provenance and cannot bypass validation.
    fn cast_attempt_with_context(
        &self,
        value: &Value,
        spec: &CastSpec,
        behavior: CastBehavior,
        _source_context: CastSourceContext,
        context: &QueryContext,
    ) -> CastResult<Value> {
        self.cast_attempt(value, spec, behavior, context)
    }
}

#[derive(Clone, Debug)]
pub struct BoundCast {
    spec: CastSpec,
    source: super::type_registry::BoundType,
    target: super::type_registry::BoundType,
    function: Arc<dyn CastFunction>,
    null_handling: CastNullHandling,
    may_return_null: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BoundCast {
    /// Validate and retain the source physical lane for a selected, total,
    /// value-preserving signed widening into BIGINT. Consumers still perform
    /// their own bounded cancellation while reading the retained rows.
    pub(crate) fn can_borrow_signed_bigint(
        &self,
        input: &super::vector::Vector,
        context: &QueryContext,
    ) -> Result<bool> {
        let eligible = self.function.preserves_integer_value(&self.spec)
            && self.is_total()
            && !self.may_return_null
            && self.null_handling == CastNullHandling::Propagate
            && self
                .spec
                .source
                .integer_bits()
                .is_some_and(|bits| bits <= 64)
            && self.spec.target == DataType::BigInt;
        if !eligible {
            return Ok(false);
        }
        self.source.validate_vector(input, context)?;
        context.check()?;
        Ok(true)
    }

    /// Optional checked fusion for unsigned widening into a selected native
    /// signed comparison. Logical target validators and custom ordering always
    /// retain the full conversion/comparison path.
    pub fn select_integer_comparison(
        &self,
        input: &super::vector::Vector,
        right: &Value,
        predicate: super::type_registry::ComparisonPredicate,
        comparison: &super::type_registry::BoundType,
        context: &QueryContext,
    ) -> Result<Option<Vec<usize>>> {
        if !self.function.preserves_integer_value(&self.spec)
            || !self.is_total()
            || self.may_return_null
            || self.null_handling != CastNullHandling::Propagate
            || !self.spec.source.unsigned_bits().is_some_and(|bits| {
                self.spec
                    .target
                    .integer_bits()
                    .is_some_and(|target| target > bits)
            })
            || comparison.data_type() != &self.spec.target
            || comparison.requires_logical_validation()
            || self.target.requires_logical_validation()
            || comparison.ordering_representation()
                != super::type_registry::OrderingRepresentation::SignedInteger
        {
            return Ok(None);
        }
        self.source.validate_vector(input, context)?;
        comparison.validate(right, context)?;
        let Value::Integer(right) = right else {
            return Ok(Some(Vec::new()));
        };
        let mut selected = Vec::with_capacity(input.len());
        if let Some((dictionary, indices)) = input.dictionary()
            && dictionary.len() <= input.len() / 4
        {
            let accepted = dictionary
                .values()
                .enumerate()
                .map(|(index, value)| {
                    if index % 1024 == 0 {
                        context.check()?;
                    }
                    Ok(match value {
                        Value::Unsigned(value) => predicate.matches((value as i128).cmp(right)),
                        Value::Null => false,
                        _ => unreachable!("validated unsigned dictionary"),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            for (row, &index) in indices.iter().enumerate() {
                if row % 1024 == 0 {
                    context.check()?;
                }
                if accepted[index] {
                    selected.push(row);
                }
            }
            context.check()?;
            return Ok(Some(selected));
        }
        let mut visit = |(index, value): (usize, Value)| -> Result<()> {
            if index % 1024 == 0 {
                context.check()?;
            }
            match value {
                Value::Unsigned(value) if predicate.matches((value as i128).cmp(right)) => {
                    selected.push(index)
                }
                Value::Unsigned(_) | Value::Null => (),
                _ => unreachable!("validated unsigned comparison input"),
            }
            Ok(())
        };
        if let Some(values) = input.flat_values() {
            values
                .iter()
                .cloned()
                .enumerate()
                .try_for_each(&mut visit)?;
        } else {
            input.values().enumerate().try_for_each(&mut visit)?;
        }
        context.check()?;
        Ok(Some(selected))
    }
    pub fn is_total(&self) -> bool {
        self.function.is_total(&self.spec)
    }
    pub fn apply_batch(
        &self,
        input: &super::vector::Vector,
        context: &QueryContext,
    ) -> Result<super::vector::Vector> {
        self.source.validate_vector(input, context)?;
        if self.spec.source == self.spec.target {
            // An explicit same-type cast is representation preserving. The
            // source validation is also the target validation, so retain the
            // input encoding rather than rebuilding every logical row.
            return Ok(input.clone());
        }
        let output = self.function.cast_batch(input, &self.spec, context);
        context.check()?;
        let output = output?;
        if output.data_type() != &self.spec.target || output.len() != input.len() {
            return Err(Error::Internal(
                "cast batch differs from its target type or cardinality".into(),
            ));
        }
        if !(input.all_valid() && output.all_valid()) {
            for (index, (a, b)) in input.values().zip(output.values()).enumerate() {
                if index % 1024 == 0 {
                    context.check()?;
                }
                if (!a.is_null() && b.is_null() && !self.may_return_null)
                    || (self.null_handling == CastNullHandling::Propagate
                        && a.is_null()
                        && !b.is_null())
                {
                    return Err(Error::Internal("cast batch changed NULL semantics".into()));
                }
            }
        }
        self.target
            .validate_vector(&output, context)
            .map_err(|error| match error {
                Error::Conversion(_) => {
                    Error::Internal("cast adapter returned an invalid logical value".into())
                }
                other => other,
            })?;
        Ok(output)
    }
    pub fn spec(&self) -> &CastSpec {
        &self.spec
    }
    pub fn adapter(&self) -> &'static str {
        self.function.name()
    }
    pub fn apply(&self, value: &Value, context: &QueryContext) -> Result<Value> {
        self.attempt(value, CastBehavior::Strict, context)
            .map_err(CastFailure::into_error)
    }
    /// Permit retaining validated UTF-8 storage without synthesizing scalar
    /// strings. The caller still owns physical validation and bounded query
    /// cancellation; selected logical validators or non-identity callbacks
    /// must continue through the ordinary scalar conversion boundary.
    pub(crate) fn can_preserve_plain_varchar_storage(&self) -> bool {
        self.spec.source == DataType::Varchar
            && self.spec.target == DataType::Varchar
            && self.null_handling == CastNullHandling::Propagate
            && !self.may_return_null
            && !self.source.requires_logical_validation()
            && !self.target.requires_logical_validation()
            && self.function.permits_owned_identity(&self.spec)
    }
    /// Consume an input whose caller no longer needs its payload. Only an
    /// explicitly opted-in selected identity can avoid the ordinary callback.
    pub fn apply_owned(&self, value: Value, context: &QueryContext) -> Result<Value> {
        if self.spec.source != self.spec.target
            || self.null_handling != CastNullHandling::Propagate
            || self.may_return_null
            || !self.function.permits_owned_identity(&self.spec)
        {
            return self.apply(&value, context);
        }
        context.check()?;
        if !value.fits_type(&self.spec.source) {
            return Err(Error::Internal(
                "cast input differs from its bound source type".into(),
            ));
        }
        if self.source.requires_logical_validation() {
            self.source.validate(&value, context)?;
        }
        if self.target.requires_logical_validation() {
            self.target
                .validate(&value, context)
                .map_err(|error| match error {
                    Error::Conversion(_) => {
                        Error::Internal("cast adapter returned an invalid logical value".into())
                    }
                    other => other,
                })?;
        }
        context.check()?;
        Ok(value)
    }
    pub fn apply_try(&self, value: &Value, context: &QueryContext) -> Result<Value> {
        self.attempt(value, CastBehavior::Try, context)
            .map_err(CastFailure::into_error)
    }
    pub fn attempt(
        &self,
        value: &Value,
        behavior: CastBehavior,
        context: &QueryContext,
    ) -> CastResult<Value> {
        self.attempt_with_context(value, behavior, CastSourceContext::Ordinary, context)
    }
    pub fn attempt_with_context(
        &self,
        value: &Value,
        behavior: CastBehavior,
        source_context: CastSourceContext,
        context: &QueryContext,
    ) -> CastResult<Value> {
        context.check()?;
        if !value.fits_type(&self.spec.source) {
            return Err(CastFailure::fatal(Error::Internal(
                "cast input differs from its bound source type".into(),
            )));
        }
        if self.source.requires_logical_validation() {
            self.source
                .validate(value, context)
                .map_err(CastFailure::fatal)?;
        }
        if value.is_null() && self.null_handling == CastNullHandling::Propagate {
            return Ok(Value::Null);
        }
        // Ordinary casts retain their original selected entry point. Only
        // extracted-source policy opts into the new adapter hook.
        let output = if source_context == CastSourceContext::Ordinary {
            self.function
                .cast_attempt(value, &self.spec, behavior, context)
        } else {
            self.function.cast_attempt_with_context(
                value,
                &self.spec,
                behavior,
                source_context,
                context,
            )
        };
        context.check()?;
        let output = match output {
            Err(error) if behavior == CastBehavior::Try && error.is_invalid_input() => {
                return Ok(Value::Null);
            }
            other => other?,
        };
        if (output.is_null() && !value.is_null() && !self.may_return_null)
            || !output.fits_type(&self.spec.target)
        {
            return Err(CastFailure::fatal(Error::Internal(format!(
                "cast adapter {} returned an invalid physical value for {}",
                self.adapter(),
                self.spec.target
            ))));
        }
        if self.target.requires_logical_validation() {
            self.target
                .validate(&output, context)
                .map_err(|error| match error {
                    Error::Conversion(_) => {
                        Error::Internal("cast adapter returned an invalid logical value".into())
                    }
                    other => other,
                })
                .map_err(CastFailure::fatal)?;
        }
        Ok(output)
    }
}

/// Startup composition with exact pair/mode selection. There is no implicit
/// search order or fallback to a built-in adapter. Missing pairs fail binding.
#[derive(Clone, Debug, Default)]
pub struct CastRegistry {
    functions: BTreeMap<CastSpec, Arc<dyn CastFunction>>,
    families: BTreeMap<String, BTreeMap<String, Arc<dyn CastFunction>>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastRegistry {
    /// Register NULL and identity conversions for one resolved type. Family
    /// constructors may call this for each supported parameter combination.
    pub fn register_type(
        &mut self,
        data_type: &DataType,
        types: &super::type_registry::TypeRegistry,
    ) -> Result<()> {
        types.bind(data_type)?;
        let mut next = self.clone();
        for source in std::iter::once(DataType::Null)
            .chain((*data_type != DataType::Null).then(|| data_type.clone()))
        {
            for mode in [CastMode::Implicit, CastMode::Assignment, CastMode::Explicit] {
                next.register(
                    CastSpec {
                        source: source.clone(),
                        target: data_type.clone(),
                        mode,
                    },
                    Arc::new(StructuralCast),
                )?;
            }
        }
        *self = next;
        Ok(())
    }
    pub fn builtins() -> Self {
        use DataType::*;
        let mut registry = Self::default();
        let types = [
            Null, Boolean, TinyInt, SmallInt, Integer, BigInt, HugeInt, Float, Double, Varchar,
        ];
        let adapter: Arc<dyn CastFunction> = Arc::new(PrimitiveCast);
        for source in &types {
            for target in &types {
                for mode in [CastMode::Implicit, CastMode::Assignment, CastMode::Explicit] {
                    let spec = CastSpec {
                        source: source.clone(),
                        target: target.clone(),
                        mode,
                    };
                    if adapter.supports(&spec) {
                        registry
                            .register(spec, adapter.clone())
                            .expect("unique built-in cast");
                    }
                }
            }
        }
        registry
            .register_type(&Date, &super::type_registry::builtin_types())
            .expect("DATE structural casts");
        for (source, target) in [(Varchar, Date), (Date, Varchar)] {
            for mode in [CastMode::Assignment, CastMode::Explicit] {
                registry
                    .register(
                        CastSpec {
                            source: source.clone(),
                            target: target.clone(),
                            mode,
                        },
                        Arc::new(DateCast),
                    )
                    .expect("unique DATE cast");
            }
        }
        numeric::register(&mut registry);
        enumeration::register(&mut registry);
        temporal::register(&mut registry);
        scalar::register(&mut registry);
        bit::register(&mut registry);
        bignum::register(&mut registry);
        nested::register(&mut registry);
        registry
    }
    /// A selected family adapter resolves parameterized types without expanding
    /// every precision/scale pair. Exact registrations take precedence. Missing
    /// families have no fallback; supports() controls each mode and parameter set.
    pub fn register_family(
        &mut self,
        source: &str,
        target: &str,
        function: Arc<dyn CastFunction>,
    ) -> Result<()> {
        let targets = self.families.entry(source.to_owned()).or_default();
        if targets.contains_key(target) {
            return Err(Error::Bind("cast family is already registered".into()));
        }
        targets.insert(target.to_owned(), function);
        Ok(())
    }
    pub fn replace_family(
        &mut self,
        source: &str,
        target: &str,
        function: Arc<dyn CastFunction>,
    ) -> Result<()> {
        let entry = self
            .families
            .get_mut(source)
            .and_then(|targets| targets.get_mut(target))
            .ok_or_else(|| Error::Bind("cast family is not registered".into()))?;
        *entry = function;
        Ok(())
    }
    fn selected(&self, spec: &CastSpec) -> Option<&Arc<dyn CastFunction>> {
        self.functions.get(spec).or_else(|| {
            self.families
                .get(spec.source.family())
                .and_then(|targets| targets.get(spec.target.family()))
                .filter(|function| function.supports(spec))
        })
    }
    pub fn register(&mut self, spec: CastSpec, function: Arc<dyn CastFunction>) -> Result<()> {
        super::type_registry::check_metadata(&spec.source)?;
        super::type_registry::check_metadata(&spec.target)?;
        if self.functions.contains_key(&spec) {
            return Err(Error::Bind("cast is already registered".into()));
        }
        self.install(spec, function)
    }
    pub fn replace(&mut self, spec: CastSpec, function: Arc<dyn CastFunction>) -> Result<()> {
        super::type_registry::check_metadata(&spec.source)?;
        super::type_registry::check_metadata(&spec.target)?;
        if self.selected(&spec).is_none() {
            return Err(Error::Bind("cannot replace an unregistered cast".into()));
        }
        self.install(spec, function)
    }
    fn install(&mut self, spec: CastSpec, function: Arc<dyn CastFunction>) -> Result<()> {
        if !function.supports(&spec) {
            return Err(Error::Bind(format!(
                "{} does not support {spec:?}",
                function.name()
            )));
        }
        self.functions.insert(spec, function);
        Ok(())
    }
    pub fn bind(
        &self,
        source: &DataType,
        target: &DataType,
        mode: CastMode,
        types: &super::type_registry::TypeRegistry,
    ) -> Result<BoundCast> {
        let source_type = types.bind(source)?;
        let target_type = types.bind(target)?;
        let spec = CastSpec {
            source: source.clone(),
            target: target.clone(),
            mode,
        };
        let function = self
            .selected(&spec)
            .cloned()
            .ok_or_else(|| Error::Bind(format!("no {mode:?} cast from {source} to {target}")))?;
        let function = match function.bind_cast(&spec, self, types)? {
            Some(bound) => {
                if !bound.supports(&spec) {
                    return Err(Error::Bind(
                        "bound cast does not support its signature".into(),
                    ));
                }
                bound
            }
            None => function,
        };
        Ok(BoundCast {
            null_handling: function.null_handling(&spec),
            may_return_null: function.may_return_null(&spec),
            spec,
            source: source_type,
            target: target_type,
            function,
        })
    }
    pub fn coercion_cost(
        &self,
        source: &DataType,
        target: &DataType,
        mode: CastMode,
    ) -> Option<u32> {
        let spec = CastSpec {
            source: source.clone(),
            target: target.clone(),
            mode,
        };
        self.selected(&spec).map(|function| {
            if source == target {
                0
            } else {
                function.coercion_cost(&spec)
            }
        })
    }
    /// Composition-aware availability and ranking for expression binding.
    /// The legacy metadata-only query above cannot resolve composite children.
    pub fn coercion_cost_with_types(
        &self,
        source: &DataType,
        target: &DataType,
        mode: CastMode,
        types: &super::type_registry::TypeRegistry,
    ) -> Result<Option<u32>> {
        let spec = CastSpec {
            source: source.clone(),
            target: target.clone(),
            mode,
        };
        let Some(function) = self.selected(&spec) else {
            return Ok(None);
        };
        let cost = function.coercion_cost_with_registry(&spec, self, types)?;
        Ok(cost.map(|cost| if source == target { 0 } else { cost }))
    }
    pub fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        let names: std::collections::BTreeSet<_> = self
            .functions
            .values()
            .chain(self.families.values().flat_map(|targets| targets.values()))
            .map(|f| f.name())
            .collect();
        names.into_iter().map(|name| ("casts", name)).collect()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn defaults() -> &'static CastRegistry {
    static REGISTRY: OnceLock<CastRegistry> = OnceLock::new();
    REGISTRY.get_or_init(CastRegistry::builtins)
}

#[derive(Debug)]
pub struct PrimitiveCast;

/// Representation-preserving identity/NULL conversions also apply to registered
/// types. BoundCast validates logical values using the selected type adapter.
#[derive(Debug)]
pub struct StructuralCast;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for StructuralCast {
    fn name(&self) -> &'static str {
        "structural-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == spec.target || spec.source == DataType::Null
    }
    fn cast(&self, value: &Value, _: &CastSpec, context: &QueryContext) -> Result<Value> {
        context.check()?;
        Ok(value.clone())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for PrimitiveCast {
    fn name(&self) -> &'static str {
        "primitive-cast"
    }
    fn permits_owned_identity(&self, spec: &CastSpec) -> bool {
        spec.source == spec.target
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        if matches!(
            spec.source,
            DataType::Date
                | DataType::Blob
                | DataType::Bit
                | DataType::Bignum
                | DataType::Uuid
                | DataType::Enum(_)
                | DataType::Extension(_)
        ) || matches!(
            spec.target,
            DataType::Date
                | DataType::Blob
                | DataType::Bit
                | DataType::Bignum
                | DataType::Uuid
                | DataType::Enum(_)
                | DataType::Extension(_)
        ) || spec.source.is_unsigned_integer()
            || spec.target.is_unsigned_integer()
            || spec.source.is_decimal()
            || spec.target.is_decimal()
        {
            return false;
        }
        if spec.source == spec.target || spec.source == DataType::Null {
            return true;
        }
        if spec.target == DataType::Null {
            return false;
        }
        if spec.mode != CastMode::Implicit {
            return true;
        }
        spec.source.is_numeric()
            && spec.target.is_numeric()
            && ((spec.source.is_integer()
                && spec.target.is_integer()
                && spec.source.integer_bits() < spec.target.integer_bits())
                || (spec.source.is_integer() && spec.target.is_floating())
                || (spec.source == DataType::Float && spec.target == DataType::Double))
    }
    fn is_total(&self, spec: &CastSpec) -> bool {
        spec.source.integer_bits().is_some_and(|source| {
            spec.target
                .integer_bits()
                .is_some_and(|target| target >= source)
        })
    }
    fn preserves_integer_value(&self, spec: &CastSpec) -> bool {
        self.is_total(spec)
    }
    fn cast_batch(
        &self,
        input: &super::vector::Vector,
        spec: &CastSpec,
        context: &QueryContext,
    ) -> Result<super::vector::Vector> {
        // Retain the selected PrimitiveCast identity while avoiding temporary
        // HUGEINT Values for the all-valid BIGINT -> DOUBLE physical lane.
        // Other widths/encodings deliberately use the trait default.
        if spec.source == DataType::BigInt
            && spec.target == DataType::Double
            && input.all_valid()
            && let Some(values) = input.flat_bigints()
        {
            let mut output = Vec::new();
            output
                .try_reserve_exact(values.len())
                .map_err(|_| Error::Resource("cannot allocate DOUBLE column".into()))?;
            for (index, &value) in values.iter().enumerate() {
                if index % 1024 == 0 {
                    context.check()?;
                }
                output.push(value as f64);
            }
            context.check()?;
            return super::vector::Vector::try_doubles(output);
        }
        if let Some(value) = input.constant_value() {
            let value = if value.is_null() {
                Value::Null
            } else {
                self.cast(value, spec, context)?
            };
            return super::vector::Vector::constant(spec.target.clone(), value, input.len());
        }
        if let Some((parent, selection)) = input.dictionary()
            && parent.len() <= input.len() / 4
        {
            let mut entries = vec![usize::MAX; parent.len()];
            let mut values = Vec::with_capacity(parent.len());
            let mut mapped = Vec::with_capacity(selection.len());
            for (offset, &source) in selection.iter().enumerate() {
                if offset % 1024 == 0 {
                    context.check()?;
                }
                if entries[source] == usize::MAX {
                    entries[source] = values.len();
                    let value = parent.get(source).expect("validated dictionary index");
                    values.push(if value.is_null() {
                        Value::Null
                    } else {
                        self.cast(&value, spec, context)?
                    });
                }
                mapped.push(entries[source]);
            }
            context.check()?;
            return Arc::new(super::vector::Vector::flat(spec.target.clone(), values)?)
                .select(mapped);
        }
        let values = input
            .values()
            .enumerate()
            .map(|(index, value)| {
                if index % 1024 == 0 {
                    context.check()?;
                }
                if value.is_null() {
                    Ok(Value::Null)
                } else {
                    self.cast(&value, spec, context)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        context.check()?;
        super::vector::Vector::flat(spec.target.clone(), values)
    }
    fn cast(&self, value: &Value, spec: &CastSpec, context: &QueryContext) -> Result<Value> {
        context.check()?;
        builtin::primitive(value, &spec.target).map_err(|error| match error {
            Error::Conversion(_)
                if (spec.source.is_signed_integer() || spec.source.is_floating())
                    && spec.target.is_signed_integer() =>
            {
                Error::Conversion(format!(
                    "Type {} with value {value} can't be cast because the value is out of range for the destination type {}",
                    primitive_numeric_name(&spec.source), primitive_numeric_name(&spec.target)
                ))
            }
            other => other,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// DuckDB's primitive cast diagnostics name the physical C++ source and target
/// types rather than their SQL aliases (for example, INT128 instead of
/// HUGEINT). PrimitiveCast only calls this for numeric types it owns.
fn primitive_numeric_name(data_type: &DataType) -> &'static str {
    match data_type {
        DataType::TinyInt => "INT8",
        DataType::SmallInt => "INT16",
        DataType::Integer => "INT32",
        DataType::BigInt => "INT64",
        DataType::HugeInt => "INT128",
        DataType::Float => "FLOAT",
        DataType::Double => "DOUBLE",
        _ => unreachable!("primitive cast owns only signed and floating numeric types"),
    }
}

#[cfg(test)]
mod packed_storage_tests {
    use super::*;
    use crate::common::type_registry::{KeyWriter, PrimitiveTypes, TypeAdapter, TypeRegistry};
    use std::cmp::Ordering;

    #[derive(Debug)]
    struct NoIdentity;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl CastFunction for NoIdentity {
        fn name(&self) -> &'static str {
            // A replacement cannot borrow merely by using the built-in name.
            "primitive"
        }
        fn supports(&self, _: &CastSpec) -> bool {
            true
        }
        fn cast(&self, value: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
            Ok(value.clone())
        }
    }

    #[derive(Debug)]
    struct LogicalVarchar;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl TypeAdapter for LogicalVarchar {
        fn name(&self) -> &'static str {
            "logical-varchar-probe"
        }
        fn validate_type(&self, data_type: &DataType) -> Result<()> {
            PrimitiveTypes.validate_type(data_type)
        }
        fn validate_value(
            &self,
            data_type: &DataType,
            value: &Value,
            context: &QueryContext,
        ) -> Result<()> {
            PrimitiveTypes.validate_value(data_type, value, context)
        }
        fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
            PrimitiveTypes.common_type(left, right)
        }
        fn compare(
            &self,
            data_type: &DataType,
            left: &Value,
            right: &Value,
            context: &QueryContext,
        ) -> Result<Ordering> {
            PrimitiveTypes.compare(data_type, left, right, context)
        }
        fn write_key(
            &self,
            data_type: &DataType,
            value: &Value,
            output: &mut KeyWriter<'_>,
            context: &QueryContext,
        ) -> Result<()> {
            PrimitiveTypes.write_key(data_type, value, output, context)
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn packed_varchar_requires_selected_identity_and_physical_only_validation() -> Result<()> {
        let types = TypeRegistry::builtins();
        let casts = CastRegistry::builtins();
        let bound = casts.bind(
            &DataType::Varchar,
            &DataType::Varchar,
            CastMode::Explicit,
            &types,
        )?;
        assert!(bound.can_preserve_plain_varchar_storage());
        let mut changed = bound.clone();
        changed.function = Arc::new(NoIdentity);
        assert!(!changed.can_preserve_plain_varchar_storage());
        let mut changed = bound.clone();
        changed.null_handling = CastNullHandling::Call;
        assert!(!changed.can_preserve_plain_varchar_storage());
        let mut changed = bound.clone();
        changed.may_return_null = true;
        assert!(!changed.can_preserve_plain_varchar_storage());
        let mut logical = types.clone();
        logical.replace(DataType::Varchar.family(), Arc::new(LogicalVarchar))?;
        for source in [true, false] {
            let mut changed = bound.clone();
            if source {
                changed.source = logical.bind(&DataType::Varchar)?;
            } else {
                changed.target = logical.bind(&DataType::Varchar)?;
            }
            assert!(!changed.can_preserve_plain_varchar_storage());
        }
        for (source, target) in [
            (DataType::Varchar, DataType::BigInt),
            (DataType::BigInt, DataType::Varchar),
            (DataType::BigInt, DataType::BigInt),
        ] {
            let other = casts.bind(&source, &target, CastMode::Explicit, &types)?;
            assert!(!other.can_preserve_plain_varchar_storage());
        }
        Ok(())
    }
}
