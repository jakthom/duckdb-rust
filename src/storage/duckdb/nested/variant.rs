//! Pinned native VARIANT metadata. The public logical type remains dynamic;
//! these canonical fields describe its unshredded physical child streams only.
use super::*;

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
    Ok(())
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
        // A reader increment must not silently authorize legacy publication.
        assert!(matches!(
            write_type(&mut Encoder::default(), &NestedType::Variant.data_type()),
            Err(Error::Unsupported(_))
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
            super::super::super::writer::statistics(&mut output, Some(&unshredded_type()), &[])?;
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
                super::super::super::writer::statistics(&mut output, Some(&shredded), &[])?;
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
