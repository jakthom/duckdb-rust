//! Canonical payload bridge for WAL vectors. The vector framing remains in the
//! WAL family; this module shares exact tags and payload handling with native
//! checkpoint columns without enabling either publication capability.
use super::*;
use crate::{common::type_registry::BoundType, parallel::QueryContext};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn data_type() -> DataType {
    unshredded_type()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn encode(
    values: &[Value],
    selected: &BoundType,
    depth: usize,
    remaining: &mut usize,
    query: &QueryContext,
) -> Result<Vec<Value>> {
    let mut limits = encoding::Limits {
        nodes: *remaining,
        bytes: 64 * 1024 * 1024,
    };
    let result = encoding::encode_with_limits(values, selected, query, &mut limits, depth)?;
    // Reader::blob bounds an individual serialized byte array at 16 MiB.
    // The canonical builder's 64 MiB total budget alone does not guarantee
    // that one row's data buffer (or one key) is readable by the WAL codec.
    check_wire_bytes(&result, 16_777_216, query)?;
    *remaining = limits.nodes;
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn check_wire_bytes(values: &[Value], limit: usize, query: &QueryContext) -> Result<()> {
    for value in values {
        query.check()?;
        if value.is_null() {
            continue;
        }
        let [keys, _, _, Value::Blob(data)] = payload::record(value)? else {
            return Err(Error::Internal("canonical WAL VARIANT row shape".into()));
        };
        if data.len() > limit {
            return Err(Error::Resource(
                "WAL VARIANT row data exceeds 16 MiB".into(),
            ));
        }
        for key in payload::sequence(keys)? {
            query.check()?;
            let Value::Varchar(key) = key else {
                return Err(Error::Internal("canonical WAL VARIANT key shape".into()));
            };
            if key.len() > limit {
                return Err(Error::Resource(
                    "WAL VARIANT object key exceeds 16 MiB".into(),
                ));
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn decode(
    values: &[Value],
    selected: &BoundType,
    depth: usize,
    remaining: &mut usize,
    query: &QueryContext,
) -> Result<Vec<Value>> {
    query.check()?;
    if selected.data_type() != &NestedType::Variant.data_type() {
        return Err(Error::Internal(
            "WAL VARIANT decoder requires selected VARIANT type".into(),
        ));
    }
    let mut budget = payload::Budget::with_context(*remaining, query);
    budget.nodes(values.len())?;
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        query.check()?;
        let decoded = if let Some(value) = payload::Unshredded::new(value, &mut budget)? {
            let child = value.decode_from(0, depth, &mut budget)?;
            if child.1.is_null() {
                return Err(corrupt("WAL VARIANT root NULL must use root validity"));
            }
            payload::envelope(child)?
        } else {
            Value::Null
        };
        selected.validate(&decoded, query)?;
        result.push(decoded);
    }
    *remaining = budget.nodes;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn canonical_wal_rows_bound_individual_wire_blobs_not_only_total_materialization() -> Result<()>
    {
        let query = QueryContext::background();
        let selected = query.types().bind(&NestedType::Variant.data_type())?;
        let value = NestedValue::value(
            NestedType::Variant.data_type(),
            NestedPayload::Variant {
                data_type: DataType::Varchar,
                value: Value::Varchar("abcd".into()),
            },
        )?;
        let row = encode(&[value], &selected, 0, &mut { 16_777_216 }, &query)?;
        assert!(matches!(
            check_wire_bytes(&row, 4, &query),
            Err(Error::Resource(_))
        ));
        check_wire_bytes(&row, 5, &query)?;
        let object = NestedType::Object(vec![("long_key".into(), DataType::Boolean)]).data_type();
        let value = NestedValue::value(
            NestedType::Variant.data_type(),
            NestedPayload::Variant {
                data_type: object.clone(),
                value: NestedValue::value(
                    object,
                    NestedPayload::Struct(vec![Value::Boolean(true)]),
                )?,
            },
        )?;
        let row = encode(&[value], &selected, 0, &mut { 16_777_216 }, &query)?;
        assert!(matches!(
            check_wire_bytes(&row, 7, &query),
            Err(Error::Resource(_))
        ));
        check_wire_bytes(&row, 8, &query)?;
        Ok(())
    }
}
