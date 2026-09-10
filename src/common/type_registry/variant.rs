//! VARIANT owns cross-category normalization; child validation and same-type
//! primitive comparisons use the registry snapshot selected when bound.
use std::{cmp::Ordering, sync::Arc};

use super::{KeyWriter, TypeAdapter, TypeRegistry};
use crate::{
    common::{
        DataType, Error, NestedPayload, NestedType, Result, Value,
        variant::{Node, invalid},
    },
    parallel::QueryContext,
};
#[cfg(test)]
mod tests;

#[derive(Debug, Default)]
pub struct VariantType {
    types: Option<TypeRegistry>,
}

#[derive(Debug, PartialEq, Eq)]
struct NumberKey {
    class: u8,
    exponent: i64,
    digits: String,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl NumberKey {
    fn new(value: &Value) -> Result<Self> {
        let (negative, magnitude, scale) = match value {
            Value::Integer(value) => (*value < 0, value.unsigned_abs(), 0),
            Value::Unsigned(value) => (false, *value, 0),
            Value::Decimal { value, scale, .. } => (*value < 0, value.unsigned_abs(), *scale),
            _ => return Err(invalid()),
        };
        if magnitude == 0 {
            return Ok(Self {
                class: 1,
                exponent: 0,
                digits: String::new(),
            });
        }
        let digits = magnitude.to_string();
        Ok(Self {
            class: if negative { 0 } else { 2 },
            exponent: digits.len() as i64 - 1 - i64::from(scale),
            digits: digits.trim_end_matches('0').to_owned(),
        })
    }
    fn compare(&self, right: &Self) -> Ordering {
        self.class.cmp(&right.class).then_with(|| {
            let magnitude = self
                .exponent
                .cmp(&right.exponent)
                .then_with(|| self.digits.cmp(&right.digits));
            if self.class == 0 {
                magnitude.reverse()
            } else {
                magnitude
            }
        })
    }
    fn write(&self, output: &mut KeyWriter<'_>) -> Result<()> {
        output.push(self.class)?;
        output.extend_from_slice(&self.exponent.to_le_bytes())?;
        output.extend_from_slice(&(self.digits.len() as u64).to_le_bytes())?;
        output.extend_from_slice(self.digits.as_bytes())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn temporal_key(node: Node<'_>) -> Result<i128> {
    match node {
        Node::Typed(_, Value::Date(date)) => Ok(i128::from(date.days()) * 86_400_000_000_000),
        Node::Typed(ty, Value::Temporal(value)) => {
            let key = value.comparison_key();
            Ok(if let Some(precision) = ty.timestamp_precision() {
                // Match the development VARIANT comparator, including its raw
                // infinity sentinel scaling rather than ordinary timestamp casts.
                key * (1_000_000_000 / i128::from(precision))
            } else if *ty == DataType::Time {
                key * 1000
            } else {
                key
            })
        }
        _ => Err(invalid()),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn primitive(node: Node<'_>) -> Result<(&DataType, &Value)> {
    match node {
        Node::Typed(ty, value) => Ok((ty, value)),
        _ => Err(invalid()),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn string(node: Node<'_>) -> Result<&str> {
    match primitive(node)?.1 {
        Value::Varchar(value) => Ok(value),
        Value::Enum(value) => value.label(),
        _ => Err(invalid()),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl VariantType {
    fn types(&self) -> Result<&TypeRegistry> {
        self.types
            .as_ref()
            .ok_or_else(|| Error::Internal("unbound VARIANT adapter".into()))
    }
    fn compare_node(
        &self,
        left: Node<'_>,
        right: Node<'_>,
        query: &QueryContext,
        depth: usize,
    ) -> Result<Ordering> {
        query.check()?;
        if depth > 64 {
            return Err(Error::Resource(
                "VARIANT comparison depth exceeds 64".into(),
            ));
        }
        let (left, right) = (left.resolved()?, right.resolved()?);
        let (rank, other_rank) = (left.rank()?, right.rank()?);
        if rank != other_rank {
            return Ok(rank.cmp(&other_rank));
        }
        if rank == 16 {
            return Ok(Ordering::Equal);
        }
        if rank < 14 {
            let ((lt, lv), (rt, rv)) = (primitive(left)?, primitive(right)?);
            if lt == rt && !matches!(lt, DataType::Enum(_)) {
                return self.types()?.bind(lt)?.compare(lv, rv, query);
            }
        }
        match rank {
            2 => {
                Ok(NumberKey::new(primitive(left)?.1)?
                    .compare(&NumberKey::new(primitive(right)?.1)?))
            }
            3 => self.types()?.bind(&DataType::Double)?.compare(
                &Value::Double(primitive(left)?.1.as_f64()?),
                &Value::Double(primitive(right)?.1.as_f64()?),
                query,
            ),
            4 => self.types()?.bind(&DataType::Varchar)?.compare(
                &Value::Varchar(string(left)?.to_owned()),
                &Value::Varchar(string(right)?.to_owned()),
                query,
            ),
            7..=11 => Ok(temporal_key(left)?.cmp(&temporal_key(right)?)),
            14 => {
                let (left, right) = (left.array()?, right.array()?);
                query.check_rows(left.len().max(right.len()))?;
                for (left, right) in left.iter().zip(&right) {
                    let order = self.compare_node(*left, *right, query, depth + 1)?;
                    if order != Ordering::Equal {
                        return Ok(order);
                    }
                }
                Ok(left.len().cmp(&right.len()))
            }
            15 => {
                let (mut left, mut right) = (left.object()?, right.object()?);
                query.check_rows(left.len().max(right.len()))?;
                left.sort_by(|a, b| a.0.cmp(b.0));
                right.sort_by(|a, b| a.0.cmp(b.0));
                for ((lk, lv), (rk, rv)) in left.iter().zip(&right) {
                    let order = lk.cmp(rk);
                    if order != Ordering::Equal {
                        return Ok(order);
                    }
                    let order = self.compare_node(*lv, *rv, query, depth + 1)?;
                    if order != Ordering::Equal {
                        return Ok(order);
                    }
                }
                Ok(left.len().cmp(&right.len()))
            }
            _ => Err(invalid()),
        }
    }
    fn write_node(
        &self,
        node: Node<'_>,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
        depth: usize,
    ) -> Result<()> {
        query.check()?;
        if depth > 64 {
            return Err(Error::Resource("VARIANT key depth exceeds 64".into()));
        }
        let node = node.resolved()?;
        let rank = node.rank()?;
        output.push(rank)?;
        match rank {
            16 => Ok(()),
            2 => NumberKey::new(primitive(node)?.1)?.write(output),
            3 => {
                let mut bytes = Vec::new();
                self.types()?.bind(&DataType::Double)?.append_key(
                    &Value::Double(primitive(node)?.1.as_f64()?),
                    &mut bytes,
                    query,
                )?;
                output.extend_from_slice(&bytes)
            }
            4 => {
                let mut bytes = Vec::new();
                self.types()?.bind(&DataType::Varchar)?.append_key(
                    &Value::Varchar(string(node)?.to_owned()),
                    &mut bytes,
                    query,
                )?;
                output.extend_from_slice(&bytes)
            }
            7..=11 => output.extend_from_slice(&temporal_key(node)?.to_le_bytes()),
            14 => {
                let values = node.array()?;
                query.check_rows(values.len())?;
                output.extend_from_slice(&(values.len() as u64).to_le_bytes())?;
                for value in values {
                    self.write_node(value, output, query, depth + 1)?;
                }
                Ok(())
            }
            15 => {
                let mut values = node.object()?;
                query.check_rows(values.len())?;
                values.sort_by(|a, b| a.0.cmp(b.0));
                output.extend_from_slice(&(values.len() as u64).to_le_bytes())?;
                for (key, value) in values {
                    output.extend_from_slice(&(key.len() as u64).to_le_bytes())?;
                    output.extend_from_slice(key.as_bytes())?;
                    self.write_node(value, output, query, depth + 1)?;
                }
                Ok(())
            }
            _ => {
                let (ty, value) = primitive(node)?;
                let mut bytes = Vec::new();
                self.types()?
                    .bind(ty)?
                    .append_key(value, &mut bytes, query)?;
                output.extend_from_slice(&bytes)
            }
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for VariantType {
    fn name(&self) -> &'static str {
        "dynamic-variant-type"
    }
    fn supports_index(&self, _: &DataType) -> bool {
        false
    }
    fn bind_type(
        &self,
        _: &DataType,
        types: &TypeRegistry,
    ) -> Result<Option<Arc<dyn TypeAdapter>>> {
        Ok(Some(Arc::new(Self {
            types: Some(types.clone()),
        })))
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        if *ty == NestedType::Variant.data_type() {
            Ok(())
        } else {
            Err(invalid())
        }
    }
    fn validate_value(&self, _: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        query.check()?;
        let Value::Nested(value) = value else {
            return Err(invalid());
        };
        let NestedPayload::Variant { data_type, value } = &value.payload else {
            return Err(invalid());
        };
        check_categories(data_type)?;
        self.types()?.bind(data_type)?.validate(value, query)?;
        if Node::Typed(data_type, value).resolved()?.rank()? == 16 {
            return Err(Error::Conversion(
                "VARIANT_NULL must use SQL NULL validity".into(),
            ));
        }
        Ok(())
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        if left.family() == "builtin.variant" || right.family() == "builtin.variant" {
            Ok(Some(NestedType::Variant.data_type()))
        } else {
            Ok(None)
        }
    }
    fn compare(
        &self,
        ty: &DataType,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        self.compare_node(Node::Typed(ty, left), Node::Typed(ty, right), query, 0)
    }
    fn write_key(
        &self,
        ty: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        self.write_node(Node::Typed(ty, value), output, query, 0)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn check_categories(ty: &DataType) -> Result<()> {
    super::check_metadata(ty)?;
    let mut pending = vec![ty];
    while let Some(ty) = pending.pop() {
        match ty {
            DataType::Extension(_) => {
                return Err(Error::Unsupported(format!(
                    "VARIANT category normalization for {ty}"
                )));
            }
            DataType::Nested(metadata) => pending.extend(metadata.children()),
            _ => {}
        }
    }
    Ok(())
}
