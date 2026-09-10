//! Logical metadata is serializable independently of an implementation. A
//! selected adapter supplies the semantics for every registered type family.
pub mod ascii;
mod batch;
pub mod date;
mod key;
pub mod numeric;
pub use key::KeyWriter;

use std::{
    cmp::Ordering,
    collections::BTreeMap,
    fmt::Debug,
    sync::{Arc, OnceLock},
};

use super::{DataType, Error, Result, TypeParameter, Value};
use crate::parallel::QueryContext;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueValidation {
    /// Physical representation alone establishes logical validity.
    Physical,
    /// Payload invariants additionally require the adapter's validator.
    Logical,
}

/// Which non-NULL comparison outcomes satisfy a predicate. NULL never matches.
#[derive(Clone, Copy, Debug)]
pub struct ComparisonPredicate {
    pub less: bool,
    pub equal: bool,
    pub greater: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ComparisonPredicate {
    pub fn matches(self, ordering: Ordering) -> bool {
        match ordering {
            Ordering::Less => self.less,
            Ordering::Equal => self.equal,
            Ordering::Greater => self.greater,
        }
    }
}

/// Equality-key capabilities selected by the type adapter, not inferred by a
/// consumer from a physical type or the adapter's concrete implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRepresentation {
    /// Use `write_key`, including its normalization and failure behavior.
    CanonicalBytes,
    /// Non-NULL keys are the integer payload itself. Equality must be exactly
    /// integer identity and key generation must be total after validation.
    /// Only physical integer types can advertise this capability. Consumers
    /// still validate logical invariants and handle NULLs themselves.
    Integer,
    /// Unsigned payload bits or a decimal coefficient form an injective i128
    /// equality key within the bound logical type. u128 is reinterpreted, not
    /// range-converted. This grants NO SQL ordering or signed arithmetic proof.
    /// Only unsigned/decimal physical types may advertise this capability.
    NumericCoefficient,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl KeyRepresentation {
    pub fn has_integer_keys(self) -> bool {
        matches!(self, Self::Integer | Self::NumericCoefficient)
    }
    /// Extract a compact equality key after physical/logical validation. NULL
    /// remains distinct. The selected capability, not the consumer, defines
    /// the mapping; a byte-key adapter cannot enter this path.
    #[inline]
    pub fn integer_key(self, value: &Value) -> Result<Option<i128>> {
        match (self, value) {
            (Self::Integer | Self::NumericCoefficient, Value::Null) => Ok(None),
            (Self::Integer, Value::Integer(value)) => Ok(Some(*value)),
            (Self::NumericCoefficient, Value::Unsigned(value)) => Ok(Some(*value as i128)),
            (Self::NumericCoefficient, Value::Decimal { value, .. }) => Ok(Some(*value)),
            _ => Err(Error::Internal(
                "value differs from its compact equality-key capability".into(),
            )),
        }
    }
}

#[cfg(kani)]
mod verification {
    use super::*;
    #[kani::proof]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn kani_unsigned_coefficient_keys_preserve_full_width_identity_and_nulls() {
        let a: u128 = kani::any();
        let b: u128 = kani::any();
        let representation = KeyRepresentation::NumericCoefficient;
        let left = representation.integer_key(&Value::Unsigned(a)).unwrap();
        let right = representation.integer_key(&Value::Unsigned(b)).unwrap();
        assert_eq!(left == right, a == b);
        assert_eq!(left.unwrap() as u128, a);
        assert_ne!(left, representation.integer_key(&Value::Null).unwrap());
    }
}

/// Ordering capabilities are independent of equality-key representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderingRepresentation {
    /// Retain the adapter's comparison and failure behavior.
    Comparison,
    /// Non-NULL comparison is exactly signed integer ordering and is total
    /// after logical validation. Consumers own NULL placement and direction.
    SignedInteger,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Immutable, pure type semantics, safe for concurrent use. Implementations
