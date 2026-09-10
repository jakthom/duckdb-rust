//! Selected, owned conversions between declared logical types. A bound cast
//! retains its adapter; changing a registry does not change existing plans.
mod builtin;
mod date;
mod integer;
pub mod numeric;
pub mod scalar;
pub mod temporal;

pub use date::DateCast;

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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CastSpec {
    pub source: DataType,
    pub target: DataType,
    pub mode: CastMode,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Pure, deterministic, synchronous conversion of a non-NULL physical value.
/// The input fits `spec.source`; output must be non-NULL and fit `spec.target`.
/// Invalid values return Conversion. Configuration/unsupported pairs are
/// rejected at binding. Other errors (including cancellation and resource
/// failures) must never be disguised as invalid input, even for TRY_CAST.
/// Adapters own retained configuration, support concurrent callers, do no I/O,
/// observe the query context during long work, and return owned values. A
/// replacement promises the same semantics for each registered specification.
pub trait CastFunction: Debug + Send + Sync {
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
    /// Convert a validated column in logical row order, preserving NULLs.
    /// Output owns exactly the source cardinality and has the declared target
    /// type. The default retains this adapter's scalar conversion semantics.
    fn cast_batch(
        &self,
        input: &super::vector::Vector,
        spec: &CastSpec,
        context: &QueryContext,
    ) -> Result<super::vector::Vector> {
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
                    self.cast(value, spec, context)
                }
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
}

#[derive(Clone, Debug)]
pub struct BoundCast {
    spec: CastSpec,
    source: super::type_registry::BoundType,
    target: super::type_registry::BoundType,
    function: Arc<dyn CastFunction>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BoundCast {
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
                        Value::Unsigned(value) => predicate.matches((*value as i128).cmp(right)),
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
        let mut visit = |(index, value): (usize, &Value)| -> Result<()> {
            if index % 1024 == 0 {
                context.check()?;
            }
            match value {
                Value::Unsigned(value) if predicate.matches((*value as i128).cmp(right)) => {
                    selected.push(index)
                }
                Value::Unsigned(_) | Value::Null => (),
                _ => unreachable!("validated unsigned comparison input"),
            }
            Ok(())
        };
        if let Some(values) = input.flat_values() {
            values.iter().enumerate().try_for_each(&mut visit)?;
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
                if a.is_null() != b.is_null() {
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
        context.check()?;
        if !value.fits_type(&self.spec.source) {
            return Err(Error::Internal(
                "cast input differs from its bound source type".into(),
            ));
        }
        if self.source.requires_logical_validation() {
            self.source.validate(value, context)?;
        }
        if value.is_null() {
            return Ok(Value::Null);
        }
        let output = self.function.cast(value, &self.spec, context);
        context.check()?;
        let output = output?;
        if output.is_null() || !output.fits_type(&self.spec.target) {
            return Err(Error::Internal(format!(
                "cast adapter {} returned an invalid physical value for {}",
                self.adapter(),
                self.spec.target
            )));
        }
        if self.target.requires_logical_validation() {
            self.target
                .validate(&output, context)
                .map_err(|error| match error {
                    Error::Conversion(_) => {
                        Error::Internal("cast adapter returned an invalid logical value".into())
                    }
                    other => other,
                })?;
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
        temporal::register(&mut registry);
        scalar::register(&mut registry);
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
    fn supports(&self, spec: &CastSpec) -> bool {
        if matches!(
            spec.source,
            DataType::Date | DataType::Blob | DataType::Uuid | DataType::Extension(_)
        ) || matches!(
            spec.target,
            DataType::Date | DataType::Blob | DataType::Uuid | DataType::Extension(_)
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
    fn cast(&self, value: &Value, spec: &CastSpec, context: &QueryContext) -> Result<Value> {
        context.check()?;
        builtin::primitive(value, &spec.target)
    }
}
