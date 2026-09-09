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
    Float,
    Double,
    Varchar,
    Date,
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
            Self::Float => "builtin.float",
            Self::Double => "builtin.double",
            Self::Varchar => "builtin.varchar",
            Self::Date => "builtin.date",
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
        matches!(
            self,
            Self::TinyInt | Self::SmallInt | Self::Integer | Self::BigInt | Self::HugeInt
        )
    }

    pub fn is_numeric(&self) -> bool {
        self.is_integer() || self.is_floating()
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
            if *left == Self::Double || *right == Self::Double {
                return Ok(Self::Double);
            }
            if *left == Self::Float || *right == Self::Float {
                return Ok(Self::Float);
            }
            return Ok(if *left == Self::HugeInt || *right == Self::HugeInt {
                Self::HugeInt
            } else {
                Self::BigInt
            });
        }
        Err(Error::Bind(format!(
            "incompatible types {left} and {right}"
        )))
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
                Self::Float => "FLOAT",
                Self::Double => "DOUBLE",
                Self::Varchar => "VARCHAR",
                Self::Date => "DATE",
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
    Float(#[serde(with = "float32_bits")] f32),
    Double(#[serde(with = "float_bits")] f64),
    Varchar(String),
    Date(Date),
    Extension(Arc<ExtensionValue>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionValue {
    pub data_type: DataType,
    pub bytes: Vec<u8>,
}

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
            Self::Float(_) => DataType::Float,
            Self::Double(_) => DataType::Double,
            Self::Varchar(_) => DataType::Varchar,
            Self::Date(_) => DataType::Date,
            Self::Extension(value) => value.data_type.clone(),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Whether this physical value fits a declared type without conversion.
    /// NULL is a valid placeholder for every type; nullability is separate.
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
            Self::Extension(value) => {
                matches!(value.data_type, DataType::Extension(_))
                    && value.data_type == *data_type
                    && value.bytes.len() <= 16 * 1024 * 1024
            }
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

    pub fn as_i128(&self) -> Result<i128> {
        match self {
            Self::Integer(v) => Ok(*v),
            _ => Err(Error::Conversion(format!("{self} is not an integer"))),
        }
    }

    pub fn as_date(&self) -> Result<Date> {
        match self {
            Self::Date(date) => Ok(*date),
            _ => Err(Error::Conversion("value is not DATE".into())),
        }
    }

    pub fn as_f64(&self) -> Result<f64> {
        match self {
            Self::Integer(v) => Ok(*v as f64),
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
            (Self::Boolean(a), Self::Boolean(b)) => Ok(a.cmp(b)),
            (Self::Date(a), Self::Date(b)) => Ok(a.cmp(b)),
            (Self::Varchar(a), Self::Varchar(b)) => Ok(a.cmp(b)),
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
    pub(crate) fn append_primitive_key(&self, key: &mut Vec<u8>) -> Result<()> {
        match self {
            Self::Null => key.push(0),
            Self::Boolean(v) => {
                key.push(1);
                key.push(u8::from(*v));
            }
            Self::Integer(v) => {
                key.push(2);
                key.extend(v.to_le_bytes());
            }
            Self::Float(v) => {
                key.push(5);
                let bits = if *v == 0.0 {
                    0
                } else if v.is_nan() {
                    f32::NAN.to_bits()
                } else {
                    v.to_bits()
                };
                key.extend(bits.to_le_bytes());
            }
            Self::Double(v) => {
                key.push(3);
                let bits = if *v == 0.0 {
                    0
                } else if v.is_nan() {
                    f64::NAN.to_bits()
                } else {
                    v.to_bits()
                };
                key.extend(bits.to_le_bytes());
            }
            Self::Date(_) | Self::Extension(_) => {
                return Err(Error::Unsupported(
                    "registered type requires its selected key adapter".into(),
                ));
            }
            Self::Varchar(v) => {
                key.push(4);
                key.extend((v.len() as u64).to_le_bytes());
                key.extend(v.as_bytes());
            }
        }
        Ok(())
    }
}

fn float_cmp(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(f, "NULL"),
            Self::Boolean(v) => write!(f, "{v}"),
            Self::Integer(v) => write!(f, "{v}"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Double(v) => write!(f, "{v}"),
            Self::Varchar(v) => write!(f, "{v}"),
            Self::Date(v) => write!(f, "{v}"),
            Self::Extension(value) => write!(f, "{}({} bytes)", value.data_type, value.bytes.len()),
        }
    }
}

mod float_bits {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(value.to_bits())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        Ok(f64::from_bits(u64::deserialize(deserializer)?))
    }
}

mod float32_bits {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(value: &f32, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u32(value.to_bits())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f32, D::Error> {
        Ok(f32::from_bits(u32::deserialize(deserializer)?))
    }
}
