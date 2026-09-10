//! Logical VARIANT views over retained typed children. ARRAY/LIST, MAP and UNION
//! need not be copied into a second physical tree merely to inspect a value.
use super::{DataType, Error, NestedPayload, NestedType, NestedValue, Result, Value};

#[derive(Clone, Copy)]
pub(crate) enum Node<'a> {
    Typed(&'a DataType, &'a Value),
    MapEntry(&'a DataType, &'a Value, &'a DataType, &'a Value),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> Node<'a> {
    pub fn resolved(self) -> Result<Self> {
        let mut node = self;
        for _ in 0..=64 {
            match node {
                Self::Typed(_, Value::Nested(value)) => match &value.payload {
                    NestedPayload::Variant { data_type, value } => {
                        node = Self::Typed(data_type, value);
                    }
                    NestedPayload::Union { tag, value: child } => {
                        let DataType::Nested(metadata) = &value.data_type else {
                            return Err(invalid());
                        };
                        let NestedType::Union(fields) = metadata.as_ref() else {
                            return Err(invalid());
                        };
                        node = Self::Typed(&fields.get(*tag).ok_or_else(invalid)?.1, child);
                    }
                    _ => return Ok(node),
                },
                _ => return Ok(node),
            }
        }
        Err(Error::Resource("VARIANT depth exceeds 64".into()))
    }

    pub fn rank(self) -> Result<u8> {
        Ok(match self.resolved()? {
            Self::MapEntry(..) => 15,
            Self::Typed(_, Value::Null) => 16,
            Self::Typed(ty, _) if ty.is_integer() || ty.is_decimal() || *ty == DataType::Bignum => {
                2
            }
            Self::Typed(ty, _) if ty.is_floating() => 3,
            Self::Typed(ty, _) => match ty {
                DataType::Boolean => 1,
                DataType::Varchar | DataType::Enum(_) => 4,
                DataType::Blob => 5,
                DataType::Uuid => 6,
                DataType::Date
                | DataType::Timestamp
                | DataType::TimestampS
                | DataType::TimestampMs
                | DataType::TimestampNs => 7,
                DataType::TimestampTz | DataType::TimestampTzNs => 8,
                DataType::Time | DataType::TimeNs => 9,
                DataType::TimeTz => 10,
                DataType::Interval => 11,
                DataType::Bit => 13,
                DataType::Nested(metadata) => match metadata.as_ref() {
                    NestedType::List(_)
                    | NestedType::Array { .. }
                    | NestedType::Map { .. }
                    | NestedType::Tuple(_) => 14,
                    NestedType::Struct(_) | NestedType::Object(_) => 15,
                    _ => return Err(invalid()),
                },
                _ => return Err(Error::Unsupported(format!("VARIANT category for {ty}"))),
            },
        })
    }

    pub fn array(self) -> Result<Vec<Self>> {
        let Self::Typed(DataType::Nested(metadata), Value::Nested(value)) = self.resolved()? else {
            return Err(invalid());
        };
        Ok(match (metadata.as_ref(), &value.payload) {
            (NestedType::Tuple(types), NestedPayload::Struct(values)) => types
                .iter()
                .zip(values)
                .map(|(ty, value)| Self::Typed(ty, value))
                .collect(),
            (
                NestedType::List(child) | NestedType::Array { element: child, .. },
                NestedPayload::Sequence(values),
            ) => values
                .iter()
                .map(|value| Self::Typed(child, value))
                .collect(),
            (NestedType::Map { key, value }, NestedPayload::Map(entries)) => entries
                .iter()
                .map(|(k, v)| Self::MapEntry(key, k, value, v))
                .collect(),
            _ => return Err(invalid()),
        })
    }

    pub fn object(self) -> Result<Vec<(&'a str, Self)>> {
        Ok(match self.resolved()? {
            Self::MapEntry(kt, k, vt, v) => {
                vec![("key", Self::Typed(kt, k)), ("value", Self::Typed(vt, v))]
            }
            Self::Typed(DataType::Nested(metadata), Value::Nested(value)) => {
                let (
                    NestedType::Struct(fields) | NestedType::Object(fields),
                    NestedPayload::Struct(values),
                ) = (metadata.as_ref(), &value.payload)
                else {
                    return Err(invalid());
                };
                fields
                    .iter()
                    .zip(values)
                    .map(|((name, ty), value)| (name.as_str(), Self::Typed(ty, value)))
                    .collect()
            }
            _ => return Err(invalid()),
        })
    }

    pub fn owned(self) -> Result<Value> {
        let (data_type, value) = match self.resolved()? {
            Self::Typed(_, Value::Null) => return Ok(Value::Null),
            Self::Typed(_, Value::Enum(value)) => {
                (DataType::Varchar, Value::Varchar(value.label()?.to_owned()))
            }
            Self::Typed(ty, value) => (ty.clone(), value.clone()),
            Self::MapEntry(kt, k, vt, v) => {
                let ty = NestedType::Struct(vec![
                    ("key".into(), kt.clone()),
                    ("value".into(), vt.clone()),
                ])
                .data_type();
                let value = NestedValue::value(
                    ty.clone(),
                    NestedPayload::Struct(vec![k.clone(), v.clone()]),
                )?;
                (ty, value)
            }
        };
        NestedValue::value(
            NestedType::Variant.data_type(),
            NestedPayload::Variant { data_type, value },
        )
    }

    pub fn type_name(self) -> Result<String> {
        let node = self.resolved()?;
        if node.rank()? == 16 {
            return Ok("VARIANT_NULL".into());
        }
        if node.rank()? == 14 {
            return Ok(format!("ARRAY({})", node.array()?.len()));
        }
        if node.rank()? == 15 {
            return Ok(format!(
                "OBJECT({})",
                node.object()?
                    .iter()
                    .map(|(key, _)| *key)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let Self::Typed(ty, value) = node else {
            return Err(invalid());
        };
        Ok(match ty {
            DataType::Boolean => {
                if *value == Value::Boolean(true) {
                    "BOOL_TRUE"
                } else {
                    "BOOL_FALSE"
                }
            }
            DataType::TinyInt => "INT8",
            DataType::SmallInt => "INT16",
            DataType::Integer => "INT32",
            DataType::BigInt => "INT64",
            DataType::HugeInt => "INT128",
            DataType::UTinyInt => "UINT8",
            DataType::USmallInt => "UINT16",
            DataType::UInteger => "UINT32",
            DataType::UBigInt => "UINT64",
            DataType::UHugeInt => "UINT128",
            DataType::Bignum => "BIGNUM",
            DataType::Decimal { width, scale } => return Ok(format!("DECIMAL({width}, {scale})")),
            DataType::Float => "FLOAT",
            DataType::Double => "DOUBLE",
            DataType::Varchar | DataType::Enum(_) => "VARCHAR",
            DataType::Blob => "BLOB",
            DataType::Uuid => "UUID",
            DataType::Date => "DATE",
            DataType::Time => "TIME_MICROS",
            DataType::TimeNs => "TIME_NANOS",
            DataType::TimeTz => "TIME_MICROS_TZ",
            DataType::TimestampS => "TIMESTAMP_SEC",
            DataType::TimestampMs => "TIMESTAMP_MILIS",
            DataType::Timestamp => "TIMESTAMP_MICROS",
            DataType::TimestampNs => "TIMESTAMP_NANOS",
            DataType::TimestampTz => "TIMESTAMP_MICROS_TZ",
            DataType::TimestampTzNs => "TIMESTAMP_NANOS_TZ",
            DataType::Interval => "INTERVAL",
            DataType::Bit => "BITSTRING",
            _ => return Err(invalid()),
        }
        .into())
    }

    pub fn materialized(
        self,
        depth: usize,
        check: &impl Fn() -> Result<()>,
    ) -> Result<(DataType, Value)> {
        check()?;
        if depth > 64 {
            return Err(Error::Resource(
                "VARIANT materialization depth exceeds 64".into(),
            ));
        }
        let node = self.resolved()?;
        match node.rank()? {
            16 => Ok((DataType::Null, Value::Null)),
            14 => {
                let children = node
                    .array()?
                    .into_iter()
                    .map(|child| child.materialized(depth + 1, check))
                    .collect::<Result<Vec<_>>>()?;
                let child_type = children
                    .first()
                    .map(|(ty, _)| ty.clone())
                    .filter(|ty| children.iter().all(|(other, _)| other == ty))
                    .unwrap_or_else(|| NestedType::Variant.data_type());
                let variant = child_type.family() == "builtin.variant";
                let values = children
                    .into_iter()
                    .map(|(ty, value)| {
                        if variant {
                            Node::Typed(&ty, &value).owned()
                        } else {
                            Ok(value)
                        }
                    })
                    .collect::<Result<_>>()?;
                let ty = NestedType::List(child_type).data_type();
                Ok((
                    ty.clone(),
                    NestedValue::value(ty, NestedPayload::Sequence(values))?,
                ))
            }
            15 => {
                let mut fields = Vec::new();
                let mut values = Vec::new();
                for (name, child) in node.object()? {
                    let (ty, value) = child.materialized(depth + 1, check)?;
                    fields.push((name.to_owned(), ty));
                    values.push(value);
                }
                let ty = if matches!(node, Self::Typed(DataType::Nested(metadata), _) if matches!(metadata.as_ref(), NestedType::Object(_))) {
                    NestedType::Object(fields)
                } else {
                    // Preserve retained declared STRUCT adapters and old
                    // private VARIANT payloads; only dynamic OBJECT metadata
                    // uses the separate exact-name validation contract.
                    NestedType::Struct(fields)
                }.data_type();
                Ok((
                    ty.clone(),
                    NestedValue::value(ty, NestedPayload::Struct(values))?,
                ))
            }
            _ => match node {
                Self::Typed(_, Value::Enum(value)) => {
                    Ok((DataType::Varchar, Value::Varchar(value.label()?.to_owned())))
                }
                Self::Typed(ty, value) => Ok((ty.clone(), value.clone())),
                _ => Err(invalid()),
            },
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn invalid() -> Error {
    Error::Conversion("invalid VARIANT logical payload".into())
}

// Development emits natively supported STRUCT trees as shredded values. Their
// observable field presentation is lexicographic, whereas unshredded arrays,
// enums and NULL-typed leaves preserve source member order. Reproduce that
// presentation rule without prescribing a shredded Rust physical layout.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn source_is_shreddable(ty: &DataType) -> bool {
    match ty {
        DataType::Nested(metadata) => match metadata.as_ref() {
            NestedType::Struct(fields) => {
                !fields.is_empty() && fields.iter().all(|(_, ty)| source_is_shreddable(ty))
            }
            _ => false,
        },
        DataType::Null | DataType::Enum(_) | DataType::Bignum | DataType::Extension(_) => false,
        _ => true,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn inject(ty: &DataType, value: &Value) -> Result<Value> {
    if source_is_shreddable(ty) {
        let (ty, value) = ordered_struct(ty, value)?;
        Node::Typed(&ty, &value).owned()
    } else {
        Node::Typed(ty, value).owned()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn ordered_struct(ty: &DataType, value: &Value) -> Result<(DataType, Value)> {
    let (DataType::Nested(metadata), Value::Nested(value)) = (ty, value) else {
        return Ok((ty.clone(), value.clone()));
    };
    let (NestedType::Struct(fields), NestedPayload::Struct(values)) =
        (metadata.as_ref(), &value.payload)
    else {
        return Err(invalid());
    };
    let mut entries = fields.iter().zip(values).collect::<Vec<_>>();
    entries.sort_by(|a, b| a.0.0.cmp(&b.0.0));
    let mut fields = Vec::with_capacity(entries.len());
    let mut values = Vec::with_capacity(entries.len());
    for ((name, ty), value) in entries {
        let (ty, value) = ordered_struct(ty, value)?;
        fields.push((name.clone(), ty));
        values.push(value);
    }
    let ty = NestedType::Struct(fields).data_type();
    let value = NestedValue::value(ty.clone(), NestedPayload::Struct(values))?;
    Ok((ty, value))
}
