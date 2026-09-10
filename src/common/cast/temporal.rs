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
        if *source == DataType::Date && target.timestamp_precision().is_some() {
            return true;
        }
        use DataType::*;
        // This is the pinned core matrix, not a transitive graph: e.g. NS->S
        // and TZ->DATE do not become casts merely because an intermediate exists.
        matches!(
            (source, target),
            (Time, TimeNs | TimeTz)
                | (TimeNs | TimeTz, Time)
                | (
                    Timestamp,
                    Date | Time
                        | TimeTz
                        | TimestampTz
                        | TimestampNs
                        | TimestampTzNs
                        | TimestampS
                        | TimestampMs
                )
                | (
                    TimestampTz,
                    TimeTz | Timestamp | TimestampNs | TimestampS | TimestampMs
                )
                | (TimestampTzNs, TimeTz | TimestampNs | TimestampTz)
                | (
                    TimestampNs,
                    Date | Time | TimeNs | Timestamp | TimestampTz | TimestampMs
                )
                | (
                    TimestampS,
                    Date | Time
                        | TimestampMs
                        | Timestamp
                        | TimestampTz
                        | TimestampNs
                        | TimestampTzNs
                )
                | (
                    TimestampMs,
                    Date | Time
                        | TimestampS
                        | Timestamp
                        | TimestampTz
                        | TimestampNs
                        | TimestampTzNs
                )
        )
    }
    fn cast(&self, value: &Value, spec: &CastSpec, context: &QueryContext) -> Result<Value> {
        context.check()?;
        let target = &spec.target;
        if let Value::Varchar(text) = value {
            return TemporalValue::parse_checked(text, target, &mut || context.check())
                .map(Value::Temporal);
        }
        if *target == DataType::Varchar {
            return Ok(Value::Varchar(value.as_temporal()?.to_string()));
        }
        if let Value::Date(date) = value {
            if matches!(target, DataType::TimestampS | DataType::TimestampMs) {
                let intermediate = CastSpec {
                    source: DataType::Date,
                    target: DataType::Timestamp,
                    mode: spec.mode,
                };
                return self
                    .cast(value, &intermediate, context)?
                    .as_temporal()?
                    .scale_timestamp(target)
                    .map(Value::Temporal);
            }
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
        let mut value = value.as_temporal()?;
        if target.timestamp_precision().is_some() {
            return value.scale_timestamp(target).map(Value::Temporal);
        }
        if let TemporalValue::TimeTz { micros, .. } = value {
            return TemporalValue::from_ticks(target, micros).map(Value::Temporal);
        }
        if value.data_type().timestamp_precision().is_some() && *target != DataType::TimeNs {
            value = value.scale_timestamp(&DataType::Timestamp)?;
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
        let ticks = if value.data_type().timestamp_precision().is_some() {
            value.ticks()?.rem_euclid(source_precision * 86400)
        } else {
            value.ticks()?
        };
        let ticks = if spec.source == DataType::TimeNs && *target == DataType::Time {
            // Unlike timestamp casts, clock precision reduction rounds to nearest.
            (i128::from(ticks) + 500) / 1000
        } else {
            i128::from(ticks) * precision / i128::from(source_precision)
        };
        if *target == DataType::TimeTz {
            Ok(Value::Temporal(TemporalValue::TimeTz {
                micros: ticks as i64,
                offset: 0,
            }))
        } else {
            TemporalValue::from_ticks(target, ticks as i64).map(Value::Temporal)
        }
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
