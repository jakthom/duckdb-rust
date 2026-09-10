use std::sync::Arc;

use super::{BoundCast, CastFunction, CastMode, CastRegistry, CastSpec};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug, Default)]
pub struct EnumCast {
    tail: Option<BoundCast>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for EnumCast {
    fn name(&self) -> &'static str {
        "ordered-enum-cast"
    }
    fn is_total(&self, spec: &CastSpec) -> bool {
        spec.source == spec.target
            || spec.source == DataType::Null
            || matches!(
                (&spec.source, &spec.target),
                (DataType::Enum(_), DataType::Varchar)
            )
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        if spec.source == DataType::Null || spec.source == spec.target {
            return true;
        }
        matches!(
            (&spec.source, &spec.target),
            (DataType::Enum(_), DataType::Varchar)
        ) || (spec.source.family() == "builtin.enum" && spec.mode != CastMode::Implicit)
            || (spec.mode != CastMode::Implicit && matches!(spec.target, DataType::Enum(_)))
    }
    fn coercion_cost_with_registry(
        &self,
        spec: &CastSpec,
        casts: &CastRegistry,
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Option<u32>> {
        if matches!(spec.source, DataType::Enum(_))
            && !matches!(spec.target, DataType::Enum(_) | DataType::Varchar)
        {
            return casts
                .coercion_cost_with_types(&DataType::Varchar, &spec.target, spec.mode, types)
                .map(|cost| cost.map(|cost| cost.saturating_add(1)));
        }
        Ok(Some(self.coercion_cost(spec)))
    }
    fn bind_cast(
        &self,
        spec: &CastSpec,
        casts: &CastRegistry,
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Option<Arc<dyn CastFunction>>> {
        if matches!(spec.source, DataType::Enum(_))
            && !matches!(spec.target, DataType::Enum(_) | DataType::Varchar)
        {
            return Ok(Some(Arc::new(Self {
                tail: Some(casts.bind(&DataType::Varchar, &spec.target, spec.mode, types)?),
            })));
        }
        Ok(None)
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if value.is_null() || spec.source == spec.target {
            return Ok(value.clone());
        }
        let label = match value {
            Value::Varchar(label) => label.as_str(),
            Value::Enum(value) => value.label()?,
            _ => {
                return Err(Error::Conversion(format!(
                    "Unimplemented type for cast ({} -> {})",
                    spec.source, spec.target
                )));
            }
        };
        if let Some(tail) = &self.tail {
            return tail.apply(&Value::Varchar(label.to_owned()), query);
        }
        match &spec.target {
            DataType::Varchar => Ok(Value::Varchar(label.to_owned())),
            DataType::Enum(metadata) => {
                let ordinal = metadata.ordinal(label).ok_or_else(|| {
                    Error::Conversion(format!(
                        "Could not convert string '{label}' to {}",
                        spec.target
                    ))
                })?;
                Value::enumeration(&spec.target, ordinal)
            }
            _ => Err(Error::Internal("ENUM cast physical target".into())),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut CastRegistry) {
    for (source, target) in [
        ("builtin.null", "builtin.enum"),
        ("builtin.enum", "builtin.enum"),
        ("builtin.varchar", "builtin.enum"),
        ("builtin.enum", "builtin.varchar"),
        ("builtin.union", "builtin.enum"),
    ] {
        registry
            .register_family(source, target, Arc::new(EnumCast::default()))
            .expect("unique ENUM cast family");
    }
    for target in [
        "builtin.boolean",
        "builtin.tinyint",
        "builtin.smallint",
        "builtin.integer",
        "builtin.bigint",
        "builtin.hugeint",
        "builtin.utinyint",
        "builtin.usmallint",
        "builtin.uinteger",
        "builtin.ubigint",
        "builtin.uhugeint",
        "builtin.float",
        "builtin.double",
        "builtin.decimal",
        "builtin.date",
        "builtin.time",
        "builtin.time_ns",
        "builtin.time_tz",
        "builtin.timestamp",
        "builtin.timestamp_s",
        "builtin.timestamp_ms",
        "builtin.timestamp_ns",
        "builtin.timestamp_tz",
        "builtin.timestamp_tz_ns",
        "builtin.interval",
        "builtin.blob",
        "builtin.bit",
        "builtin.uuid",
        "builtin.list",
        "builtin.array",
        "builtin.struct",
        "builtin.map",
    ] {
        registry
            .register_family("builtin.enum", target, Arc::new(EnumCast::default()))
            .expect("unique ENUM chained cast family");
        registry
            .register_family(target, "builtin.enum", Arc::new(EnumCast::default()))
            .expect("unique checked unsupported ENUM source family");
    }
}
