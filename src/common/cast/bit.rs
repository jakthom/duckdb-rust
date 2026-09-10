use std::sync::Arc;

use super::{CastFunction, CastMode, CastRegistry, CastSpec};
use crate::{
    common::{BitString, DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
pub struct BitCast;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for BitCast {
    fn name(&self) -> &'static str {
        "packed-bit-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Null
            || spec.source == spec.target
            || spec.mode != CastMode::Implicit
    }
    fn is_total(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Null || spec.source == spec.target
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if value.is_null() || spec.source == spec.target {
            return Ok(value.clone());
        }
        if spec.target == DataType::Bit {
            let bits = match value {
                Value::Varchar(text) => BitString::parse(text, || query.check())?,
                Value::Blob(bytes) => {
                    if bytes.is_empty() {
                        return Err(Error::Conversion("Cannot cast empty BLOB to BIT".into()));
                    }
                    BitString::from_blob(bytes.clone())?
                }
                _ => {
                    let bytes = match value {
                        Value::Boolean(value) => vec![u8::from(*value)],
                        Value::Integer(value) => value.to_be_bytes()[16
                            - usize::from(
                                spec.source.integer_bits().ok_or_else(|| {
                                    Error::Conversion("BIT signed input width".into())
                                })? / 8,
                            )..]
                            .to_vec(),
                        Value::Unsigned(value) => value.to_be_bytes()[16
                            - usize::from(
                                spec.source.unsigned_bits().ok_or_else(|| {
                                    Error::Conversion("BIT unsigned input width".into())
                                })? / 8,
                            )..]
                            .to_vec(),
                        Value::Float(value) => value.to_bits().to_be_bytes().to_vec(),
                        Value::Double(value) => value.to_bits().to_be_bytes().to_vec(),
                        _ => return Err(Error::Conversion("unsupported cast to BIT".into())),
                    };
                    BitString::from_blob(bytes)?
                }
            };
            return Ok(bits.value());
        }
        let Value::Bit(bits) = value else {
            return Err(Error::Conversion("unsupported BIT source".into()));
        };
        if spec.target == DataType::Varchar {
            return bits.to_text(|| query.check()).map(Value::Varchar);
        }
        let bytes = bits.to_blob(|| query.check())?;
        if spec.target == DataType::Blob {
            return Ok(Value::Blob(bytes));
        }
        let width = match &spec.target {
            DataType::Boolean => 1,
            DataType::Float => 4,
            DataType::Double => 8,
            target if target.is_integer() => {
                usize::from(
                    target
                        .integer_bits()
                        .or_else(|| target.unsigned_bits())
                        .unwrap(),
                ) / 8
            }
            _ => return Err(Error::Conversion("unsupported cast from BIT".into())),
        };
        if bytes.len() > width {
            return Err(Error::Conversion(format!(
                "Bitstring doesn't fit inside of {}",
                spec.target
            )));
        }
        let mut padded = [0_u8; 16];
        padded[16 - bytes.len()..].copy_from_slice(&bytes);
        let word = u128::from_be_bytes(padded);
        Ok(match spec.target {
            DataType::Boolean => Value::Boolean(word != 0),
            DataType::Float => Value::Float(f32::from_bits(word as u32)),
            DataType::Double => Value::Double(f64::from_bits(word as u64)),
            _ if spec.target.is_unsigned_integer() => Value::Unsigned(word),
            _ => Value::Integer(((word as i128) << (128 - width * 8)) >> (128 - width * 8)),
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut CastRegistry) {
    let adapter: Arc<dyn CastFunction> = Arc::new(BitCast);
    registry
        .register_family("builtin.null", "builtin.bit", adapter.clone())
        .expect("unique BIT NULL cast");
    registry
        .register_family("builtin.bit", "builtin.bit", adapter.clone())
        .expect("unique BIT identity cast");
    for family in [
        "boolean",
        "tinyint",
        "smallint",
        "integer",
        "bigint",
        "hugeint",
        "utinyint",
        "usmallint",
        "uinteger",
        "ubigint",
        "uhugeint",
        "float",
        "double",
        "decimal",
        "varchar",
        "blob",
        "uuid",
        "date",
        "time",
        "time_ns",
        "time_tz",
        "timestamp",
        "timestamp_s",
        "timestamp_ms",
        "timestamp_ns",
        "timestamp_tz",
        "timestamp_tz_ns",
        "interval",
    ] {
        let family = format!("builtin.{family}");
        registry
            .register_family(&family, "builtin.bit", adapter.clone())
            .expect("unique BIT source cast");
        registry
            .register_family("builtin.bit", &family, adapter.clone())
            .expect("unique BIT target cast");
    }
}
