use super::{CastFunction, CastMode, CastRegistry, CastSpec};
use crate::{
    common::{DataType, Error, Result, TemporalValue, Value, temporal::TEMPORAL_TYPES},
    parallel::QueryContext,
};
use std::sync::Arc;

#[derive(Debug)]
pub struct TemporalCast;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for TemporalCast {
    fn name(&self) -> &'static str {
        "calendar-temporal-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        let (source, target) = (&spec.source, &spec.target);
        if spec.mode == CastMode::Implicit {
            return matches!(
                (source, target),
                (
                    DataType::Date,
                    DataType::TimestampS
                        | DataType::TimestampMs
                        | DataType::Timestamp
                        | DataType::TimestampNs
                        | DataType::TimestampTz
                        | DataType::TimestampTzNs
                ) | (
                    DataType::TimestampS,
                    DataType::TimestampMs | DataType::Timestamp | DataType::TimestampNs
                ) | (
                    DataType::TimestampMs,
                    DataType::Timestamp | DataType::TimestampNs
                ) | (
                    DataType::Timestamp,
                    DataType::TimestampNs | DataType::TimestampTz | DataType::TimestampTzNs
                ) | (DataType::TimestampNs, DataType::Timestamp)
            );
        }
        if spec.mode != CastMode::Implicit
            && ((source.is_temporal() && *target == DataType::Varchar)
                || (*source == DataType::Varchar && target.is_temporal()))
        {
            return true;
        }
        if source.timestamp_precision().is_some() && target.timestamp_precision().is_some() {
            return true;
        }
        if *source == DataType::Date && target.timestamp_precision().is_some() {
            return true;
        }
        if spec.mode != CastMode::Implicit
            && source.timestamp_precision().is_some()
            && matches!(target, DataType::Date | DataType::Time | DataType::TimeNs)
        {
            return true;
        }
        matches!(
            (source, target),
            (DataType::Time, DataType::TimeNs) | (DataType::TimeNs, DataType::Time)
        )
    }
    fn cast(&self, value: &Value, spec: &CastSpec, context: &QueryContext) -> Result<Value> {
        context.check()?;
        let target = &spec.target;
        if let Value::Varchar(text) = value {
            return TemporalValue::parse(text, target).map(Value::Temporal);
        }
        if *target == DataType::Varchar {
            return Ok(Value::Varchar(value.as_temporal()?.to_string()));
        }
        if let Value::Date(date) = value {
            let ticks = if !date.is_finite() {
                if *date == crate::common::Date::INFINITY {
                    i64::MAX
                } else {
                    -i64::MAX
                }
            } else {
                i64::try_from(
                    i128::from(date.days())
                        * 86400
                        * i128::from(
                            target
                                .timestamp_precision()
                                .ok_or_else(|| Error::Conversion("expected timestamp".into()))?,
                        ),
                )
                .map_err(|_| Error::Conversion("DATE exceeds timestamp range".into()))?
            };
            return TemporalValue::from_ticks(target, ticks).map(Value::Temporal);
        }
        let value = value.as_temporal()?;
        if target.timestamp_precision().is_some() {
            return value.scale_timestamp(target).map(Value::Temporal);
        }
        if *target == DataType::Date {
            return value.date().map(Value::Date);
        }
        let source_precision =
            value
                .data_type()
                .timestamp_precision()
                .unwrap_or(if spec.source == DataType::TimeNs {
                    1_000_000_000
                } else {
                    1_000_000
                });
        if !value.is_finite() {
            return Err(Error::Conversion("infinite timestamp has no time".into()));
        }
        let precision = if *target == DataType::TimeNs {
            1_000_000_000
        } else {
            1_000_000
        };
        let ticks = i128::from(value.ticks()?.rem_euclid(source_precision * 86400)) * precision
            / i128::from(source_precision);
        TemporalValue::from_ticks(target, ticks as i64).map(Value::Temporal)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut CastRegistry) {
    let types = crate::common::type_registry::builtin_types();
    let adapter: Arc<dyn CastFunction> = Arc::new(TemporalCast);
    for data_type in &TEMPORAL_TYPES {
        registry
            .register_type(data_type, &types)
            .expect("temporal structural casts");
    }
    let all: Vec<_> = TEMPORAL_TYPES
        .into_iter()
        .chain([DataType::Date, DataType::Varchar])
        .collect();
    for source in &all {
        for target in &all {
            if source == target {
                continue;
            }
            for mode in [CastMode::Implicit, CastMode::Assignment, CastMode::Explicit] {
                let spec = CastSpec {
                    source: source.clone(),
                    target: target.clone(),
                    mode,
                };
                if adapter.supports(&spec) {
                    registry
                        .register(spec, adapter.clone())
                        .expect("unique temporal cast");
                }
            }
        }
    }
}
