//! Owned recursive metadata and scalar payloads. Vectors may share the outer
//! payload, while logical validation remains the selected adapter's responsibility.
use std::{fmt, sync::Arc};

use serde::{Deserialize, Serialize};

use super::{DataType, Error, Result, Value};
mod keywords;

struct ChildType<'a>(&'a DataType);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Display for ChildType<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self.0 == DataType::Null {
            write!(f, "\"NULL\"")
        } else {
            write!(f, "{}", self.0)
        }
    }
}

/// Resolve untyped NULL leaves at a table-storage boundary. Do not apply this
/// during ordinary expression inference, where LIST(NULL) is observable.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub fn normalize_storage_type(data_type: &DataType) -> Result<DataType> {
    super::type_registry::check_metadata(data_type)?;
    normalize_storage_type_inner(data_type)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn normalize_storage_type_inner(data_type: &DataType) -> Result<DataType> {
    let DataType::Nested(metadata) = data_type else {
        return Ok(if *data_type == DataType::Null {
            DataType::Integer
        } else {
            data_type.clone()
        });
    };
    let fields = |fields: &[(String, DataType)]| {
        fields
            .iter()
            .map(|(name, ty)| Ok((name.clone(), normalize_storage_type_inner(ty)?)))
            .collect::<Result<Vec<_>>>()
    };
    Ok(match metadata.as_ref() {
        NestedType::List(child) => NestedType::List(normalize_storage_type_inner(child)?),
        NestedType::Array { element, length } => NestedType::Array {
            element: normalize_storage_type_inner(element)?,
            length: *length,
        },
        NestedType::Struct(children) => NestedType::Struct(fields(children)?),
        NestedType::Map { key, value } => NestedType::Map {
            key: normalize_storage_type_inner(key)?,
            value: normalize_storage_type_inner(value)?,
        },
        NestedType::Union(children) => NestedType::Union(fields(children)?),
        NestedType::Variant => NestedType::Variant,
    }
    .data_type())
}

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
    Variant { data_type: DataType, value: Value },
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
        // Shared Arc subtrees can describe exponentially many logical visits
        // while using little physical memory. Bound both depth and traversal.
        self.fits_at_depth(0, &mut 16_777_216)
    }

    fn fits_at_depth(&self, depth: usize, remaining: &mut usize) -> bool {
        if depth > 64
            || *remaining == 0
            || super::type_registry::check_metadata(&self.data_type).is_err()
        {
            return false;
        }
        *remaining -= 1;
        let DataType::Nested(metadata) = &self.data_type else {
            return false;
        };
        let mut fits = |value: &Value, target: &DataType| match value {
            Value::Nested(value) => {
                value.fits_at_depth(depth + 1, remaining) && value.data_type == *target
            }
            value => {
                if *remaining == 0 {
                    return false;
                }
                *remaining -= 1;
                value.fits_type(target)
            }
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
            (NestedType::Variant, NestedPayload::Variant { data_type, value }) => {
                super::type_registry::check_metadata(data_type).is_ok() && fits(value, data_type)
            }
            _ => false,
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Display for NestedType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::List(element) => write!(f, "{}[]", ChildType(element)),
            Self::Array { element, length } => write!(f, "{}[{length}]", ChildType(element)),
            Self::Map { key, value } => write!(f, "MAP({}, {})", ChildType(key), ChildType(value)),
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
                    if keywords::requires_quotes(name) {
                        write!(f, "\"{}\"", name.replace('"', "\"\""))?;
                    } else {
                        write!(f, "{name}")?;
                    }
                    write!(f, " {}", ChildType(ty))?;
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
                let DataType::Nested(metadata) = &self.data_type else {
                    return Err(fmt::Error);
                };
                let child = match metadata.as_ref() {
                    NestedType::List(child) | NestedType::Array { element: child, .. } => child,
                    _ => return Err(fmt::Error),
                };
                write!(f, "[")?;
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    display_child(f, child, value)?;
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
                for (index, ((name, ty), value)) in fields.iter().zip(values).enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    display_quoted(f, name, true)?;
                    write!(f, ": ")?;
                    display_child(f, ty, value)?;
                }
                write!(f, "}}")
            }
            NestedPayload::Map(entries) => {
                let DataType::Nested(metadata) = &self.data_type else {
                    return Err(fmt::Error);
                };
                let NestedType::Map { key: kt, value: vt } = metadata.as_ref() else {
                    return Err(fmt::Error);
                };
                write!(f, "{{")?;
                for (index, (key, value)) in entries.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    display_child(f, kt, key)?;
                    write!(f, "=")?;
                    display_child(f, vt, value)?;
                }
                write!(f, "}}")
            }
            NestedPayload::Union { value, .. } => {
                write!(f, "{value}")
            }
            NestedPayload::Variant { data_type, value } => {
                let (_, value) = super::variant::Node::Typed(data_type, value)
                    .materialized(0, &|| Ok(()))
                    .map_err(|_| fmt::Error)?;
                write!(f, "{value}")
            }
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn display_child(f: &mut fmt::Formatter<'_>, ty: &DataType, value: &Value) -> fmt::Result {
    if value.is_null() || matches!(ty, DataType::Nested(_)) {
        write!(f, "{value}")
    } else {
        display_quoted(f, &value.to_string(), false)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn display_quoted(f: &mut fmt::Formatter<'_>, value: &str, key: bool) -> fmt::Result {
    let quote = key
        || value.is_empty()
        || value.eq_ignore_ascii_case("null")
        || value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_whitespace)
        || value.as_bytes().last().is_some_and(u8::is_ascii_whitespace)
        || value.bytes().any(|byte| {
            matches!(
                byte,
                b'"' | b'\'' | b'(' | b')' | b',' | b':' | b'=' | b'[' | b']' | b'{' | b'}'
            )
        });
    if !quote {
        return write!(f, "{value}");
    }
    write!(f, "'")?;
    for character in value.chars() {
        if matches!(character, '\'' | '\\') {
            write!(f, "\\")?;
        }
        write!(f, "{character}")?;
    }
    write!(f, "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn variant_payload_retains_declared_width_and_recursive_visit_budget() -> Result<()> {
        let ty = NestedType::Variant.data_type();
        let value = NestedValue {
            data_type: ty.clone(),
            payload: NestedPayload::Variant {
                data_type: DataType::BigInt,
                value: Value::Integer(1),
            },
        };
        assert!(value.fits_type());
        assert!(matches!(
            value.payload,
            NestedPayload::Variant {
                data_type: DataType::BigInt,
                ..
            }
        ));
        let mut shared = Value::Nested(Arc::new(value));
        let mut child = ty;
        for _ in 0..10 {
            child = NestedType::List(child).data_type();
            shared = Value::Nested(Arc::new(NestedValue {
                data_type: child.clone(),
                payload: NestedPayload::Sequence(vec![shared.clone(), shared]),
            }));
        }
        let Value::Nested(value) = shared else {
            unreachable!()
        };
        let mut budget = 100;
        assert!(!value.fits_at_depth(0, &mut budget));
        assert_eq!(budget, 0);
        assert!(value.fits_type());
        Ok(())
    }
}
