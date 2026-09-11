use std::sync::Arc;

use super::{CastFunction, CastMode, CastRegistry, CastSpec};
use crate::{
    common::{
        DataType, Error, Result, Value,
        scalar::{parse_blob, parse_uuid},
    },
    parallel::QueryContext,
};

#[derive(Debug)]
pub struct BinaryScalarCast;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for BinaryScalarCast {
    fn name(&self) -> &'static str {
        "binary-scalar-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        if spec.source == DataType::Null || spec.source == spec.target {
            return true;
        }
        spec.mode != CastMode::Implicit
            && matches!(
                (&spec.source, &spec.target),
                (DataType::Varchar, DataType::Blob | DataType::Uuid)
                    | (DataType::Blob | DataType::Uuid, DataType::Varchar)
                    | (DataType::Uuid, DataType::Blob | DataType::UHugeInt)
                    | (DataType::Blob | DataType::UHugeInt, DataType::Uuid)
            )
    }
    fn cast(&self, value: &Value, spec: &CastSpec, context: &QueryContext) -> Result<Value> {
        context.check()?;
        if value.is_null() || spec.source == spec.target {
            return Ok(value.clone());
        }
        match (value, &spec.target) {
            (Value::Varchar(text), DataType::Blob) => {
                parse_blob(text, || context.check()).map(Value::Blob)
            }
            (Value::Varchar(text), DataType::Uuid) => {
                parse_uuid(text, || context.check()).map(Value::Uuid)
            }
            (Value::Blob(_) | Value::Uuid(_), DataType::Varchar) => {
                Ok(Value::Varchar(value.to_string()))
            }
            (Value::Uuid(value), DataType::Blob) => Ok(Value::Blob(value.to_be_bytes().to_vec())),
            (Value::Blob(bytes), DataType::Uuid) => {
                let bytes: [u8; 16] = bytes.as_slice().try_into().map_err(|_| {
                    Error::Conversion("BLOB to UUID requires exactly 16 bytes".into())
                })?;
                Ok(Value::Uuid(u128::from_be_bytes(bytes)))
            }
            (Value::Uuid(value), DataType::UHugeInt) => Ok(Value::Unsigned(*value)),
            (Value::Unsigned(value), DataType::Uuid) => Ok(Value::Uuid(*value)),
            _ => Err(Error::Conversion(
                "unsupported binary scalar conversion".into(),
            )),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut CastRegistry) {
    let adapter: Arc<dyn CastFunction> = Arc::new(BinaryScalarCast);
    for source in [
        DataType::Null,
        DataType::Varchar,
        DataType::Blob,
        DataType::Uuid,
        DataType::UHugeInt,
    ] {
        for target in [
            DataType::Varchar,
            DataType::Blob,
            DataType::Uuid,
            DataType::UHugeInt,
        ] {
            if !matches!(source, DataType::Blob | DataType::Uuid)
                && !matches!(target, DataType::Blob | DataType::Uuid)
            {
                continue;
            }
            registry
                .register_family(source.family(), target.family(), adapter.clone())
                .expect("unique binary cast family");
        }
    }
}
