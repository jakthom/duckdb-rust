use crate::common::{DataType, Error, Result, Value};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn primitive(value: &Value, target: &DataType) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    if target.is_signed_integer() {
        let value = match value {
            Value::Integer(v) => *v,
            Value::Double(v)
                if v.is_finite() && *v >= i128::MIN as f64 && *v < -(i128::MIN as f64) =>
            {
                v.round() as i128
            }
            Value::Float(v) => return primitive(&Value::Double(f64::from(*v)), target),
            Value::Boolean(v) => i128::from(*v),
            Value::Varchar(v) => v
                .trim()
                .parse()
                .map_err(|_| Error::Conversion(format!("cannot cast {v:?} to {target}")))?,
            _ => {
                return Err(Error::Conversion(format!(
                    "cannot cast {value} to {target}"
                )));
            }
        };
        let fits = match target {
            DataType::TinyInt => i8::try_from(value).is_ok(),
            DataType::SmallInt => i16::try_from(value).is_ok(),
            DataType::Integer => i32::try_from(value).is_ok(),
            DataType::BigInt => i64::try_from(value).is_ok(),
            _ => true,
        };
        return if fits {
            Ok(Value::Integer(value))
        } else {
            Err(Error::Conversion(format!("{value} overflows {target}")))
        };
    }
    if *target == DataType::Float {
        let value = match value {
            Value::Float(v) => return Ok(Value::Float(*v)),
            Value::Integer(v) => *v as f32,
            Value::Unsigned(v) => *v as f32,
            Value::Decimal { .. } => value.as_f64()? as f32,
            Value::Boolean(v) => u8::from(*v) as f32,
            Value::Varchar(v) => {
                return v
                    .trim()
                    .parse::<f32>()
                    .map(Value::Float)
                    .map_err(|_| Error::Conversion(format!("cannot cast {v:?} to FLOAT")));
            }
            Value::Double(v) => {
                let result = *v as f32;
                if v.is_finite() && !result.is_finite() {
                    return Err(Error::Conversion(format!("{v} overflows FLOAT")));
                }
                result
            }
            Value::Null => unreachable!("handled NULL"),
            Value::Date(_)
            | Value::Blob(_)
            | Value::Uuid(_)
            | Value::Temporal(_)
            | Value::Nested(_)
            | Value::Extension(_) => {
                return Err(Error::Conversion(
                    "value requires a separate cast adapter".into(),
                ));
            }
        };
        return Ok(Value::Float(value));
    }
    match (value, target) {
        (Value::Boolean(_), DataType::Boolean)
        | (Value::Double(_), DataType::Double)
        | (Value::Varchar(_), DataType::Varchar) => Ok(value.clone()),
        (_, DataType::Varchar) => Ok(Value::Varchar(value.to_string())),
        (Value::Integer(v), DataType::Boolean) => Ok(Value::Boolean(*v != 0)),
        (Value::Float(v), DataType::Boolean) => Ok(Value::Boolean(*v != 0.0)),
        (Value::Double(v), DataType::Boolean) => Ok(Value::Boolean(*v != 0.0)),
        (Value::Boolean(v), DataType::Double) => Ok(Value::Double(f64::from(u8::from(*v)))),
        (Value::Varchar(v), DataType::Boolean) => match v.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" => Ok(Value::Boolean(true)),
            "false" | "f" | "0" => Ok(Value::Boolean(false)),
            _ => Err(Error::Conversion(format!("cannot cast {v:?} to BOOLEAN"))),
        },
        (Value::Varchar(v), DataType::Double) => v
            .parse()
            .map(Value::Double)
            .map_err(|_| Error::Conversion(format!("cannot cast {v:?} to DOUBLE"))),
        (_, DataType::Double) => Ok(Value::Double(value.as_f64()?)),
        _ => Err(Error::Conversion(format!(
            "cannot cast {value} to {target}"
        ))),
    }
}
