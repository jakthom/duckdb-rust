use std::{cmp::Ordering, fmt, sync::Arc};

use serde::{Deserialize, Serialize};

use super::{Date, Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DataType {
    Null,
    Boolean,
    TinyInt,
    SmallInt,
    Integer,
    BigInt,
    HugeInt,
    UTinyInt,
    USmallInt,
    UInteger,
    UBigInt,
    UHugeInt,
    Decimal { width: u8, scale: u8 },
    Float,
    Double,
    Varchar,
    Blob,
    Uuid,
    Date,
    Time,
    TimeNs,
    TimeTz,
    Timestamp,
    TimestampS,
    TimestampMs,
    TimestampNs,
    TimestampTz,
    TimestampTzNs,
    Interval,
    Extension(Arc<TypeIdentity>),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TypeIdentity {
    pub name: String,
    pub parameters: Vec<TypeParameter>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TypeParameter {
    Integer(i64),
    Text(String),
    Type(Box<DataType>),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl DataType {
    pub fn extension(name: impl Into<String>, parameters: Vec<TypeParameter>) -> Self {
        Self::Extension(Arc::new(TypeIdentity {
            name: name.into(),
            parameters,
        }))
    }
    pub fn family(&self) -> &str {
        match self {
            Self::Null => "builtin.null",
            Self::Boolean => "builtin.boolean",
            Self::TinyInt => "builtin.tinyint",
            Self::SmallInt => "builtin.smallint",
            Self::Integer => "builtin.integer",
            Self::BigInt => "builtin.bigint",
            Self::HugeInt => "builtin.hugeint",
            Self::UTinyInt => "builtin.utinyint",
            Self::USmallInt => "builtin.usmallint",
            Self::UInteger => "builtin.uinteger",
            Self::UBigInt => "builtin.ubigint",
            Self::UHugeInt => "builtin.uhugeint",
            Self::Decimal { .. } => "builtin.decimal",
            Self::Float => "builtin.float",
            Self::Double => "builtin.double",
            Self::Varchar => "builtin.varchar",
            Self::Blob => "builtin.blob",
            Self::Uuid => "builtin.uuid",
            Self::Date => "builtin.date",
            Self::Time => "builtin.time",
            Self::TimeNs => "builtin.time_ns",
            Self::TimeTz => "builtin.time_tz",
            Self::Timestamp => "builtin.timestamp",
            Self::TimestampS => "builtin.timestamp_s",
            Self::TimestampMs => "builtin.timestamp_ms",
            Self::TimestampNs => "builtin.timestamp_ns",
            Self::TimestampTz => "builtin.timestamp_tz",
            Self::TimestampTzNs => "builtin.timestamp_tz_ns",
            Self::Interval => "builtin.interval",
            Self::Extension(identity) => &identity.name,
        }
    }

    pub fn integer_bits(&self) -> Option<u8> {
        match self {
            Self::TinyInt => Some(8),
            Self::SmallInt => Some(16),
            Self::Integer => Some(32),
            Self::BigInt => Some(64),
            Self::HugeInt => Some(128),
            _ => None,
        }
    }
    pub fn is_integer(&self) -> bool {
        self.is_signed_integer() || self.is_unsigned_integer()
    }
    /// Machine-width fast paths using i128 must require this capability.
    pub fn is_signed_integer(&self) -> bool {
        matches!(
            self,
            Self::TinyInt | Self::SmallInt | Self::Integer | Self::BigInt | Self::HugeInt
        )
    }

    pub fn is_numeric(&self) -> bool {
        self.is_integer() || self.is_floating() || self.is_decimal()
    }

    pub fn is_floating(&self) -> bool {
        matches!(self, Self::Float | Self::Double)
    }

    pub fn common(left: &Self, right: &Self) -> Result<Self> {
        if matches!(left, Self::Extension(_)) || matches!(right, Self::Extension(_)) {
            return Err(Error::Unsupported(
                "registered common types require their selected registry".into(),
            ));
        }
        if left == right || *right == Self::Null {
            return Ok(left.clone());
        }
        if *left == Self::Null {
            return Ok(right.clone());
        }
        if left.is_numeric() && right.is_numeric() {
            return super::numeric::common_type(left, right);
        }
        Err(Error::Bind(format!(
            "incompatible types {left} and {right}"
        )))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Self::Decimal { width, scale } = self {
            return write!(f, "DECIMAL({width},{scale})");
        }
        if let Self::Extension(identity) = self {
            let TypeIdentity { name, parameters } = identity.as_ref();
            write!(f, "{name}")?;
            if !parameters.is_empty() {
                write!(f, "(")?;
                for (i, parameter) in parameters.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    match parameter {
                        TypeParameter::Integer(value) => write!(f, "{value}")?,
                        TypeParameter::Text(value) => write!(f, "'{}'", value.replace('\'', "''"))?,
                        TypeParameter::Type(value) => write!(f, "{value}")?,
                    }
                }
                write!(f, ")")?;
            }
            return Ok(());
        }
        write!(
            f,
            "{}",
            match self {
                Self::Null => "NULL",
                Self::Boolean => "BOOLEAN",
                Self::TinyInt => "TINYINT",
                Self::SmallInt => "SMALLINT",
                Self::Integer => "INTEGER",
                Self::BigInt => "BIGINT",
                Self::HugeInt => "HUGEINT",
                Self::UTinyInt => "UTINYINT",
                Self::USmallInt => "USMALLINT",
                Self::UInteger => "UINTEGER",
                Self::UBigInt => "UBIGINT",
                Self::UHugeInt => "UHUGEINT",
                Self::Decimal { .. } => unreachable!("handled decimal type"),
                Self::Float => "FLOAT",
                Self::Double => "DOUBLE",
                Self::Varchar => "VARCHAR",
                Self::Blob => "BLOB",
                Self::Uuid => "UUID",
                Self::Date => "DATE",
                Self::Time => "TIME",
                Self::TimeNs => "TIME_NS",
                Self::TimeTz => "TIME WITH TIME ZONE",
                Self::Timestamp => "TIMESTAMP",
                Self::TimestampS => "TIMESTAMP_S",
                Self::TimestampMs => "TIMESTAMP_MS",
                Self::TimestampNs => "TIMESTAMP_NS",
                Self::TimestampTz => "TIMESTAMP WITH TIME ZONE",
                Self::TimestampTzNs => "TIMESTAMP_NS WITH TIME ZONE",
                Self::Interval => "INTERVAL",
                Self::Extension(_) => unreachable!("handled extension type"),
            }
        )
    }
}

pub type Row = Vec<Value>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Boolean(bool),
    Integer(i128),
    Unsigned(u128),
    Decimal {
        value: i128,
        width: u8,
        scale: u8,
    },
    Float(#[serde(with = "float32_bits")] f32),
    Double(#[serde(with = "float_bits")] f64),
    Varchar(String),
    Blob(Vec<u8>),
    /// UUID bits in network/text order, without the native storage sign-bit flip.
    Uuid(u128),
    Date(Date),
    Temporal(super::TemporalValue),
    Extension(Arc<ExtensionValue>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionValue {
    pub data_type: DataType,
    pub bytes: Vec<u8>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Value {
    /// Constructs an owned opaque payload. Engine boundaries validate it against
    /// the selected type adapter, as they do other externally supplied values.
    pub fn extension(data_type: DataType, bytes: Vec<u8>) -> Self {
        Self::Extension(Arc::new(ExtensionValue { data_type, bytes }))
    }
    pub fn data_type(&self) -> DataType {
        match self {
            Self::Null => DataType::Null,
            Self::Boolean(_) => DataType::Boolean,
            Self::Integer(v) if i32::try_from(*v).is_ok() => DataType::Integer,
            Self::Integer(v) if i64::try_from(*v).is_ok() => DataType::BigInt,
            Self::Integer(_) => DataType::HugeInt,
            Self::Unsigned(v) if u32::try_from(*v).is_ok() => DataType::UInteger,
            Self::Unsigned(v) if u64::try_from(*v).is_ok() => DataType::UBigInt,
            Self::Unsigned(_) => DataType::UHugeInt,
            Self::Decimal { width, scale, .. } => DataType::Decimal {
                width: *width,
                scale: *scale,
            },
            Self::Float(_) => DataType::Float,
            Self::Double(_) => DataType::Double,
            Self::Varchar(_) => DataType::Varchar,
            Self::Blob(_) => DataType::Blob,
            Self::Uuid(_) => DataType::Uuid,
            Self::Date(_) => DataType::Date,
            Self::Temporal(value) => value.data_type(),
            Self::Extension(value) => value.data_type.clone(),
        }
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Whether this physical value fits a declared type without conversion.
    /// NULL is a valid placeholder for every type; nullability is separate.
    #[inline]
    pub fn fits_type(&self, data_type: &DataType) -> bool {
        match self {
            Self::Null => true,
            Self::Integer(v) => match data_type {
                DataType::TinyInt => i8::try_from(*v).is_ok(),
                DataType::SmallInt => i16::try_from(*v).is_ok(),
                DataType::Integer => i32::try_from(*v).is_ok(),
                DataType::BigInt => i64::try_from(*v).is_ok(),
                DataType::HugeInt => true,
                _ => false,
            },
            Self::Unsigned(v) => data_type
                .unsigned_bits()
                .is_some_and(|bits| bits == 128 || *v < (1_u128 << bits)),
            Self::Decimal {
                value,
                width,
                scale,
            } => {
                *data_type
                    == DataType::Decimal {
                        width: *width,
                        scale: *scale,
                    }
                    && (1..=38).contains(width)
                    && scale <= width
                    && value.unsigned_abs() < super::numeric::DECIMAL_POWERS[usize::from(*width)]
            }
            Self::Extension(value) => {
                matches!(value.data_type, DataType::Extension(_))
                    && value.data_type == *data_type
                    && value.bytes.len() <= 16 * 1024 * 1024
            }
            Self::Temporal(value) => value.data_type() == *data_type && value.validate().is_ok(),
            _ => self.data_type() == *data_type,
        }
    }

    pub fn as_bool(&self) -> Result<Option<bool>> {
        match self {
            Self::Null => Ok(None),
            Self::Boolean(v) => Ok(Some(*v)),
            _ => Err(Error::Conversion(format!("{self} is not BOOLEAN"))),
        }
    }

    #[inline]
    pub fn as_i128(&self) -> Result<i128> {
        match self {
            Self::Integer(v) => Ok(*v),
            Self::Unsigned(v) => i128::try_from(*v)
                .map_err(|_| Error::Conversion("unsigned integer exceeds HUGEINT".into())),
            _ => Err(Error::Conversion(format!("{self} is not an integer"))),
        }
    }

    pub fn as_date(&self) -> Result<Date> {
        match self {
            Self::Date(date) => Ok(*date),
            _ => Err(Error::Conversion("value is not DATE".into())),
        }
    }

    pub fn as_temporal(&self) -> Result<super::TemporalValue> {
        match self {
            Self::Temporal(value) => Ok(*value),
            _ => Err(Error::Conversion("value is not temporal".into())),
        }
    }

    pub fn as_f64(&self) -> Result<f64> {
        match self {
            Self::Integer(v) => Ok(*v as f64),
            Self::Unsigned(v) => Ok(*v as f64),
            Self::Decimal { value, scale, .. } => {
                Ok(*value as f64 / 10_f64.powi(i32::from(*scale)))
            }
            Self::Float(v) => Ok(f64::from(*v)),
            Self::Double(v) => Ok(*v),
            _ => Err(Error::Conversion(format!("{self} is not numeric"))),
        }
    }

    pub fn as_f32(&self) -> Result<f32> {
        match self {
            Self::Float(v) => Ok(*v),
            _ => Err(Error::Conversion(format!("{self} is not FLOAT"))),
        }
    }

    /// Context-free explicit conversion using the default primitive registry.
    /// Query compilation must select casts from its configured registry instead.
    pub fn cast(&self, target: &DataType) -> Result<Self> {
        super::cast::defaults()
            .bind(
                &self.data_type(),
                target,
                super::cast::CastMode::Explicit,
                &super::type_registry::builtin_types(),
            )?
            .apply(self, &crate::parallel::QueryContext::background())
    }

    /// Total ordering within a bound SQL type. NULL placement belongs to the sort operator.
    pub fn compare(&self, other: &Self) -> Result<Ordering> {
        if matches!(self, Self::Extension(_)) || matches!(other, Self::Extension(_)) {
            return Err(Error::Unsupported(
                "registered comparisons require their selected registry".into(),
            ));
        }
        match (self, other) {
            (Self::Null, Self::Null) => Ok(Ordering::Equal),
            (Self::Integer(a), Self::Integer(b)) => Ok(a.cmp(b)),
            (Self::Unsigned(a), Self::Unsigned(b)) => Ok(a.cmp(b)),
            (
                Self::Decimal {
                    value: a,
                    scale: sa,
                    ..
                },
                Self::Decimal {
                    value: b,
                    scale: sb,
                    ..
                },
            ) => super::numeric::compare_decimal(*a, *sa, *b, *sb),
            (Self::Integer(a), Self::Unsigned(b)) => Ok(if *a < 0 {
                Ordering::Less
            } else {
                (*a as u128).cmp(b)
            }),
            (Self::Unsigned(a), Self::Integer(b)) => Ok(if *b < 0 {
                Ordering::Greater
            } else {
                a.cmp(&(*b as u128))
            }),
            (Self::Boolean(a), Self::Boolean(b)) => Ok(a.cmp(b)),
            (Self::Date(a), Self::Date(b)) => Ok(a.cmp(b)),
            (Self::Temporal(a), Self::Temporal(b)) => a.compare(*b),
            (Self::Varchar(a), Self::Varchar(b)) => Ok(a.cmp(b)),
            (Self::Blob(a), Self::Blob(b)) => Ok(a.cmp(b)),
            (Self::Uuid(a), Self::Uuid(b)) => Ok(a.cmp(b)),
            (Self::Float(a), Self::Float(b)) => Ok(float_cmp(f64::from(*a), f64::from(*b))),
            (Self::Double(a), Self::Double(b)) => Ok(float_cmp(*a, *b)),
            (left, right) if left.data_type().is_numeric() && right.data_type().is_numeric() => {
                Ok(float_cmp(self.as_f64()?, other.as_f64()?))
            }
            _ => Err(Error::Conversion(format!(
                "cannot compare {} and {}",
                self.data_type(),
                other.data_type()
            ))),
        }
    }

    /// Canonical keys for values already coerced to the same bound type.
    pub(crate) fn append_primitive_key(
        &self,
        key: &mut super::type_registry::KeyWriter<'_>,
    ) -> Result<()> {
        match self {
            Self::Null => key.push(0)?,
            Self::Boolean(v) => {
                key.extend_from_slice(&[1, u8::from(*v)])?;
            }
            Self::Integer(v) => {
                key.push(2)?;
                key.extend_from_slice(&v.to_le_bytes())?;
            }
            Self::Unsigned(v) => {
                key.push(6)?;
                key.extend_from_slice(&v.to_le_bytes())?;
            }
            Self::Decimal { value, .. } => {
                key.push(7)?;
                key.extend_from_slice(&value.to_le_bytes())?;
            }
            Self::Float(v) => {
                key.push(5)?;
                let bits = if *v == 0.0 {
                    0
                } else if v.is_nan() {
                    f32::NAN.to_bits()
                } else {
                    v.to_bits()
                };
                key.extend_from_slice(&bits.to_le_bytes())?;
            }
            Self::Double(v) => {
                key.push(3)?;
                let bits = if *v == 0.0 {
                    0
                } else if v.is_nan() {
                    f64::NAN.to_bits()
                } else {
                    v.to_bits()
                };
                key.extend_from_slice(&bits.to_le_bytes())?;
            }
            Self::Date(_)
            | Self::Blob(_)
            | Self::Uuid(_)
            | Self::Temporal(_)
            | Self::Extension(_) => {
                return Err(Error::Unsupported(
                    "registered type requires its selected key adapter".into(),
                ));
            }
            Self::Varchar(v) => {
                key.push(4)?;
                key.extend_from_slice(&(v.len() as u64).to_le_bytes())?;
                key.extend_from_slice(v.as_bytes())?;
            }
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn float_cmp(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(f, "NULL"),
            Self::Boolean(v) => write!(f, "{v}"),
            Self::Integer(v) => write!(f, "{v}"),
            Self::Unsigned(v) => write!(f, "{v}"),
            Self::Decimal { value, scale, .. } => super::numeric::format_decimal(*value, *scale, f),
            Self::Float(v) => write!(f, "{v}"),
            Self::Double(v) => write!(f, "{v}"),
            Self::Varchar(v) => write!(f, "{v}"),
            Self::Blob(v) => super::scalar::format_blob(v, f),
            Self::Uuid(v) => super::scalar::format_uuid(*v, f),
            Self::Date(v) => write!(f, "{v}"),
            Self::Temporal(v) => write!(f, "{v}"),
            Self::Extension(value) => write!(f, "{}({} bytes)", value.data_type, value.bytes.len()),
        }
    }
}

mod float_bits {
    use serde::{Deserialize, Deserializer, Serializer};
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    pub fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(value.to_bits())
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        Ok(f64::from_bits(u64::deserialize(deserializer)?))
    }
}

mod float32_bits {
    use serde::{Deserialize, Deserializer, Serializer};
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    pub fn serialize<S: Serializer>(value: &f32, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(value.to_bits())
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f32, D::Error> {
        Ok(f32::from_bits(u32::deserialize(deserializer)?))
    }
}
