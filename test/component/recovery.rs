use std::{fs, io::Read, path::PathBuf, sync::Arc};

use duckdb_rust::{
    Database, DatabaseBuilder, Error, Result, Value,
    catalog::{Catalog, CatalogMut, ColumnDefinition, TableDefinition, TableName, UniqueKey},
    common::DataType,
    execution::index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
    main::QueryResult,
    parallel::{InterruptHandle, QueryContext},
    storage::{
        TableStorage, TableStorageMut,
        checkpoint::FileCheckpoint,
        duckdb::{DuckDbFormat, compression, wal::DuckDbWalRecovery},
        filesystem::{LocalCheckpointStorage, OpenMode},
        format::{JsonSnapshotFormat, SnapshotFormat},
        recovery::{RecoveredChange as Change, Recovery, RecoveryInput, RecoveryTarget},
        table::Snapshot,
    },
};
use serde_json::{Value as Json, json};

#[path = "recovery/publication.rs"]
mod publication;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test/data/wal")
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn fixture(name: &str, suffix: &str) -> Vec<u8> {
    let bytes = fs::read(root().join(format!("{name}.{suffix}.gz"))).unwrap();
    let mut result = Vec::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .read_to_end(&mut result)
        .unwrap();
    result
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn manifest() -> Json {
    serde_json::from_slice(&fs::read(root().join("manifest.json")).unwrap()).unwrap()
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn recover(name: &str, log: Vec<u8>, context: &QueryContext) -> Result<Snapshot> {
    DuckDbWalRecovery.recover(
        RecoveryInput {
            checkpoint: fixture(name, "duckdb"),
            log,
        },
        &DuckDbFormat::default(),
        context,
    )
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn json_rows(result: QueryResult) -> Json {
    Json::Array(
        result
            .rows
            .into_iter()
            .map(|row| {
                let values = row.into_iter().map(|value| match value {
                    Value::Null => Json::Null,
                    Value::Integer(v) => serde_json::from_str(&v.to_string()).unwrap(),
                    Value::Boolean(v) => json!(v),
                    Value::Varchar(v) => json!(v),
                    Value::Double(v) if v.is_finite() => json!(v),
                    Value::Double(v) => json!(if v.is_nan() {
                        "NaN"
                    } else if v.is_sign_negative() {
                        "-inf"
                    } else {
                        "inf"
                    }),
                    other => panic!("unhandled fixture value: {other:?}"),
                });
                Json::Object(
                    result
                        .columns
                        .iter()
                        .map(|c| c.name.clone())
                        .zip(values)
                        .collect(),
                )
            })
            .collect(),
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn interrupted_reference_logs_recover_through_selected_adapters_and_keep_files_unchanged()
-> Result<()> {
    let manifest = manifest();
    for (name, case) in manifest["cases"].as_object().unwrap() {
        let checkpoint = fixture(name, "duckdb");
        let log = fixture(name, "wal");
        for scalar in [false, true] {
            for indexes in [
                Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
                Arc::new(BTreeIndexFactory),
            ] {
                let directory = tempfile::tempdir()?;
                let path = directory.path().join("recovery.duckdb");
                let wal_path = directory.path().join("recovery.duckdb.wal");
                fs::write(&path, &checkpoint)?;
                for state in case["states"].as_array().unwrap() {
                    let end = state["end"].as_u64().unwrap() as usize;
                    fs::write(&wal_path, &log[..end])?;
                    for _ in 0..2 {
                        let mut decoders = compression::decoders();
                        if scalar {
                            decoders.replace(Arc::new(compression::ScalarBitPackingDecoder))?;
                        }
                        let durability = FileCheckpoint::open(
                            &path,
                            OpenMode::ReadOnly,
                            Arc::new(DuckDbFormat::new(decoders)),
                        )?
                        .with_recovery(Arc::new(DuckDbWalRecovery))?;
                        let database = DatabaseBuilder::new()
                            .indexes(indexes.clone())
                            .durability(Arc::new(durability))
                            .build()?;
                        let mut connection = database.connect();
                        if !state["rows"].is_null() {
                            assert_eq!(
                                json_rows(connection.query(case["query"].as_str().unwrap())?),
                                state["rows"],
                                "{name} at {end}"
                            );
                        } else {
                            assert!(connection.query("SELECT * FROM extra.t").is_err());
                        }
                        assert!(
                            connection
                                .execute("CREATE TABLE forbidden(i INTEGER)")
                                .is_err()
                        );
                    }
                    assert_eq!(fs::read(&path)?, checkpoint);
                    assert_eq!(fs::read(&wal_path)?, log[..end]);
                }
            }
        }
        // Preserve the actual physical identities observed independently by
        // DuckDB, including deletion holes and append high-water marks.
        for state in case["states"].as_array().unwrap() {
            if state["identities"].is_null() {
                continue;
            }
            let snapshot = recover(
                name,
                log[..state["end"].as_u64().unwrap() as usize].to_vec(),
                &QueryContext::background(),
            )?;
            let table = if name == "create" {
                TableName::new("extra", "t")
            } else {
                TableName::main("t")
            };
            let expected: Vec<_> = state["identities"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    (
                        r["row_id"].as_u64().unwrap(),
                        r[if name == "create" { "id" } else { "i" }]
                            .as_i64()
                            .unwrap(),
                    )
                })
                .collect();
            let mut actual: Vec<_> = snapshot
                .scan(&table, &QueryContext::background())?
                .into_iter()
                .map(|(id, row)| (id, row[0].as_i128().unwrap() as i64))
                .collect();
            actual.sort_by_key(|(_, i)| *i);
            assert_eq!(actual, expected, "physical identities in {name}");
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn every_framed_tail_truncation_exposes_only_complete_transactions() -> Result<()> {
    let manifest = manifest();
    let case = &manifest["cases"]["mutations"];
    let log = fixture("mutations", "wal");
    let first_end = case["states"][1]["end"].as_u64().unwrap() as usize;
    let baseline = recover("mutations", Vec::new(), &QueryContext::background())?
        .scan(&TableName::main("t"), &QueryContext::background())?;
    let committed = recover(
        "mutations",
        log[..first_end].to_vec(),
        &QueryContext::background(),
    )?
    .scan(&TableName::main("t"), &QueryContext::background())?;
    // All byte boundaries after the complete unframed version header.
    let boundaries = log.len().saturating_sub(8);
    for (completed, end) in (8..log.len()).enumerate() {
        let snapshot = recover(
            "mutations",
            log[..end].to_vec(),
            &QueryContext::background(),
        )?;
        let rows = snapshot.scan(&TableName::main("t"), &QueryContext::background())?;
        assert_eq!(
            rows,
            if end < first_end {
                baseline.clone()
            } else {
                committed.clone()
            },
            "torn at byte {end}"
        );
        let completed = completed + 1;
        if completed == 1 || completed % 128 == 0 || completed == boundaries {
            eprintln!("recovery truncation sweep: {completed}/{boundaries} boundaries");
        }
    }
    Ok(())
}

// Fixture framing is deliberately independent of the engine's decoder.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn checksum(data: &[u8]) -> u64 {
    let mut value = 5381;
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        value ^= u64::from_le_bytes(chunk.try_into().unwrap()).wrapping_mul(0xbf58476d1ce4e5b9);
    }
    let rest = chunks.remainder();
    if !rest.is_empty() {
        let multiplier = 0xc6a4a7935bd1e995u64;
        let mut word = [0; 8];
        word[..rest.len()].copy_from_slice(rest);
        let mut h =
            0xe17a1465 ^ (rest.len() as u64).wrapping_mul(multiplier) ^ u64::from_le_bytes(word);
        h = h.wrapping_mul(multiplier);
        h ^= h >> 47;
        h = h.wrapping_mul(multiplier);
        h ^= h >> 47;
        value ^= h;
    }
    value
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn frame(payload: &[u8]) -> Vec<u8> {
    let mut bytes = (payload.len() as u64).to_le_bytes().to_vec();
    bytes.extend(checksum(payload).to_le_bytes());
    bytes.extend(payload);
    bytes
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn corrupt_or_incompatible_logs_fail_without_returning_partial_state() -> Result<()> {
    let log = fixture("mutations", "wal");
    let mut offset = 8;
    while offset < log.len() {
        let size = u64::from_le_bytes(log[offset..offset + 8].try_into().unwrap()) as usize;
        let mut damaged = log.clone();
        damaged[offset + 16] ^= 1;
        assert!(matches!(
            recover("mutations", damaged, &QueryContext::background()),
            Err(Error::Corrupt(_))
        ));
        offset += 16 + size;
    }
    let mut bad = log.clone();
    bad[5] = 3;
    assert!(matches!(
        recover("mutations", bad, &QueryContext::background()),
        Err(Error::Unsupported(_))
    ));
    for kind in [5, 20, 29, 99] {
        let mut bad = log[..8].to_vec();
        bad.extend(frame(&[100, 0, kind, 255, 255]));
        // A checkpoint marker may refer to already applied state even without
        // a flush. Other unsupported records in an uncommitted tail are skipped.
        if kind == 99 {
            assert!(matches!(
                recover("mutations", bad.clone(), &QueryContext::background()),
                Err(Error::Unsupported(_))
            ));
        } else {
            recover("mutations", bad.clone(), &QueryContext::background())?;
        }
        bad.extend(frame(&[100, 0, 100, 255, 255]));
        assert!(matches!(
            recover("mutations", bad, &QueryContext::background()),
            Err(Error::Unsupported(_))
        ));
    }
    let mut bad = log[..8].to_vec();
    bad.extend(frame(&[100, 0, 100, 1, 255, 255]));
    assert!(matches!(
        recover("mutations", bad, &QueryContext::background()),
        Err(Error::Corrupt(_))
    ));
    // The independent v1.3 writer predates tagged WAL headers. Construct the
    // newer source-defined header explicitly; do not claim oracle coverage.
    let checkpoint = fixture("version65", "duckdb");
    let log65 = fixture("version65", "wal");
    let iteration = [4096, 8192]
        .into_iter()
        .map(|offset| u64::from_le_bytes(checkpoint[offset + 8..offset + 16].try_into().unwrap()))
        .max()
        .unwrap();
    let mut identity = log65[..6].to_vec();
    identity.extend([102, 0, 16]);
    for byte in &checkpoint[124..140] {
        varint(u64::from(*byte), &mut identity);
    }
    identity.extend([103, 0]);
    let iteration_offset = identity.len();
    varint(iteration, &mut identity);
    identity.extend([255, 255]);
    identity.extend(&log65[8..]);
    recover("version65", identity.clone(), &QueryContext::background())?;
    let mut wrong_iteration = identity.clone();
    wrong_iteration[iteration_offset] ^= 1;
    assert!(matches!(
        recover("version65", wrong_iteration, &QueryContext::background()),
        Err(Error::Unsupported(_))
    ));
    let mut wrong_length = identity.clone();
    wrong_length[8] = 15;
    assert!(matches!(
        recover("version65", wrong_length, &QueryContext::background()),
        Err(Error::Corrupt(_))
    ));
    let mut noncanonical_checkpoint = log[..8].to_vec();
    noncanonical_checkpoint.extend(frame(&[100, 0, 227, 0, 255, 255]));
    assert!(matches!(
        recover(
            "mutations",
            noncanonical_checkpoint,
            &QueryContext::background()
        ),
        Err(Error::Unsupported(_))
    ));
    identity[9] ^= 1;
    assert!(matches!(
        recover("version65", identity, &QueryContext::background()),
        Err(Error::Corrupt(_))
    ));
    let interrupt = InterruptHandle::default();
    interrupt.interrupt();
    let context = QueryContext::new(interrupt, None, 64, 10_000)?;
    assert!(matches!(
        recover("mutations", log.clone(), &context),
        Err(Error::Interrupted)
    ));
    let context = QueryContext::new(InterruptHandle::default(), None, 2, 2)?;
    assert!(matches!(
        recover("mutations", log, &context),
        Err(Error::Resource(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn table() -> TableDefinition {
    let mut column = ColumnDefinition::new("i", DataType::Integer);
    column.nullable = false;
    TableDefinition {
        name: TableName::main("t"),
        columns: vec![column],
        unique_keys: vec![UniqueKey {
            columns: vec![0],
            primary: true,
        }],
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn recovery_target_defers_constraints_and_preserves_atomicity_with_both_indexes() -> Result<()> {
    let context = QueryContext::background();
    let name = TableName::main("t");
    for factory in [
        Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
        Arc::new(BTreeIndexFactory),
    ] {
        let mut snapshot = Snapshot::default().with_indexes(factory, &context)?;
        snapshot.create_table(table(), false)?;
        snapshot.insert(
            &name,
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]],
            &context,
        )?;
        // Append temporarily duplicates a key, then removes its old row. Index
        // constraints must inspect the committed result, not record order.
        snapshot.apply_committed(
            &[
                Change::Insert {
                    table: name.clone(),
                    rows: vec![vec![Value::Integer(1)]],
                },
                Change::Delete {
                    table: name.clone(),
                    ids: vec![0],
                },
            ],
            &context,
        )?;
        assert_eq!(
            snapshot.lookup(&name, &[0], &vec![Value::Integer(1)], &context)?[0].0,
            2
        );
        let before = snapshot.scan(&name, &context)?;
        let retained = snapshot.clone();
        for changes in [
            vec![Change::Insert {
                table: name.clone(),
                rows: vec![vec![Value::Integer(2)]],
            }],
            vec![
                Change::Delete {
                    table: name.clone(),
                    ids: vec![1],
                },
                Change::Update {
                    table: name.clone(),
                    column: 0,
                    values: vec![(99, Value::Integer(0))],
                },
            ],
            vec![
                Change::CreateSchema("never_visible".into()),
                Change::Update {
                    table: name.clone(),
                    column: 0,
                    values: vec![(2, Value::Null)],
                },
            ],
        ] {
            assert!(snapshot.apply_committed(&changes, &context).is_err());
            assert_eq!(snapshot.scan(&name, &context)?, before);
            assert_eq!(snapshot.schemas()?, vec!["main"]);
        }
        snapshot.apply_committed(
            &[Change::Update {
                table: name.clone(),
                column: 0,
                values: vec![(2, Value::Integer(3))],
            }],
            &context,
        )?;
        assert_eq!(retained.scan(&name, &context)?, before);
        assert!(
            snapshot
                .lookup(&name, &[0], &vec![Value::Integer(1)], &context)?
                .is_empty()
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn composition_and_sidecars_reject_unsupported_publication_states() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("test.duckdb");
    fs::write(&path, fixture("mutations", "duckdb"))?;
    let storage = Arc::new(LocalCheckpointStorage::open(
        &path,
        OpenMode::ReadOnly,
        || unreachable!(),
    )?);
    assert!(matches!(
        FileCheckpoint::new(storage.clone(), Arc::new(JsonSnapshotFormat))
            .with_recovery(Arc::new(DuckDbWalRecovery)),
        Err(Error::Unsupported(_))
    ));
    drop(storage);
    for suffix in [".wal.checkpoint", ".wal.recovery"] {
        let sidecar = directory.path().join(format!("test.duckdb{suffix}"));
        for bytes in [vec![], vec![1]] {
            fs::write(&sidecar, bytes)?;
            assert!(matches!(
                Database::open_read_only(&path),
                Err(Error::Unsupported(_))
            ));
            assert!(matches!(Database::open(&path), Err(Error::Unsupported(_))));
        }
        fs::remove_file(sidecar)?;
    }
    let orphan = directory.path().join("orphan.duckdb");
    fs::write(directory.path().join("orphan.duckdb.wal"), [1])?;
    assert!(Database::open(&orphan).is_err());
    assert!(!orphan.exists());
    // Recovery is explicitly selected; a format-only composition cannot
    // silently ignore the sidecar and expose stale checkpoint state.
    fs::write(
        directory.path().join("test.duckdb.wal"),
        fixture("mutations", "wal"),
    )?;
    let durability =
        FileCheckpoint::open(&path, OpenMode::ReadOnly, Arc::new(DuckDbFormat::default()))?;
    assert!(
        DatabaseBuilder::new()
            .durability(Arc::new(durability))
            .build()
            .is_err()
    );
    assert!(
        DuckDbWalRecovery
            .recover(
                RecoveryInput {
                    checkpoint: JsonSnapshotFormat.encode(&Snapshot::default())?,
                    log: Vec::new()
                },
                &JsonSnapshotFormat,
                &QueryContext::background()
            )
            .is_err()
    );
    let database = Database::open_read_only(&path)?;
    assert!(
        database
            .adapters()
            .iter()
            .any(|(role, name)| *role == "recovery" && *name == "duckdb-wal-v2")
    );
    assert!(!database.connect().query("SELECT * FROM t")?.rows.is_empty());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn varint(mut value: u64, bytes: &mut Vec<u8>) {
    while value >= 128 {
        bytes.push(value as u8 | 128);
        value >>= 7;
    }
    bytes.push(value as u8);
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn property(field: u16, value: &[u8], output: &mut Vec<u8>) {
    output.extend(field.to_le_bytes());
    output.extend(value);
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn blob(bytes: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    varint(bytes.len() as u64, &mut result);
    result.extend(bytes);
    result
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn string_vector(values: &[Option<&str>]) -> Vec<u8> {
    let mut result = Vec::new();
    property(100, &[1], &mut result);
    let mut mask = vec![0; values.len().div_ceil(64) * 8];
    for (i, value) in values.iter().enumerate() {
        if value.is_some() {
            mask[i / 8] |= 1 << (i % 8);
        }
    }
    property(101, &blob(&mask), &mut result);
    property(102, &[], &mut result);
    varint(values.len() as u64, &mut result);
    for value in values {
        result.extend(blob(value.map_or(&[128][..], str::as_bytes)));
    }
    result
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn vector_log(left: &[u8], right: &[u8], count: u8) -> Vec<u8> {
    let mut result = vec![100, 0, 98, 101, 0, 2, 255, 255];
    let mut table = vec![100, 0, 25];
    property(101, &blob(b"main"), &mut table);
    property(102, &blob(b"t"), &mut table);
    table.extend([255, 255]);
    result.extend(frame(&table));
    let mut insert = vec![
        100, 0, 26, 101, 0, 100, 0, count, 101, 0, 2, 100, 0, 13, 255, 255, 100, 0, 25, 255, 255,
        102, 0, 2,
    ];
    insert.extend(left);
    insert.extend([255, 255]);
    insert.extend(right);
    insert.extend([255, 255, 255, 255, 255, 255]);
    result.extend(frame(&insert));
    result.extend(frame(&[100, 0, 100, 255, 255]));
    result
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn serialized_vectors_preserve_selection_nulls_sequences_and_reject_bad_shapes() -> Result<()> {
    // Source-defined compressed serialization variants supplement the native
    // fixtures. The scalar oracle below is independent of decoder execution.
    let sequence = [90, 0, 4, 91, 0, 125, 92, 0, 2]; // start -3, increment 2 (signed LEB128)
    let mut dictionary = vec![90, 0, 3];
    let selection: Vec<u8> = [1u32, 0, 1, 2, 0, 2]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    property(91, &blob(&selection), &mut dictionary);
    property(92, &[3], &mut dictionary);
    dictionary.extend(string_vector(&[Some("a\0🦆"), None, Some("")]));
    let snapshot = recover(
        "version65",
        vector_log(&sequence, &dictionary, 6),
        &QueryContext::background(),
    )?;
    let rows = snapshot.scan(&TableName::main("t"), &QueryContext::background())?;
    for (i, string) in [None, Some("a\0🦆"), None, Some(""), Some("a\0🦆"), Some("")]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            rows[i + 1].1,
            vec![
                Value::Integer(-3 + 2 * i as i128),
                string.map_or(Value::Null, |s| Value::Varchar(s.into()))
            ]
        );
    }
    let mut constant = vec![90, 0, 2];
    constant.extend(string_vector(&[None]));
    let snapshot = recover(
        "version65",
        vector_log(&sequence, &constant, 6),
        &QueryContext::background(),
    )?;
    assert!(
        snapshot.scan(&TableName::main("t"), &QueryContext::background())?[1..]
            .iter()
            .all(|(_, row)| row[1].is_null())
    );
    let mut invalid = dictionary.clone();
    invalid[6..10].copy_from_slice(&3u32.to_le_bytes());
    assert!(matches!(
        recover(
            "version65",
            vector_log(&sequence, &invalid, 6),
            &QueryContext::background()
        ),
        Err(Error::Corrupt(_))
    ));
    let mut nested = [90, 0, 2].repeat(66);
    nested.extend(string_vector(&[None]));
    assert!(matches!(
        recover(
            "version65",
            vector_log(&sequence, &nested, 6),
            &QueryContext::background()
        ),
        Err(Error::Resource(_))
    ));
    let overflow = [90, 0, 4, 91, 0, 255, 255, 255, 255, 7, 92, 0, 1]; // i32::MAX + row index
    assert!(matches!(
        recover(
            "version65",
            vector_log(&overflow, &constant, 6),
            &QueryContext::background()
        ),
        Err(Error::Corrupt(_))
    ));
    Ok(())
}
