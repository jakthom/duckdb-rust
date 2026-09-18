//! Ordered ENUM dictionaries. Labels are byte/case-sensitive and may be empty;
//! dictionary identity is its ordered contents, not a catalog name or address.
use std::{collections::BTreeSet, sync::Arc};

use serde::{Deserialize, Serialize};

use super::{DataType, Error, Result, Value};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EnumType {
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumValue {
    pub data_type: Arc<EnumType>,
    pub ordinal: u32,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl EnumType {
    pub fn validate(&self) -> Result<()> {
        if self.labels.len() > u32::MAX as usize {
            return Err(Error::Resource(
                "ENUM exceeds the 32-bit ordinal domain".into(),
            ));
        }
        let mut seen = BTreeSet::new();
        let mut remaining: usize = 16 * 1024 * 1024;
        for label in &self.labels {
            remaining = remaining
                .checked_sub(label.len())
                .ok_or_else(|| Error::Resource("ENUM labels exceed 16 MiB".into()))?;
            if !seen.insert(label) {
                return Err(Error::InvalidInput(format!(
                    "Attempted to create ENUM type with duplicate value {label}"
                )));
            }
        }
        Ok(())
    }
    pub fn ordinal(&self, label: &str) -> Option<u32> {
        self.labels
            .iter()
            .position(|candidate| candidate == label)
            .and_then(|index| u32::try_from(index).ok())
    }
    /// Native enum widths reserve the all-ones ordinal for physical NULL.
    pub fn physical_width(&self) -> usize {
        if self.labels.len() <= u8::MAX as usize {
            1
        } else if self.labels.len() <= u16::MAX as usize {
            2
        } else {
            4
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl EnumValue {
    pub fn label(&self) -> Result<&str> {
        self.data_type
            .labels
            .get(self.ordinal as usize)
            .map(String::as_str)
            .ok_or_else(|| Error::Conversion("ENUM ordinal is outside its dictionary".into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl DataType {
    pub fn enumeration(labels: Vec<String>) -> Result<Self> {
        let metadata = EnumType { labels };
        metadata.validate()?;
        Ok(Self::Enum(Arc::new(metadata)))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Value {
    pub fn enumeration(data_type: &DataType, ordinal: u32) -> Result<Self> {
        let DataType::Enum(metadata) = data_type else {
            return Err(Error::Conversion(
                "ENUM value requires ENUM metadata".into(),
            ));
        };
        let value = EnumValue {
            data_type: metadata.clone(),
            ordinal,
        };
        value.label()?;
        Ok(Self::Enum(Arc::new(value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{
        cast::{CastMode, CastRegistry},
        type_registry::builtin_types,
    };
    use crate::parallel::QueryContext;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn enum_metadata_values_and_registered_casts_keep_label_order() -> Result<()> {
        let a = DataType::enumeration(vec!["b".into(), "a".into(), "A".into(), "".into()])?;
        let b = DataType::enumeration(vec!["a".into(), "b".into()])?;
        let types = builtin_types();
        let casts = CastRegistry::builtins();
        let query = QueryContext::background();
        let a_value = Value::enumeration(&a, 1)?;
        let b_value = Value::enumeration(&a, 0)?;
        assert_eq!(a_value.to_string(), "a");
        assert!(types.bind(&a)?.compare(&a_value, &b_value, &query)?.is_gt());
        assert_eq!(types.common_type(&a, &b)?, DataType::Varchar);
        assert_eq!(
            casts
                .bind(&a, &b, CastMode::Explicit, &types)?
                .apply(&a_value, &query)?,
            Value::enumeration(&b, 0)?
        );
        assert_eq!(
            casts
                .bind(&a, &DataType::Varchar, CastMode::Implicit, &types)?
                .apply(&a_value, &query)?,
            Value::Varchar("a".into())
        );
        assert!(
            casts
                .bind(&DataType::Varchar, &a, CastMode::Implicit, &types)
                .is_err()
        );
        assert!(Value::enumeration(&a, 4).is_err());
        assert!(DataType::enumeration(vec!["a".into(), "a".into()]).is_err());
        for (count, width) in [(0, 1), (255, 1), (256, 2), (65535, 2), (65536, 4)] {
            let metadata = EnumType {
                labels: (0..count).map(|n| n.to_string()).collect(),
            };
            assert_eq!(metadata.physical_width(), width);
        }
        let mut c = crate::Database::memory()?.connect();
        assert!(c.query("SELECT NULL::ENUM()").is_err());
        assert_eq!(c.query("SELECT 'a'::ENUM('b','a') < 'b'::ENUM('b','a'),'a'::ENUM('a','b') = 'a'::ENUM('b','a'),TRY_CAST('c' AS ENUM('a','b'))")?.rows,
            vec![vec![Value::Boolean(false),Value::Boolean(true),Value::Null]]);
        Ok(())
    }
}
