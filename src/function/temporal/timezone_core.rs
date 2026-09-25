//! Core fixed-offset timezone extraction and TIMETZ relocation. Named zones,
//! TIMESTAMPTZ and session settings remain owned by an ICU adapter.

use super::*;
use crate::function::ScalarSignature;

const MAX_OFFSET_SECONDS: i64 = 15 * 60 * 60 + 59 * 60 + 59;

#[derive(Clone, Copy, Debug)]
enum Part {
    Timezone,
    Hour,
    Minute,
}

#[derive(Debug)]
struct CoreTimezone {
    name: &'static str,
    part: Part,
    signature: Option<ScalarSignature>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for (name, part) in [
        ("timezone", Part::Timezone),
        ("timezone_hour", Part::Hour),
        ("timezone_minute", Part::Minute),
    ] {
        registry
            .register_scalar(Arc::new(CoreTimezone {
                name,
                part,
                signature: None,
            }))
            .expect("unique fixed-offset timezone function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn candidates(part: Part) -> Vec<ScalarSignature> {
    use DataType::*;
    let mut candidates = [Date, Timestamp, Interval, Time, TimeNs, TimeTz]
        .into_iter()
        .map(|data_type| ScalarSignature {
            arguments: vec![data_type],
            argument_names: None,
            return_type: BigInt,
        })
        .collect::<Vec<_>>();
    if matches!(part, Part::Timezone) {
        candidates.push(ScalarSignature {
            arguments: vec![Interval, TimeTz],
            argument_names: None,
            return_type: TimeTz,
        });
    }
    // Retain extension-owned signatures so a zoned argument cannot select a
    // lossy core cast. Selecting one remains an explicit unsupported boundary.
    candidates.push(ScalarSignature {
        arguments: vec![TimestampTz],
        argument_names: None,
        return_type: BigInt,
    });
    if matches!(part, Part::Timezone) {
        for (right, result) in [
            (Timestamp, TimestampTz),
            (TimestampTz, Timestamp),
            (TimeTz, TimeTz),
        ] {
            candidates.push(ScalarSignature {
                arguments: vec![Varchar, right],
                argument_names: None,
                return_type: result,
            });
        }
    }
    candidates
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn requires_icu(signature: &ScalarSignature) -> bool {
    signature.arguments.contains(&DataType::TimestampTz)
        || signature.arguments.first() == Some(&DataType::Varchar)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CoreTimezone {
    fn signature(&self) -> Result<&ScalarSignature> {
        self.signature.as_ref().ok_or_else(|| {
            Error::Unsupported(format!(
                "{} requires selected statement-local binding",
                self.name
            ))
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for CoreTimezone {
    fn name(&self) -> &str {
        self.name
    }

    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        let candidates = candidates(self.part);
        ScalarSignature::validate_candidates(self.name, &candidates, query)?;
        let selected = arguments.select_overload(self.name, &candidates)?;
        let signature = ScalarSignature::selected(&candidates, selected)?.clone();
        if signature.arguments.len() != arguments.len() {
            return Err(Error::Internal(
                "fixed-offset timezone overload changed argument count".into(),
            ));
        }
        if requires_icu(&signature) {
            return Err(Error::Unsupported(format!(
                "{} with named or timestamp time zones requires a selected ICU timezone adapter",
                self.name
            )));
        }
        Ok(Some(Arc::new(Self {
            name: self.name,
            part: self.part,
            signature: Some(signature),
        })))
    }

    fn argument_types(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        let signature = self.signature()?;
        if arguments.len() != signature.arguments.len() {
            return Err(Error::Bind(format!(
                "{} requires {} argument{}",
                self.name,
                signature.arguments.len(),
                if signature.arguments.len() == 1 {
                    ""
                } else {
                    "s"
                }
            )));
        }
        Ok(signature.arguments.clone())
    }

    fn return_type(
        &self,
        arguments: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        let signature = self.signature()?;
        if arguments != signature.arguments {
            return Err(Error::Bind(format!(
                "{} arguments differ from the selected overload",
                self.name
            )));
        }
        Ok(signature.return_type.clone())
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let signature = self.signature()?;
        if arguments.len() != signature.arguments.len() {
            return Err(Error::Internal(
                "fixed-offset timezone argument count changed after binding".into(),
            ));
        }
        for (value, data_type) in arguments.iter().zip(&signature.arguments) {
            if !value.is_null() && !value.fits_type(data_type) {
                return Err(Error::Internal(
                    "fixed-offset timezone input was not coerced".into(),
                ));
            }
        }
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if arguments.len() == 2 {
            return relocate(&arguments[0], &arguments[1]);
        }
        extract(self.name, self.part, &arguments[0])
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn extract(name: &str, part: Part, value: &Value) -> Result<Value> {
    let offset = match value {
        Value::Date(date) => {
            if !date.is_finite() {
                return Ok(Value::Null);
            }
            return Err(unsupported_unit("date", name));
        }
        Value::Temporal(TemporalValue::Interval { .. }) => {
            return Err(unsupported_unit("interval", name));
        }
        Value::Temporal(value @ TemporalValue::Timestamp(_)) => {
            if !value.is_finite() {
                return Ok(Value::Null);
            }
            0
        }
        Value::Temporal(TemporalValue::Time(_) | TemporalValue::TimeNs(_)) => 0,
        Value::Temporal(TemporalValue::TimeTz { offset, .. }) => i64::from(*offset),
        _ => {
            return Err(Error::Internal(
                "fixed-offset timezone received an unbound input".into(),
            ));
        }
    };
    Ok(Value::Integer(match part {
        Part::Timezone => i128::from(offset),
        Part::Hour => i128::from(offset / 3600),
        Part::Minute => i128::from(offset / 60 % 60),
    }))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn unsupported_unit(domain: &str, name: &str) -> Error {
    Error::NotImplemented(format!("\"{domain}\" units \"{name}\" not recognized"))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn relocate(interval: &Value, timetz: &Value) -> Result<Value> {
    let Value::Temporal(TemporalValue::Interval {
        months: _,
        days: _,
        micros: interval_micros,
    }) = interval
    else {
        return Err(Error::Internal(
            "timezone relocation expected bound INTERVAL".into(),
        ));
    };
    let Value::Temporal(TemporalValue::TimeTz { micros, offset }) = timetz else {
        return Err(Error::Internal(
            "timezone relocation expected bound TIMETZ".into(),
        ));
    };
    let output_offset = interval_micros / 1_000_000;
    if !(-MAX_OFFSET_SECONDS..=MAX_OFFSET_SECONDS).contains(&output_offset) {
        // DuckDB's unchecked physical constructor can encode an invalid TIMETZ
        // outside this declared range. Preserve Rust's safe logical invariant.
        return Err(Error::OutOfRange(
            "TIME WITH TIME ZONE offset outside +/-15:59:59".into(),
        ));
    }
    let utc = i128::from(*micros) - i128::from(*offset) * 1_000_000;
    let shift = i128::from(*interval_micros % MICROS_PER_DAY);
    let output_micros = (utc + shift).rem_euclid(i128::from(MICROS_PER_DAY)) as i64;
    let value = TemporalValue::TimeTz {
        micros: output_micros,
        offset: output_offset as i32,
    };
    value.validate()?;
    Ok(Value::Temporal(value))
}
