//! Selected, owned conversions between declared logical types. A bound cast
//! retains its adapter; changing a registry does not change existing plans.
mod builtin;
mod date;
mod integer;
pub mod numeric;

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
    families: BTreeMap<(String, String), Arc<dyn CastFunction>>,
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
        let key = (source.to_owned(), target.to_owned());
        if self.families.contains_key(&key) {
            return Err(Error::Bind("cast family is already registered".into()));
        }
        self.families.insert(key, function);
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
            .get_mut(&(source.to_owned(), target.to_owned()))
            .ok_or_else(|| Error::Bind("cast family is not registered".into()))?;
        *entry = function;
        Ok(())
    }
    fn selected(&self, spec: &CastSpec) -> Option<&Arc<dyn CastFunction>> {
        self.functions.get(spec).or_else(|| {
            self.families
                .get(&(
                    spec.source.family().to_owned(),
                    spec.target.family().to_owned(),
                ))
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
    pub fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        let names: std::collections::BTreeSet<_> = self
            .functions
            .values()
            .chain(self.families.values())
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
        if matches!(spec.source, DataType::Date | DataType::Extension(_))
            || matches!(spec.target, DataType::Date | DataType::Extension(_))
            || spec.source.is_unsigned_integer()
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