/// validate parameters and non-NULL physical values, return a total ordering,
/// and produce canonical keys: equal values have identical keys and unequal
/// values have different keys within one logical type. Composite framing and
/// NULL handling belong to BoundType. Ordering need not match key byte order.
/// All returned data is owned. Calls are synchronous and must check the context
/// during long work; no I/O, mutation, ambient configuration or external effects
/// are permitted. The query context is for resources/cancellation; value
/// semantics must not depend on which other adapters it happens to carry.
/// Replacement preserves the meaning of serialized metadata and
/// payloads. Missing families/unsupported parameters fail before use.
pub trait TypeAdapter: Debug + Send + Sync {
    fn ordering_representation(&self, _data_type: &DataType) -> OrderingRepresentation {
        OrderingRepresentation::Comparison
    }
    fn key_representation(&self, _data_type: &DataType) -> KeyRepresentation {
        KeyRepresentation::CanonicalBytes
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Logical
    }
    fn name(&self) -> &'static str;
    fn validate_type(&self, data_type: &DataType) -> Result<()>;
    fn validate_value(
        &self,
        data_type: &DataType,
        value: &Value,
        context: &QueryContext,
    ) -> Result<()>;
    /// None declines this pair. Conflicting proposed targets fail binding.
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>>;
    fn compare(
        &self,
        data_type: &DataType,
        left: &Value,
        right: &Value,
        context: &QueryContext,
    ) -> Result<Ordering>;
    /// Compare equal-length validated columns in row order. NULL at either
    /// input produces None; all other pairs produce their scalar ordering.
    /// Output is owned and has exactly the input cardinality. Implementations
    /// observe cancellation; the default uses this adapter's scalar method.
    fn compare_batch(
        &self,
        data_type: &DataType,
        left: &super::vector::Vector,
        right: &super::vector::Vector,
        context: &QueryContext,
    ) -> Result<Vec<Option<Ordering>>> {
        batch::compare_values(left, right, context, |a, b| {
            self.compare(data_type, a, b, context)
        })
    }
    /// Return every matching logical row exactly once in ascending row order.
    /// Inputs have identical bound metadata and cardinality and are validated.
    /// The default retains the selected scalar comparison and error order.
    fn select_comparison(
        &self,
        data_type: &DataType,
        left: &super::vector::Vector,
        right: &super::vector::Vector,
        predicate: ComparisonPredicate,
        context: &QueryContext,
    ) -> Result<Vec<usize>> {
        batch::select_values(left, right, predicate, context, |a, b| {
            self.compare(data_type, a, b, context)
        })
    }
    /// Optional proof that every row has the same predicate outcome. Some
    /// replaces comparison work only after validation; it promises the same
    /// values and no omitted data-dependent errors. None preserves normal
    /// comparison. NULL never matches, including for an all-true proof.
    fn uniform_comparison(
        &self,
        _data_type: &DataType,
        _left: &super::vector::Vector,
        _right: &super::vector::Vector,
        _predicate: ComparisonPredicate,
        _context: &QueryContext,
    ) -> Result<Option<bool>> {
        Ok(None)
    }
    /// Append the canonical bytes of one validated, non-NULL value. The writer
    /// permits appends only and bounds key size; it cannot alter earlier keys.
    /// Errors and cancellation discard this component, including partial writes.
    fn write_key(
        &self,
        data_type: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        context: &QueryContext,
    ) -> Result<()>;
}

/// Retains the adapter selected for a complete type, including parameters.
#[derive(Clone, Debug)]
pub struct BoundType {
    data_type: DataType,
    adapter: Arc<dyn TypeAdapter>,
    validation: ValueValidation,
    key_representation: KeyRepresentation,
    ordering_representation: OrderingRepresentation,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BoundType {
    pub fn ordering_representation(&self) -> OrderingRepresentation {
        self.ordering_representation
    }
    pub fn key_representation(&self) -> KeyRepresentation {
        self.key_representation
    }
    pub fn requires_logical_validation(&self) -> bool {
        self.validation == ValueValidation::Logical
    }
    pub fn data_type(&self) -> &DataType {
        &self.data_type
    }
    pub fn adapter(&self) -> &'static str {
        self.adapter.name()
    }
    pub fn validate(&self, value: &Value, context: &QueryContext) -> Result<()> {
        context.check()?;
        if !value.fits_type(&self.data_type) {
            return Err(Error::Conversion(
                "value differs from its declared physical type".into(),
            ));
        }
        if value.is_null() || self.validation == ValueValidation::Physical {
            return Ok(());
        }
        let result = self.adapter.validate_value(&self.data_type, value, context);
        context.check()?;
        result
    }
    pub fn compare(&self, left: &Value, right: &Value, context: &QueryContext) -> Result<Ordering> {
        self.validate(left, context)?;
        self.validate(right, context)?;
        if left.is_null() || right.is_null() {
            return Err(Error::Internal(
                "NULL ordering belongs to the consuming operator".into(),
            ));
        }
        let result = self.adapter.compare(&self.data_type, left, right, context);
        context.check()?;
        result
    }
}

