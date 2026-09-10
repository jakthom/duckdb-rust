//! Owned recursive metadata and scalar payloads. Vectors may share the outer
//! payload, while logical validation remains the selected adapter's responsibility.
use std::{fmt, sync::Arc};

use serde::{Deserialize, Serialize};

use super::{DataType, Error, Result, Value};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum NestedType {
    List(DataType),
    Array { element: DataType, length: usize },
    Struct(Vec<(String, DataType)>),
    Map { key: DataType, value: DataType },
    Union(Vec<(String, DataType)>),
    Variant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NestedValue {
    pub data_type: DataType,
    pub payload: NestedPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NestedPayload {
    Sequence(Vec<Value>),
    Struct(Vec<Value>),
    Map(Vec<(Value, Value)>),
    Union { tag: usize, value: Value },
    Variant(Value),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl NestedType {
    pub fn data_type(self) -> DataType {
        DataType::Nested(Arc::new(self))
    }

    pub fn children(&self) -> Vec<&DataType> {
        match self {
            Self::List(element) | Self::Array { element, .. } => vec![element],
            Self::Struct(fields) | Self::Union(fields) => fields.iter().map(|(_, ty)| ty).collect(),
            Self::Map { key, value } => vec![key, value],
            Self::Variant => Vec::new(),
        }
    }

    pub fn family(&self) -> &'static str {
        match self {
            Self::List(_) => "builtin.list",
            Self::Array { .. } => "builtin.array",
            Self::Struct(_) => "builtin.struct",
            Self::Map { .. } => "builtin.map",
            Self::Union(_) => "builtin.union",
            Self::Variant => "builtin.variant",
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl NestedValue {
    pub fn value(data_type: DataType, payload: NestedPayload) -> Result<Value> {
        let value = Self { data_type, payload };
        if !value.fits_type() {
            return Err(Error::Conversion(
                "nested payload differs from declared shape".into(),
            ));
        }
        Ok(Value::Nested(Arc::new(value)))
    }

    pub fn fits_type(&self) -> bool {
        self.fits_at_depth(0)
    }

    fn fits_at_depth(&self, depth: usize) -> bool {
        if depth > 64 || super::type_registry::check_metadata(&self.data_type).is_err() {
            return false;
        }
        let DataType::Nested(metadata) = &self.data_type else {
            return false;
        };
        let fits = |value: &Value, target: &DataType| match value {
            Value::Nested(value) => value.fits_at_depth(depth + 1) && value.data_type == *target,
            value => value.fits_type(target),
        };
        match (metadata.as_ref(), &self.payload) {
            (NestedType::List(element), NestedPayload::Sequence(values)) => {
                values.iter().all(|value| fits(value, element))
            }
            (NestedType::Array { element, length }, NestedPayload::Sequence(values)) => {
                values.len() == *length && values.iter().all(|value| fits(value, element))
            }
            (NestedType::Struct(fields), NestedPayload::Struct(values)) => {
                fields.len() == values.len()
                    && fields
                        .iter()
                        .zip(values)
                        .all(|((_, ty), value)| fits(value, ty))
            }
            (NestedType::Map { key, value }, NestedPayload::Map(entries)) => entries
                .iter()
                .all(|(k, v)| !k.is_null() && fits(k, key) && fits(v, value)),
            (NestedType::Union(fields), NestedPayload::Union { tag, value }) => {
                fields.get(*tag).is_some_and(|(_, ty)| fits(value, ty))
            }
            (NestedType::Variant, NestedPayload::Variant(value)) => fits(value, &value.data_type()),
            _ => false,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Display for NestedType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::List(element) => write!(f, "{element}[]"),
            Self::Array { element, length } => write!(f, "{element}[{length}]"),
            Self::Map { key, value } => write!(f, "MAP({key}, {value})"),
            Self::Struct(fields) | Self::Union(fields) => {
                write!(
                    f,
                    "{}(",
                    if matches!(self, Self::Struct(_)) {
                        "STRUCT"
                    } else {
                        "UNION"
                    }
                )?;
                for (index, (name, ty)) in fields.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "\"{}\" {ty}", name.replace('"', "\"\""))?;
                }
                write!(f, ")")
            }
            Self::Variant => write!(f, "VARIANT"),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Display for NestedValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.payload {
            NestedPayload::Sequence(values) => {
                write!(f, "[")?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{value}")?;
                }
                write!(f, "]")
            }
            NestedPayload::Struct(values) => {
                let DataType::Nested(metadata) = &self.data_type else {
                    return Err(fmt::Error);
                };
                let NestedType::Struct(fields) = metadata.as_ref() else {
                    return Err(fmt::Error);
                };
                write!(f, "{{")?;
                for (index, ((name, _), value)) in fields.iter().zip(values).enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "'{}': {value}", name.replace('\'', "''"))?;
                }
                write!(f, "}}")
            }
            NestedPayload::Map(entries) => {
                write!(f, "{{")?;
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{key}={value}")?;
                }
                write!(f, "}}")
            }
            NestedPayload::Union { value, .. } | NestedPayload::Variant(value) => {
                write!(f, "{value}")
            }
        }
    }
}
