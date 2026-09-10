use std::sync::Arc;

use super::{CastFunction, CastMode, CastRegistry, CastSpec};
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
pub struct EnumCast;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for EnumCast {
    fn name(&self) -> &'static str {
        "ordered-enum-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        if spec.source == DataType::Null || spec.source == spec.target {
            return true;
        }
        matches!(
            (&spec.source, &spec.target),
            (DataType::Enum(_), DataType::Varchar)
        ) || (spec.mode != CastMode::Implicit
            && matches!(
                (&spec.source, &spec.target),
                (DataType::Enum(_) | DataType::Varchar, DataType::Enum(_))
            ))
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if value.is_null() || spec.source == spec.target {
            return Ok(value.clone());
        }
        let label = match value {
            Value::Varchar(label) => label.as_str(),
            Value::Enum(value) => value.label()?,
            _ => return Err(Error::Internal("ENUM cast physical source".into())),
        };
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
    ] {
        registry
            .register_family(source, target, Arc::new(EnumCast))
            .expect("unique ENUM cast family");
    }
}
