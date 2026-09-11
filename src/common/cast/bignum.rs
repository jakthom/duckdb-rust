use std::sync::Arc;

use super::{
    CastBehavior, CastFailure, CastFunction, CastMode, CastRegistry, CastResult, CastSpec,
};
use crate::{
    common::{BignumValue, DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
pub struct BignumCast;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for BignumCast {
    fn name(&self) -> &'static str {
        "magnitude-limb-bignum-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Null
            || spec.source == spec.target
            || spec.mode != CastMode::Implicit
            || (spec.target == DataType::Bignum
                && (spec.source.is_integer() || spec.source.is_floating()))
            || (spec.source == DataType::Bignum && spec.target == DataType::Double)
    }
    fn coercion_cost(&self, spec: &CastSpec) -> u32 {
        if spec.target == DataType::Double {
            104
        } else if spec.target == DataType::Bignum {
            106
        } else {
            110
        }
    }
    fn is_total(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Null
            || spec.source == spec.target
            || (spec.target == DataType::Bignum && spec.source.is_integer())
    }
    fn cast_attempt(
        &self,
        value: &Value,
        spec: &CastSpec,
        behavior: CastBehavior,
        query: &QueryContext,
    ) -> CastResult<Value> {
        let result = self.cast(value, spec, query);
        // Development's numeric BIGNUM output loops throw instead of reporting
        // a failed conversion. TRY_CAST advertises infallibility, so its scalar
        // executor exposes Internal rather than recovering these local errors.
        if behavior == CastBehavior::Try
            && spec.source == DataType::Bignum
            && (spec.target.is_integer() || spec.target == DataType::Double)
        {
            match result {
                Err(error @ (Error::Conversion(_) | Error::OutOfRange(_))) => {
                    return Err(CastFailure::fatal(Error::Internal(format!(
                        "Scalar function \"__cast\" threw an execution error, but the function is not marked as fallible - the function must call SetFallible(). Error: {error}"
                    ))));
                }
                other => return other.map_err(CastFailure::from),
            }
        }
        result.map_err(CastFailure::from)
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if value.is_null() || spec.source == spec.target {
            return Ok(value.clone());
        }
        let unsupported = || {
            Error::Conversion(format!(
                "Unimplemented type for cast ({} -> {})",
                spec.source, spec.target
            ))
        };
        if spec.target == DataType::Bignum {
            return Ok(match value {
                Value::Integer(value) => BignumValue::from_i128(*value),
                Value::Unsigned(value) => BignumValue::from_u128(*value),
                Value::Float(value) => BignumValue::from_f64(f64::from(*value))?,
                Value::Double(value) => BignumValue::from_f64(*value)?,
                Value::Varchar(value) => BignumValue::parse(value, || query.check())?,
                _ => return Err(unsupported()),
            }
            .value());
        }
        let Value::Bignum(value) = value else {
            return Err(unsupported());
        };
        match spec.target {
            DataType::Varchar => value.to_decimal(|| query.check()).map(Value::Varchar),
            DataType::Double => value.to_f64(|| query.check()).map(Value::Double),
            _ if spec.target.is_integer() => {
                let word = value.low_u128();
                let width = spec
                    .target
                    .integer_bits()
                    .or_else(|| spec.target.unsigned_bits())
                    .unwrap();
                // The pinned C++ custom 128-bit types have unspecialized
                // std::numeric_limits<T>::max()==0 in this cast template. This
                // surprising SQL-observable behavior is deliberately local.
                let maximum = if width == 128 {
                    0
                } else {
                    (1_u128 << (width - u8::from(spec.target.is_signed_integer()))) - 1
                };
                let result = if value.is_negative() {
                    if word > maximum + 1 {
                        return Err(Error::OutOfRange(
                            "Negative bignum too small for type".into(),
                        ));
                    }
                    word.wrapping_neg()
                } else {
                    if word > maximum {
                        return Err(Error::OutOfRange(
                            "Positive bignum too large for type".into(),
                        ));
                    }
                    word
                };
                if spec.target.is_signed_integer() {
                    Ok(Value::Integer(result as i128))
                } else {
                    Ok(Value::Unsigned(if width == 128 {
                        result
                    } else {
                        result & ((1_u128 << width) - 1)
                    }))
                }
            }
            _ => Err(unsupported()),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut CastRegistry) {
    let adapter: Arc<dyn CastFunction> = Arc::new(BignumCast);
    registry
        .register_family("builtin.null", "builtin.bignum", adapter.clone())
        .expect("unique BIGNUM NULL cast");
    registry
        .register_family("builtin.bignum", "builtin.bignum", adapter.clone())
        .expect("unique BIGNUM identity cast");
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
        "bit",
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
            .register_family(&family, "builtin.bignum", adapter.clone())
            .expect("unique BIGNUM source cast");
        registry
            .register_family("builtin.bignum", &family, adapter.clone())
            .expect("unique BIGNUM target cast");
    }
}