#[derive(Clone, Debug, Default)]
pub struct TypeRegistry {
    adapters: BTreeMap<String, Arc<dyn TypeAdapter>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeRegistry {
    pub fn builtins() -> Self {
        let mut registry = Self::default();
        for data_type in [
            DataType::Null,
            DataType::Boolean,
            DataType::TinyInt,
            DataType::SmallInt,
            DataType::Integer,
            DataType::BigInt,
            DataType::HugeInt,
            DataType::Float,
            DataType::Double,
            DataType::Varchar,
        ] {
            registry
                .register(data_type.family(), Arc::new(PrimitiveTypes))
                .expect("unique builtin type family");
        }
        registry
            .register(DataType::Date.family(), Arc::new(date::DateType))
            .expect("unique DATE family");
        for data_type in [
            DataType::UTinyInt,
            DataType::USmallInt,
            DataType::UInteger,
            DataType::UBigInt,
            DataType::UHugeInt,
            DataType::Decimal {
                width: 18,
                scale: 3,
            },
        ] {
            registry
                .register(data_type.family(), Arc::new(numeric::ExactNumericTypes))
                .expect("unique numeric family");
        }
        registry
    }
    pub fn register(&mut self, family: &str, adapter: Arc<dyn TypeAdapter>) -> Result<()> {
        validate_name(family)?;
        if self.adapters.contains_key(family) {
            return Err(Error::Catalog("type family is already registered".into()));
        }
        self.adapters.insert(family.to_owned(), adapter);
        Ok(())
    }
    pub fn replace(&mut self, family: &str, adapter: Arc<dyn TypeAdapter>) -> Result<()> {
        if !self.adapters.contains_key(family) {
            return Err(Error::Catalog(
                "cannot replace an unregistered type family".into(),
            ));
        }
        self.adapters.insert(family.to_owned(), adapter);
        Ok(())
    }
    pub fn bind(&self, data_type: &DataType) -> Result<BoundType> {
        check_metadata(data_type)?;
        let adapter = self
            .adapters
            .get(data_type.family())
            .cloned()
            .ok_or_else(|| {
                Error::Unsupported(format!("unregistered type family {}", data_type.family()))
            })?;
        self.validate_children(data_type)?;
        adapter.validate_type(data_type)?;
        let key_representation = adapter.key_representation(data_type);
        if key_representation == KeyRepresentation::Integer && !data_type.is_signed_integer() {
            return Err(Error::Bind(
                "integer equality keys require a physical integer type".into(),
            ));
        }
        if key_representation == KeyRepresentation::NumericCoefficient
            && !data_type.is_unsigned_integer()
            && !data_type.is_decimal()
        {
            return Err(Error::Bind(
                "numeric coefficient keys require unsigned or decimal physical types".into(),
            ));
        }
        let ordering_representation = adapter.ordering_representation(data_type);
        if ordering_representation == OrderingRepresentation::SignedInteger
            && !data_type.is_signed_integer()
        {
            return Err(Error::Bind(
                "signed integer ordering requires a physical integer type".into(),
            ));
        }
        Ok(BoundType {
            data_type: data_type.clone(),
            validation: adapter.value_validation(),
            key_representation,
            ordering_representation,
            adapter,
        })
    }
    fn validate_metadata(
        data_type: &DataType,
        depth: usize,
        nodes: &mut usize,
        bytes: &mut usize,
    ) -> Result<()> {
        *nodes = nodes
            .checked_sub(1)
            .ok_or_else(|| Error::Resource("type metadata exceeds 4096 nodes".into()))?;
        if depth > 64 {
            return Err(Error::Resource("type nesting exceeds 64".into()));
        }
        if let DataType::Decimal { width, scale } = data_type
            && (!(1..=38).contains(width) || scale > width)
        {
            return Err(Error::Bind(
                "DECIMAL requires width 1..38 and scale 0..width".into(),
            ));
        }
        if let DataType::Extension(identity) = data_type {
            let super::TypeIdentity { name, parameters } = identity.as_ref();
            validate_name(name)?;
            *bytes = bytes
                .checked_sub(name.len())
                .ok_or_else(|| Error::Resource("type metadata exceeds 16 MiB".into()))?;
            if name.starts_with("builtin.") {
                return Err(Error::Bind("extension uses a reserved type family".into()));
            }
            if parameters.len() > 1024 {
                return Err(Error::Resource("too many type parameters".into()));
            }
            for parameter in parameters {
                *nodes = nodes
                    .checked_sub(1)
                    .ok_or_else(|| Error::Resource("type metadata exceeds 4096 nodes".into()))?;
                if let TypeParameter::Text(text) = parameter {
                    *bytes = bytes
                        .checked_sub(text.len())
                        .ok_or_else(|| Error::Resource("type metadata exceeds 16 MiB".into()))?;
                }
                if let TypeParameter::Type(child) = parameter {
                    Self::validate_metadata(child, depth + 1, nodes, bytes)?;
                }
            }
        }
        Ok(())
    }
    fn validate_children(&self, data_type: &DataType) -> Result<()> {
        if let DataType::Extension(identity) = data_type {
            for parameter in &identity.parameters {
                if let TypeParameter::Type(child) = parameter {
                    self.adapters
                        .get(child.family())
                        .ok_or_else(|| Error::Unsupported("unregistered child type".into()))?
                        .validate_type(child)?;
                    self.validate_children(child)?;
                }
            }
        }
        Ok(())
    }
    pub fn common_type(&self, left: &DataType, right: &DataType) -> Result<DataType> {
        let a = self.bind(left)?;
        if left == right {
            return Ok(left.clone());
        }
        let b = self.bind(right)?;
        let result = if *right == DataType::Null {
            Some(left.clone())
        } else if *left == DataType::Null {
            Some(right.clone())
        } else {
            let a = a.adapter.common_type(left, right)?;
            let b = b.adapter.common_type(right, left)?;
            match (a, b) {
                (Some(a), Some(b)) if a != b => {
                    return Err(Error::Bind(
                        "type adapters disagree on a common type".into(),
                    ));
                }
                (Some(t), _) | (_, Some(t)) => Some(t),
                _ => None,
            }
        }
        .ok_or_else(|| Error::Bind("types have no common coercion target".into()))?;
        if result != *left && result != *right {
            self.bind(&result)?;
        }
        Ok(result)
    }
    pub fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        self.adapters
            .values()
            .map(|a| a.name())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|name| ("types", name))
            .collect()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn check_metadata(data_type: &DataType) -> Result<()> {
    TypeRegistry::validate_metadata(data_type, 0, &mut 4096, &mut (16 * 1024 * 1024))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 256
        || name.split('.').any(|part| {
            part.is_empty()
                || !part.bytes().enumerate().all(|(i, b)| {
                    b == b'_' || b.is_ascii_lowercase() || (i > 0 && b.is_ascii_digit())
                })
        })
    {
        return Err(Error::Bind(
            "type family must use lowercase identifier components".into(),
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn builtin_types() -> Arc<TypeRegistry> {
    static REGISTRY: OnceLock<Arc<TypeRegistry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Arc::new(TypeRegistry::builtins()))
        .clone()
}

#[derive(Debug)]
pub struct PrimitiveTypes;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for PrimitiveTypes {
    fn ordering_representation(&self, data_type: &DataType) -> OrderingRepresentation {
        if data_type.is_signed_integer() {
            OrderingRepresentation::SignedInteger
        } else {
            OrderingRepresentation::Comparison
        }
    }
    fn key_representation(&self, data_type: &DataType) -> KeyRepresentation {
        if data_type.is_signed_integer() {
            KeyRepresentation::Integer
        } else {
            KeyRepresentation::CanonicalBytes
        }
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn name(&self) -> &'static str {
        "primitive-types"
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        if matches!(data_type, DataType::Date | DataType::Extension(_))
            || data_type.is_unsigned_integer()
            || data_type.is_decimal()
        {
            return Err(Error::Unsupported(
                "type requires a separate adapter".into(),
            ));
        }
        Ok(())
    }
    fn validate_value(&self, _: &DataType, _: &Value, context: &QueryContext) -> Result<()> {
        context.check()
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        Ok(DataType::common(left, right).ok())
    }
    fn compare(
        &self,
        _: &DataType,
        left: &Value,
        right: &Value,
        context: &QueryContext,
    ) -> Result<Ordering> {
        context.check()?;
        left.compare(right)
    }
    fn compare_batch(
        &self,
        data_type: &DataType,
        left: &super::vector::Vector,
        right: &super::vector::Vector,
        context: &QueryContext,
    ) -> Result<Vec<Option<Ordering>>> {
        if data_type.is_signed_integer() {
            batch::compare_values(left, right, context, |a, b| match (a, b) {
                (Value::Integer(a), Value::Integer(b)) => Ok(a.cmp(b)),
                _ => Err(Error::Internal("invalid integer comparison input".into())),
            })
        } else {
            batch::compare_values(left, right, context, Value::compare)
        }
    }
    fn select_comparison(
        &self,
        data_type: &DataType,
        left: &super::vector::Vector,
        right: &super::vector::Vector,
        predicate: ComparisonPredicate,
        context: &QueryContext,
    ) -> Result<Vec<usize>> {
        if data_type.is_signed_integer() {
            batch::select_values(left, right, predicate, context, |a, b| match (a, b) {
                (Value::Integer(a), Value::Integer(b)) => Ok(a.cmp(b)),
                _ => Err(Error::Internal("invalid integer comparison input".into())),
            })
        } else {
            batch::select_values(left, right, predicate, context, Value::compare)
        }
    }
    fn write_key(
        &self,
        _: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        context: &QueryContext,
    ) -> Result<()> {
        context.check()?;
        value.append_primitive_key(output)
    }
}
