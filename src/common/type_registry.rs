//! Logical metadata is serializable independently of an implementation. A
//! selected adapter supplies the semantics for every registered type family.
pub mod ascii;
pub mod date;

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
    fn key(&self, data_type: &DataType, value: &Value, context: &QueryContext) -> Result<Vec<u8>>;
}

/// Retains the adapter selected for a complete type, including parameters.
#[derive(Clone, Debug)]
pub struct BoundType {
    data_type: DataType,
    adapter: Arc<dyn TypeAdapter>,
    validation: ValueValidation,
}

impl BoundType {
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
    /// Appends one self-delimiting component. Failure leaves the output intact.
    pub fn append_key(
        &self,
        value: &Value,
        output: &mut Vec<u8>,
        context: &QueryContext,
    ) -> Result<()> {
        self.validate(value, context)?;
        if value.is_null() {
            output.push(0);
            return Ok(());
        }
        let key = self.adapter.key(&self.data_type, value, context);
        context.check()?;
        let key = key?;
        if key.len() > 16 * 1024 * 1024 {
            return Err(Error::Resource("type key exceeds 16 MiB".into()));
        }
        output
            .try_reserve(key.len() + 9)
            .map_err(|_| Error::Resource("cannot allocate composite type key".into()))?;
        output.push(1);
        output.extend((key.len() as u64).to_le_bytes());
        output.extend(key);
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct TypeRegistry {
    adapters: BTreeMap<String, Arc<dyn TypeAdapter>>,
}

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
        Ok(BoundType {
            data_type: data_type.clone(),
            validation: adapter.value_validation(),
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

pub(crate) fn check_metadata(data_type: &DataType) -> Result<()> {
    TypeRegistry::validate_metadata(data_type, 0, &mut 4096, &mut (16 * 1024 * 1024))
}

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

pub fn builtin_types() -> Arc<TypeRegistry> {
    static REGISTRY: OnceLock<Arc<TypeRegistry>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Arc::new(TypeRegistry::builtins()))
        .clone()
}

#[derive(Debug)]
pub struct PrimitiveTypes;
impl TypeAdapter for PrimitiveTypes {
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn name(&self) -> &'static str {
        "primitive-types"
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        if matches!(data_type, DataType::Date | DataType::Extension(_)) {
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
    fn key(&self, _: &DataType, value: &Value, context: &QueryContext) -> Result<Vec<u8>> {
        context.check()?;
        let size = match value {
            Value::Varchar(text) => text.len().checked_add(9),
            _ => Some(17),
        }
        .filter(|&size| size <= 16 * 1024 * 1024)
        .ok_or_else(|| Error::Resource("type key exceeds 16 MiB".into()))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(size)
            .map_err(|_| Error::Resource("cannot allocate primitive type key".into()))?;
        value.append_primitive_key(&mut bytes)?;
        Ok(bytes)
    }
}
