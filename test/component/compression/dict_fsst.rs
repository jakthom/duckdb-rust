use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn empty_validity_explicitly_preserves_base_nulls_and_obeys_dispatch_limits() -> Result<()> {
    let registry = compression::decoders();
    let query = QueryContext::background();
    for count in [0, 1, 2049] {
        assert_eq!(
            read(&registry, 14, &[], count, SegmentType::Validity, &query)?,
            vec![Value::Null; count]
        );
    }
    assert!(matches!(
        read(
            &registry,
            14,
            &[],
            1,
            SegmentType::Values(&DataType::Blob),
            &query
        ),
        Err(Error::Unsupported(_))
    ));
    let interrupt = InterruptHandle::default();
    let limited = QueryContext::new(interrupt.clone(), None, 8, 2)?;
    assert!(matches!(
        read(&registry, 14, &[], 3, SegmentType::Validity, &limited),
        Err(Error::Resource(_))
    ));
    interrupt.interrupt();
    assert!(matches!(
        read(&registry, 14, &[], 1, SegmentType::Validity, &limited),
        Err(Error::Interrupted)
    ));
    // The registry's foreign-output regression separately checks that a default
    // adapter emitting the same NULL marker is rejected rather than authorized.
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independently_written_dict_fsst_modes_survive_sql_mutation_rollback_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for name in ["dictionary", "combined", "unique"] {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "test/data/duckdb/dict-fsst-development/dict_fsst_{name}.duckdb.gz"
        ));
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(std::fs::File::open(source)?).read_to_end(&mut bytes)?;
        let path = directory.path().join(format!("{name}.duckdb"));
        std::fs::write(&path, &bytes)?;
        let expected = (0..10013)
            .map(|i| {
                let value = match name {
                    "dictionary" if i % 11 == 0 => Value::Null,
                    "dictionary" if i % 13 == 0 => Value::Varchar(String::new()),
                    "dictionary" => Value::Varchar(format!("category-{}", i % 7)),
                    "combined" if i % 29 == 0 => Value::Null,
                    "combined" if i % 31 == 0 => Value::Varchar(String::new()),
                    "combined" => {
                        Value::Varchar(format!("{}{}", "duckdb-scalar-🦆-".repeat(8), i % 5003))
                    }
                    _ => Value::Varchar(format!("{}{i}", "unique-scalar-".repeat(8))),
                };
                vec![Value::Integer(i), value]
            })
            .collect::<Vec<_>>();
        {
            let mut connection = duckdb_rust::Database::open_read_only(&path)?.connect();
            let result = connection.query("SELECT id,text FROM t ORDER BY id")?;
            assert_eq!(result.columns[1].data_type, DataType::Varchar);
            assert_eq!(result.rows, expected, "{name} independent decode");
        }
        assert_eq!(
            std::fs::read(&path)?,
            bytes,
            "read-only mode changed producer file"
        );
        {
            let mut connection = duckdb_rust::Database::open(&path)?.connect();
            connection.execute("BEGIN; UPDATE t SET text='aborted'; DELETE FROM t; ROLLBACK")?;
            assert_eq!(
                connection.query("SELECT id,text FROM t ORDER BY id")?.rows,
                expected
            );
            connection
                .execute("UPDATE t SET text='committed' WHERE id=1; DELETE FROM t WHERE id=2")?;
        }
        let mut after = expected;
        after[1][1] = Value::Varchar("committed".into());
        after.remove(2);
        let mut connection = duckdb_rust::Database::open_read_only(&path)?.connect();
        assert_eq!(
            connection.query("SELECT id,text FROM t ORDER BY id")?.rows,
            after,
            "{name} checkpoint reopen"
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn segment(mode: u8, entries: &[&[u8]], indices: &[u128]) -> Vec<u8> {
    // Independent layout oracle from pinned development's native header: three
    // u32 fields at 0/4/12 and mode/length-width/index-width bytes at 8/9/10.
    let lengths: Vec<u128> = std::iter::once(0)
        .chain(entries.iter().map(|entry| entry.len() as u128))
        .collect();
    let length_width = (128 - lengths.iter().copied().max().unwrap_or(0).leading_zeros()) as usize;
    let index_width = if mode == 2 {
        0
    } else {
        (usize::BITS - entries.len().leading_zeros()) as usize
    };
    let mut symbols = Vec::new();
    if mode != 0 {
        symbols.extend((20_190_218_u64 << 32).to_le_bytes());
        symbols.extend([0, 1, 1, 0, 0, 0, 0, 0, 0]);
        symbols.extend(b"abA"); // symbol 0="ab"; symbol 1="A"
    }
    let mut bytes = vec![0; 16];
    let pool: Vec<u8> = entries
        .iter()
        .flat_map(|entry| entry.iter().copied())
        .collect();
    bytes[0..4].copy_from_slice(&(pool.len() as u32).to_le_bytes());
    bytes[4..8].copy_from_slice(&((entries.len() + 1) as u32).to_le_bytes());
    bytes[8] = mode;
    bytes[9] = length_width as u8;
    bytes[10] = index_width as u8;
    bytes[12..16].copy_from_slice(&(symbols.len() as u32).to_le_bytes());
    bytes.extend(pool);
    bytes.resize(bytes.len().div_ceil(8) * 8, 0);
    bytes.extend(symbols);
    bytes.resize(bytes.len().div_ceil(8) * 8, 0);
    bytes.extend(pack(&lengths, length_width));
    bytes.resize(bytes.len().div_ceil(8) * 8, 0);
    bytes.extend(pack(indices, index_width));
    bytes
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_dict_fsst_modes_preserve_nulls_empty_strings_binary_bytes_and_packed_tails() -> Result<()>
{
    let registry = compression::decoders();
    let query = QueryContext::background();
    for mode in [0, 1, 2] {
        let plain: &[&[u8]] = &[b"ab", b"A\0", b""];
        let encoded: &[&[u8]] = &[&[0], &[1, 255, 0], &[]];
        let indices: Vec<u128> = if mode == 2 {
            vec![1, 2, 3]
        } else {
            (0..65).map(|n| n % 4).collect()
        };
        let bytes = segment(mode, if mode == 0 { plain } else { encoded }, &indices);
        for data_type in [DataType::Varchar, DataType::Blob] {
            let expected = indices
                .iter()
                .map(|&index| {
                    if index == 0 {
                        Value::Null
                    } else if data_type == DataType::Blob {
                        Value::Blob(plain[index as usize - 1].to_vec())
                    } else {
                        Value::Varchar(
                            String::from_utf8(plain[index as usize - 1].to_vec()).unwrap(),
                        )
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(
                read(
                    &registry,
                    15,
                    &bytes,
                    indices.len(),
                    SegmentType::Values(&data_type),
                    &query
                )?,
                expected
            );
        }
    }
    let bytes = segment(1, &[&[255, 255]], &[1]);
    assert_eq!(
        read(
            &registry,
            15,
            &bytes,
            1,
            SegmentType::Values(&DataType::Blob),
            &query
        )?,
        vec![Value::Blob(vec![255])]
    );
    assert!(matches!(
        read(
            &registry,
            15,
            &bytes,
            1,
            SegmentType::Values(&DataType::Varchar),
            &query
        ),
        Err(Error::Corrupt(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_dict_fsst_rejects_malformed_headers_tables_lengths_codes_and_indices() -> Result<()> {
    let registry = compression::decoders();
    let query = QueryContext::background();
    let read = |bytes: &[u8], count| {
        super::read(
            &registry,
            15,
            bytes,
            count,
            SegmentType::Values(&DataType::Varchar),
            &query,
        )
    };
    for mode in [0, 1, 2] {
        let bytes = segment(mode, &[&[0], &[1, 255, 0]], &[1, 2]);
        for end in 0..bytes.len() {
            assert!(read(&bytes[..end], 2).is_err(), "mode {mode}, end {end}");
        }
        for (offset, value) in [(8, 3), (9, 33), (10, 33)] {
            let mut bad = bytes.clone();
            bad[offset] = value;
            assert!(matches!(read(&bad, 2), Err(Error::Corrupt(_))));
        }
        let mut bad = bytes.clone();
        bad[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(read(&bad, 2), Err(Error::Corrupt(_))));
        let mut bad = bytes.clone();
        bad[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(read(&bad, 2), Err(Error::Corrupt(_))));
        if mode != 0 {
            let symbol_start = (16 + 4_usize).div_ceil(8) * 8;
            let mut bad = bytes.clone();
            bad[symbol_start + 4] = 0;
            assert!(matches!(read(&bad, 2), Err(Error::Corrupt(_))));
            let mut bad = bytes.clone();
            bad[symbol_start + 9] = 255;
            bad[symbol_start + 10] = 255;
            assert!(matches!(read(&bad, 2), Err(Error::Corrupt(_))));
        }
    }
    for entries in [&[&[255][..]][..], &[&[2][..]][..]] {
        assert!(matches!(
            read(&segment(1, entries, &[1]), 1),
            Err(Error::Corrupt(_))
        ));
    }
    assert!(matches!(
        read(&segment(0, &[b"a", b"b"], &[3]), 2),
        Err(Error::Corrupt(_))
    ));
    let bytes = segment(0, &[b"a"], &[1]);
    let interrupt = InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 8, 2)?;
    interrupt.interrupt();
    assert!(matches!(
        super::read(
            &registry,
            15,
            &bytes,
            1,
            SegmentType::Values(&DataType::Varchar),
            &query
        ),
        Err(Error::Interrupted)
    ));
    Ok(())
}
