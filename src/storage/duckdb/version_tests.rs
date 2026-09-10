use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn rechecksum(bytes: &mut [u8]) -> Result<()> {
    let sum = checksum(&bytes[8..])?;
    bytes[..8].copy_from_slice(&sum.to_le_bytes());
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn storage_versions_distinguish_the_development_sentinel_from_legacy_serialization() -> Result<()> {
    let original = writer::encode(&Snapshot::new(crate::common::type_registry::builtin_types()))?;
    for (main, database, accepted) in [
        (64_u64, 0_u64, true),
        (64, 1, true),
        (65, 4, true),
        (67, 6, true),
        (68, 7, true),
        (64, 64, true),
        (64, 69, true),
        (999, 69, true),
        (69, 69, true),
        (999, 7, false),
        (999, 70, false),
        (68, 8, false),
        (70, 69, false),
        (63, 1, false),
    ] {
        let mut bytes = original.clone();
        bytes[12..20].copy_from_slice(&main.to_le_bytes());
        rechecksum(&mut bytes[..4096])?;
        for offset in [4096, 8192] {
            bytes[offset + 56..offset + 64].copy_from_slice(&database.to_le_bytes());
            rechecksum(&mut bytes[offset..offset + 4096])?;
        }
        let decoded =
            DuckDbFormat::default().decode(bytes, crate::common::type_registry::builtin_types());
        assert_eq!(
            decoded.is_ok(),
            accepted,
            "main={main}, database={database}"
        );
        if !accepted {
            assert!(matches!(decoded, Err(Error::Unsupported(_))));
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn development_qualified_catalog_metadata_round_trips_and_rejects_ambiguous_names() -> Result<()> {
    use super::binary::Encoder;
    for (path, table, accepted) in [
        (
            vec!["source", "analytics", "measurements"],
            "measurements",
            true,
        ),
        (
            vec!["source", "analytics", "different"],
            "measurements",
            false,
        ),
        (
            vec!["source", "outer", "inner", "measurements"],
            "measurements",
            false,
        ),
    ] {
        let mut output = Encoder::default();
        output.property(100, 1);
        output.property(105, 0);
        output.field(111);
        output.property(100, path.len() as u64);
        for part in path {
            output.string(part)?;
        }
        output.end();
        output.field(200);
        output.string(table)?;
        output.field(201);
        output.property(100, 1);
        writer::column_definition(
            &mut output,
            &crate::catalog::ColumnDefinition::new("n", crate::DataType::Integer),
        )?;
        output.end();
        output.end();
        let mut reader = Reader::new(output.0);
        let result = catalog::create_base(&mut reader, 1)
            .and_then(|name| catalog::table_definition(&mut reader, name));
        assert_eq!(result.is_ok(), accepted);
        if let Ok(definition) = result {
            assert_eq!(definition.name.schema, "analytics");
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn development_default_source_spans_do_not_change_the_value() -> Result<()> {
    use super::binary::Encoder;
    let mut output = Encoder::default();
    output.property(100, 7);
    output.property(101, 75);
    output.property(103, 123);
    output.property(104, 2);
    output.field(200);
    output.field(100);
    super::primitive::write_type(&mut output, &crate::DataType::Integer)?;
    output.field(101);
    output.boolean(false);
    output.field(102);
    output.signed(42);
    output.end();
    output.end();
    assert_eq!(
        catalog::constant_expression(&mut Reader::new(output.0))?,
        crate::Value::Integer(42)
    );
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn per_column_metadata_ownership_is_bounded_and_column_ordered() -> Result<()> {
    let blocks = Blocks::new(writer::encode(&Snapshot::new(
        crate::common::type_registry::builtin_types(),
    ))?)?;
    let marker = 1_u64 << 63;
    for (entries, accepted) in [
        (vec![], true),
        (vec![marker, 0, marker | 1, 0], true),
        (vec![0], false),
        (vec![marker | 2, 0], false),
        (vec![marker | 1, 0, marker, 0], false),
        (vec![marker, blocks.block_count], false),
        (vec![marker, 64_u64 << 56], false),
    ] {
        let mut output = binary::Encoder::default();
        output.property(107, entries.len() as u64);
        for entry in entries {
            output.unsigned(entry);
        }
        output.end();
        assert_eq!(
            columns::read_column_ownership(&mut Reader::new(output.0), &blocks, 2).is_ok(),
            accepted
        );
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn segment_byte_sizes_preserve_absence_and_reject_width_overflow() -> Result<()> {
    for (value, expected) in [(None, None), (Some(0), Some(0)), (Some(1234), Some(1234))] {
        let mut output = binary::Encoder::default();
        output.field(106);
        output.boolean(value.is_some());
        if let Some(value) = value {
            output.unsigned(value);
        }
        output.end();
        assert_eq!(
            columns::segment_byte_size(&mut Reader::new(output.0))?,
            expected
        );
    }
    let mut output = binary::Encoder::default();
    output.field(106);
    output.boolean(true);
    output.unsigned(u64::from(u32::MAX) + 1);
    output.end();
    assert!(matches!(
        columns::segment_byte_size(&mut Reader::new(output.0)),
        Err(crate::Error::Corrupt(_))
    ));
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parsed_native_types_resolve_known_names_without_executing_or_erasing_metadata() -> Result<()> {
    for (name, children, expected) in [
        ("TIMETZ", 0, Some(crate::DataType::TimeTz)),
        ("timestamp_ns", 0, Some(crate::DataType::TimestampNs)),
        ("application_type", 0, None),
        ("TIMETZ", 1, None),
    ] {
        let mut output = binary::Encoder::default();
        output.property(100, 4); // UNBOUND, not SQLNULL
        output.field(101);
        output.boolean(true);
        output.property(100, 7); // UnboundTypeInfo
        output.field(204);
        output.boolean(true);
        output.property(100, 21);
        output.property(101, 207); // TypeExpression
        output.field(202);
        output.string(name)?;
        output.property(203, children);
        output.field(204);
        output.property(100, 3);
        output.string("")?;
        output.string("")?;
        output.string(name)?;
        output.end();
        output.end();
        output.end();
        output.end();
        let decoded = catalog::logical_type(&mut Reader::new(output.0));
        if let Some(expected) = expected {
            assert_eq!(decoded?, expected);
        } else {
            assert!(matches!(decoded, Err(Error::Unsupported(_))));
        }
    }
    Ok(())
}
