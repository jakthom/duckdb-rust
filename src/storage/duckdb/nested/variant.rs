//! Pinned native VARIANT metadata. The public logical type remains dynamic;
//! these canonical fields describe its unshredded physical child streams only.
use super::*;
mod encoding;
mod payload;
mod shredded;
// Exact content traversal is an internal prerequisite for the format owner's
// layout-validation/publication seam, not a change to SQL VARIANT equality.
#[cfg(test)]
mod exact;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn encode_rows(
    values: &[Value],
    context: &crate::parallel::QueryContext,
) -> Result<Vec<Value>> {
    let selected = context.types().bind(&NestedType::Variant.data_type())?;
    encoding::encode_rows(values, &selected, context)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_column(
    blocks: &super::super::Blocks,
    decoders: &DecoderRegistry,
    reader: &mut Reader,
    count: usize,
    row_start: usize,
) -> Result<Vec<Value>> {
    let extra = if reader.optional(99)? && reader.boolean()? {
        reader.field(100)?;
        if reader.unsigned()? != 1 {
            return Err(corrupt("VARIANT column has non-VARIANT extra data"));
        }
        reader.field(101)?;
        let ty = logical_type_at(reader, 1)?;
        validate_shredded_metadata(&ty)?;
        reader.end()?;
        Some(ty)
    } else {
        None
    };
    super::super::columns::read_segments(
        blocks,
        decoders,
        reader,
        Some(&NestedType::Variant.data_type()),
        None,
        0,
        row_start,
    )?;
    reader.field(101)?;
    let validity =
        super::super::columns::read_column(blocks, decoders, reader, None, count, row_start)?;
    if validity
        .iter()
        .any(|value| !matches!(value, Value::Boolean(_)))
    {
        return Err(corrupt(
            "VARIANT root validity must contain explicit Boolean values",
        ));
    }
    reader.field(102)?;
    let unshredded = super::super::columns::read_column(
        blocks,
        decoders,
        reader,
        Some(&unshredded_type()),
        count,
        row_start,
    )?;
    let shredded = if let Some(ty) = &extra {
        reader.field(103)?;
        Some(super::super::columns::read_column(
            blocks,
            decoders,
            reader,
            Some(ty),
            count,
            row_start,
        )?)
    } else {
        None
    };
    reader.end()?;
    let mut budget = payload::Budget::new();
    let mut result = Vec::new();
    result
        .try_reserve_exact(count)
        .map_err(|_| Error::Resource("cannot allocate native VARIANT rows".into()))?;
    for (row, valid) in validity.iter().enumerate() {
        if *valid == Value::Boolean(false) {
            result.push(Value::Null);
            continue;
        }
        let unshredded = payload::Unshredded::new(&unshredded[row], &mut budget)?;
        // Native shredded-vector reconstruction uses VariantBuilder's
        // CollectObjectChildren(LEXICOGRAPHIC), including leftover subtrees.
        // Ordinary unshredded columns retain their stored member ordering.
        let unshredded = if extra.is_some() {
            unshredded.map(payload::Unshredded::with_ordered_objects)
        } else {
            unshredded
        };
        let value = if let (Some(ty), Some(shredded)) = (&extra, &shredded) {
            shredded::decode(ty, &shredded[row], unshredded.as_ref(), 0, &mut budget)?
                .unwrap_or((DataType::Null, Value::Null))
        } else {
            let value = unshredded
                .ok_or_else(|| corrupt("valid VARIANT row has no unshredded payload"))?
                .decode(0, &mut budget)?;
            if value.1.is_null() {
                return Err(corrupt(
                    "unshredded VARIANT root NULL must use root validity",
                ));
            }
            value
        };
        result.push(payload::envelope(value)?);
    }
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn fields() -> Vec<(String, DataType)> {
    vec![
        (
            "keys".into(),
            NestedType::List(DataType::Varchar).data_type(),
        ),
        (
            "children".into(),
            NestedType::List(
                NestedType::Struct(vec![
                    ("keys_index".into(), DataType::UInteger),
                    ("values_index".into(), DataType::UInteger),
                ])
                .data_type(),
            )
            .data_type(),
        ),
        (
            "values".into(),
            NestedType::List(
                NestedType::Struct(vec![
                    ("type_id".into(), DataType::UTinyInt),
                    ("byte_offset".into(), DataType::UInteger),
                ])
                .data_type(),
            )
            .data_type(),
        ),
        ("data".into(), DataType::Blob),
    ]
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn unshredded_type() -> DataType {
    NestedType::Struct(fields()).data_type()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate_shredded_metadata(data_type: &DataType) -> Result<()> {
    // The shredded tree is ordinary typed storage, never another dynamic
    // VARIANT column. Reject recursion back into a fresh statistics schema;
    // otherwise each payload could reset metadata's depth bound indefinitely.
    crate::common::type_registry::check_metadata(data_type)?;
    let mut pending = vec![data_type];
    while let Some(ty) = pending.pop() {
        if let DataType::Nested(metadata) = ty {
            if matches!(metadata.as_ref(), NestedType::Variant) {
                return Err(corrupt(
                    "VARIANT shredded storage contains dynamic VARIANT metadata",
                ));
            }
            pending.extend(metadata.children());
        }
    }
    shredded::validate(data_type)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_statistics(reader: &mut Reader) -> Result<()> {
    reader.field(200)?;
    // VariantStatsShreddingState: uninitialized, not shredded, shredded,
    // inconsistent. Only the shredded state owns an additional typed tree.
    let state = reader.unsigned()?;
    if state > 3 {
        return Err(corrupt("unknown VARIANT statistics shredding state"));
    }
    reader.field(225)?;
    super::super::columns::statistics(reader, Some(&unshredded_type()))?;
    if state == 2 {
        reader.field(230)?;
        let shredded = logical_type_at(reader, 1)?;
        validate_shredded_metadata(&shredded)?;
        reader.field(235)?;
        super::super::columns::statistics(reader, Some(&shredded))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn variant_metadata_preserves_dynamic_identity_and_rejects_noncanonical_children() -> Result<()>
    {
        let mut output = Encoder::default();
        write_type(&mut output, &unshredded_type())?;
        // Replace only the logical ID in this otherwise canonical STRUCT
        // metadata. VARIANT and STRUCT both carry StructTypeInfo on disk.
        assert_eq!(&output.0[..3], &[100, 0, 100]);
        output.0[2] = 109;
        let mut reader = Reader::new(output.0);
        assert_eq!(
            logical_type_at(&mut reader, 0)?,
            NestedType::Variant.data_type()
        );
        assert!(reader.finished());

        let mut malformed = fields();
        malformed[1].0 = "other".into();
        let mut output = Encoder::default();
        write_type(&mut output, &NestedType::Struct(malformed).data_type())?;
        output.0[2] = 109;
        assert!(matches!(
            logical_type_at(&mut Reader::new(output.0), 0),
            Err(Error::Corrupt(_))
        ));
        let mut output = Encoder::default();
        write_type(&mut output, &NestedType::Variant.data_type())?;
        let mut reader = Reader::new(output.0);
        assert_eq!(
            logical_type_at(&mut reader, 0)?,
            NestedType::Variant.data_type()
        );
        assert!(reader.finished());
        // Logical metadata support does not authorize an older checkpoint.
        assert!(matches!(
            super::super::super::write_support::checkpoint_type(
                &NestedType::Variant.data_type(),
                64
            ),
            Err(Error::InvalidInput(_))
        ));
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn variant_statistics_bound_shredding_state_and_typed_child_metadata() -> Result<()> {
        for state in 0..=3 {
            let mut output = Encoder::default();
            output.property(200, state);
            output.field(225);
            super::super::super::writer::statistics(
                &mut output,
                Some(&unshredded_type()),
                &[],
                &crate::parallel::QueryContext::background(),
            )?;
            if state == 2 {
                let shredded = NestedType::Struct(vec![
                    (
                        "typed_value".into(),
                        DataType::Decimal {
                            width: 12,
                            scale: 2,
                        },
                    ),
                    ("untyped_value_index".into(), DataType::UInteger),
                ])
                .data_type();
                output.field(230);
                write_type(&mut output, &shredded)?;
                output.field(235);
                super::super::super::writer::statistics(
                    &mut output,
                    Some(&shredded),
                    &[],
                    &crate::parallel::QueryContext::background(),
                )?;
            }
            let mut reader = Reader::new(output.0);
            read_statistics(&mut reader)?;
            assert!(reader.finished());
        }
        let mut output = Encoder::default();
        output.property(200, 4);
        assert!(matches!(
            read_statistics(&mut Reader::new(output.0)),
            Err(Error::Corrupt(_))
        ));
        assert!(matches!(
            validate_shredded_metadata(
                &NestedType::List(NestedType::Variant.data_type()).data_type()
            ),
            Err(Error::Corrupt(_))
        ));
        Ok(())
    }
}
