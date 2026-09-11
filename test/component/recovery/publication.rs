use super::*;
use duckdb_rust::storage::{
    filesystem::{CheckpointStorage, FileFaultInjector, PublicationStep},
    recovery::RecoveryPublication,
};

const REPLACE_STEPS: &[PublicationStep] = &[
    PublicationStep::CheckpointCreate,
    PublicationStep::CheckpointWrite,
    PublicationStep::CheckpointSync,
    PublicationStep::RecoveryLogCreate,
    PublicationStep::RecoveryLogWrite,
    PublicationStep::RecoveryLogSync,
    PublicationStep::RecoveryLogRename,
    PublicationStep::RecoveryLogDirectorySync,
    PublicationStep::CheckpointRename,
    PublicationStep::CheckpointDirectorySync,
    PublicationStep::LogRemove,
    PublicationStep::LogRetirementDirectorySync,
];
const RETIRE_STEPS: &[PublicationStep] = &[
    PublicationStep::CurrentCheckpointSync,
    PublicationStep::CheckpointDirectorySync,
    PublicationStep::LogRemove,
    PublicationStep::LogRetirementDirectorySync,
];

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn input(name: &str) -> RecoveryInput {
    RecoveryInput {
        checkpoint: fixture(name, "duckdb"),
        log: fixture(name, "wal"),
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn root(bytes: &[u8]) -> (u64, u64) {
    [4096, 8192]
        .into_iter()
        .map(|offset| {
            (
                u64::from_le_bytes(bytes[offset + 8..offset + 16].try_into().unwrap()),
                u64::from_le_bytes(bytes[offset + 16..offset + 24].try_into().unwrap()),
            )
        })
        .max_by_key(|(generation, _)| *generation)
        .unwrap()
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn populate(path: &std::path::Path, input: &RecoveryInput) -> Result<()> {
    fs::write(path, &input.checkpoint)?;
    fs::write(path.with_extension("duckdb.wal"), &input.log)?;
    Ok(())
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn verify(path: &std::path::Path, name: &str) -> Result<()> {
    let manifest = manifest();
    let case = &manifest["cases"][name];
    for _ in 0..2 {
        let database = Database::open_read_only(path)?;
        assert_eq!(
            json_rows(database.connect().query(case["query"].as_str().unwrap())?),
            *case["states"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .get("rows")
                .unwrap()
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn bridge_logs_recover_both_checkpoint_generations_and_retire_completed_work() -> Result<()> {
    for name in ["mutations", "create", "batches", "version65"] {
        let original = input(name);
        let prepared = DuckDbWalRecovery.prepare(
            input(name),
            &DuckDbFormat::default(),
            &QueryContext::background(),
        )?;
        let RecoveryPublication::Replace {
            checkpoint,
            bridge_log,
        } = prepared.publication
        else {
            panic!("expected checkpoint publication");
        };
        assert_ne!(root(&original.checkpoint).1, root(&checkpoint).1);
        assert_eq!(root(&original.checkpoint).0 + 1, root(&checkpoint).0);
        for bytes in [&original.checkpoint, &checkpoint] {
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("case.duckdb");
            populate(
                &path,
                &RecoveryInput {
                    checkpoint: bytes.clone(),
                    log: bridge_log.clone(),
                },
            )?;
            verify(&path, name)?;
            let plan = DuckDbWalRecovery.prepare(
                RecoveryInput {
                    checkpoint: bytes.clone(),
                    log: bridge_log.clone(),
                },
                &DuckDbFormat::default(),
                &QueryContext::background(),
            )?;
            assert_eq!(
                matches!(plan.publication, RecoveryPublication::RetireLog),
                bytes == &checkpoint
            );
            drop(Database::open(&path)?);
            assert!(!path.with_extension("duckdb.wal").exists());
            verify(&path, name)?;
            let database = Database::open(&path)?;
            database.connect().execute("CREATE TABLE after_recovery(i INTEGER PRIMARY KEY); INSERT INTO after_recovery VALUES(42)")?;
            drop(database);
            assert_eq!(
                Database::open_read_only(&path)?
                    .connect()
                    .query("SELECT * FROM after_recovery")?
                    .rows,
                vec![vec![Value::Integer(42)]]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_checkpoint_markers_select_replay_or_retirement() -> Result<()> {
    let manifest: Json =
        serde_json::from_slice(&fs::read(super::root().join("checkpoints/manifest.json"))?)
            .unwrap();
    for (name, case) in manifest["cases"].as_object().unwrap() {
        let original = input(&format!("checkpoints/{name}"));
        // Find the independently serialized checkpoint entry. A complete
        // marker still identifies the root if its following flush is torn.
        let mut offset = 8;
        let marker_end = loop {
            let size =
                u64::from_le_bytes(original.log[offset..offset + 8].try_into().unwrap()) as usize;
            let kind = original.log[offset + 18];
            offset += 16 + size;
            if kind == 99 {
                break offset;
            }
        };
        for end in marker_end..=original.log.len() {
            let prepared = DuckDbWalRecovery.prepare(
                RecoveryInput {
                    checkpoint: original.checkpoint.clone(),
                    log: original.log[..end].to_vec(),
                },
                &DuckDbFormat::default(),
                &QueryContext::background(),
            )?;
            assert_eq!(
                matches!(prepared.publication, RecoveryPublication::RetireLog),
                case["checkpoint_published"].as_bool().unwrap()
            );
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("native.duckdb");
            populate(&path, &prepared.basis)?;
            for read_only in [true, false, true] {
                let database = if read_only {
                    Database::open_read_only(&path)?
                } else {
                    Database::open(&path)?
                };
                assert_eq!(
                    json_rows(database.connect().query(case["query"].as_str().unwrap())?),
                    case["rows"]
                );
            }
            assert!(!path.with_extension("duckdb.wal").exists());
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn tagged_bridge_identity_accepts_only_the_published_successor_generation() -> Result<()> {
    let mut original = input("version65");
    let generation = root(&original.checkpoint).0;
    let tagged = |generation, frames: &[u8]| {
        let mut log = vec![100, 0, 98, 101, 0, 2, 102, 0, 16];
        for &byte in &original.checkpoint[124..140] {
            varint(u64::from(byte), &mut log);
        }
        log.extend([103, 0]);
        varint(generation, &mut log);
        log.extend([255, 255]);
        log.extend(frames);
        log
    };
    original.log = tagged(generation, &original.log[8..]);
    let header_end = original.log.len() - (fixture("version65", "wal").len() - 8);
    let format = DuckDbFormat::default();
    let context = QueryContext::background();
    let prepared = DuckDbWalRecovery.prepare(
        RecoveryInput {
            checkpoint: original.checkpoint.clone(),
            log: original.log.clone(),
        },
        &format,
        &context,
    )?;
    let RecoveryPublication::Replace {
        checkpoint,
        bridge_log,
    } = prepared.publication
    else {
        panic!("expected successor");
    };
    assert_eq!(&checkpoint[124..140], &original.checkpoint[124..140]);
    for delta in [0, 1, 2] {
        let result = DuckDbWalRecovery.prepare(
            RecoveryInput {
                checkpoint: checkpoint.clone(),
                log: tagged(generation + delta, &bridge_log[header_end..]),
            },
            &format,
            &context,
        );
        if delta < 2 {
            assert!(matches!(
                result?.publication,
                RecoveryPublication::RetireLog
            ));
        } else {
            assert!(matches!(result, Err(Error::Unsupported(_))));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn successor_root_collision_is_resolved_without_losing_metadata_chains() -> Result<()> {
    for columns in [2, 180] {
        let mut snapshot = Snapshot::default();
        let definition = TableDefinition {
            name: TableName::main("t"),
            columns: (0..columns)
                .map(|i| {
                    ColumnDefinition::new(
                        format!("column_{i}_{}", "x".repeat(1600)),
                        DataType::Integer,
                    )
                })
                .collect(),
            unique_keys: Vec::new(),
        };
        snapshot.create_table(definition.clone(), false)?;
        snapshot.insert(
            &definition.name,
            vec![(0..columns).map(|i| Value::Integer(i as i128)).collect()],
            &QueryContext::background(),
        )?;
        let format = DuckDbFormat::default();
        let previous = format.encode(&snapshot)?;
        // Normal encoding is deterministic and would reuse the same root.
        assert_eq!(root(&format.encode(&snapshot)?), root(&previous));
        let successor = format.encode_successor(&snapshot, &previous)?;
        assert_ne!(root(&successor.bytes).1, root(&previous).1);
        assert_eq!(root(&successor.bytes).0, root(&previous).0 + 1);
        let decoded = format.decode(
            successor.bytes,
            duckdb_rust::common::type_registry::builtin_types(),
        )?;
        assert_eq!(decoded.table(&definition.name)?, definition);
        assert_eq!(
            decoded.scan(&definition.name, &QueryContext::background())?,
            snapshot.scan(&definition.name, &QueryContext::background())?
        );
    }
    Ok(())
}

struct FailAt(PublicationStep);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FileFaultInjector for FailAt {
    fn before(&self, step: PublicationStep) -> Result<()> {
        if step == self.0 {
            Err(std::io::Error::other(format!("injected {step:?}")).into())
        } else {
            Ok(())
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn io_failures_at_each_publication_boundary_preserve_commits_and_allow_retry() -> Result<()> {
    let format = DuckDbFormat::default();
    for retire in [false, true] {
        let steps = if retire { RETIRE_STEPS } else { REPLACE_STEPS };
        for &step in steps {
            let mut basis = input("mutations");
            if retire {
                let prepared = DuckDbWalRecovery.prepare(
                    input("mutations"),
                    &format,
                    &QueryContext::background(),
                )?;
                let RecoveryPublication::Replace {
                    checkpoint,
                    bridge_log,
                } = prepared.publication
                else {
                    unreachable!()
                };
                basis = RecoveryInput {
                    checkpoint,
                    log: bridge_log,
                };
            }
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("case.duckdb");
            populate(&path, &basis)?;
            let prepared =
                DuckDbWalRecovery.prepare(basis, &format, &QueryContext::background())?;
            let storage =
                LocalCheckpointStorage::open(&path, OpenMode::ReadWrite, || unreachable!())?
                    .with_faults(Arc::new(FailAt(step)));
            let error = storage
                .publish_recovery(&prepared.basis, &prepared.publication)
                .unwrap_err();
            let uncertain = step == PublicationStep::LogRetirementDirectorySync
                || !retire
                    && matches!(
                        step,
                        PublicationStep::CheckpointDirectorySync | PublicationStep::LogRemove
                    );
            assert_eq!(
                matches!(error, Error::CommitUnknown(_)),
                uncertain,
                "{step:?}: {error}"
            );
            drop(storage);
            // Definite errors may change the bridge log but cannot lose any
            // acknowledged state. Ordinary errors remove their staged files.
            assert!(
                fs::read_dir(directory.path())?.all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains("-"))
            );
            verify(&path, "mutations")?;
            drop(Database::open(&path)?);
            assert!(!path.with_extension("duckdb.wal").exists());
            verify(&path, "mutations")?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn stale_recovery_plans_and_direct_checkpoint_writes_cannot_discard_a_log() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("case.duckdb");
    let basis = input("mutations");
    populate(&path, &basis)?;
    let prepared =
        DuckDbWalRecovery.prepare(basis, &DuckDbFormat::default(), &QueryContext::background())?;
    let storage = LocalCheckpointStorage::open(&path, OpenMode::ReadWrite, || unreachable!())?;
    assert!(matches!(
        storage.replace(&prepared.basis.checkpoint),
        Err(Error::Unsupported(_))
    ));
    storage.publish_recovery(&prepared.basis, &prepared.publication)?;
    let published = storage.read()?;
    assert!(matches!(
        storage.publish_recovery(&prepared.basis, &prepared.publication),
        Err(Error::Transaction(_))
    ));
    assert_eq!(storage.read()?, published);
    assert!(storage.read_log()?.is_empty());
    drop(storage);
    verify(&path, "mutations")?;
    Ok(())
}

struct ExitAt(PublicationStep);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FileFaultInjector for ExitAt {
    fn before(&self, step: PublicationStep) -> Result<()> {
        if step == self.0 {
            std::process::exit(86);
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Child entry point used by Cargo tests and the independent verification
/// script. Exit deliberately skips Rust destructors, leaving real files at the
/// selected I/O boundary. The database library contains no environment hooks.
#[test]
fn publication_child() -> Result<()> {
    let Ok(path) = std::env::var("DDB_RECOVERY_CHILD_PATH") else {
        return Ok(());
    };
    let ordinal: usize = std::env::var("DDB_RECOVERY_CHILD_STEP")
        .unwrap()
        .parse()
        .unwrap();
    let retire = std::env::var("DDB_RECOVERY_CHILD_RETIRE").is_ok();
    let step = if retire {
        RETIRE_STEPS[ordinal]
    } else {
        REPLACE_STEPS[ordinal]
    };
    let storage = LocalCheckpointStorage::open(
        std::path::Path::new(&path),
        OpenMode::ReadWrite,
        || unreachable!(),
    )?
    .with_faults(Arc::new(ExitAt(step)));
    let durability = FileCheckpoint::new(Arc::new(storage), Arc::new(DuckDbFormat::default()))
        .with_recovery(Arc::new(DuckDbWalRecovery))?;
    let _database = DatabaseBuilder::new()
        .durability(Arc::new(durability))
        .build()?;
    panic!("requested publication boundary was not reached");
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn process_interruption_at_every_publication_boundary_remains_recoverable() -> Result<()> {
    for retire in [false, true] {
        let steps = if retire { RETIRE_STEPS } else { REPLACE_STEPS };
        for (ordinal, step) in steps.iter().enumerate() {
            let mut basis = input("mutations");
            if retire {
                let prepared = DuckDbWalRecovery.prepare(
                    input("mutations"),
                    &DuckDbFormat::default(),
                    &QueryContext::background(),
                )?;
                let RecoveryPublication::Replace {
                    checkpoint,
                    bridge_log,
                } = prepared.publication
                else {
                    unreachable!()
                };
                basis = RecoveryInput {
                    checkpoint,
                    log: bridge_log,
                };
            }
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("case.duckdb");
            populate(&path, &basis)?;
            let mut child = std::process::Command::new(std::env::current_exe()?);
            child
                .args(["--exact", "publication::publication_child", "--nocapture"])
                .env("DDB_RECOVERY_CHILD_PATH", &path)
                .env("DDB_RECOVERY_CHILD_STEP", ordinal.to_string());
            if retire {
                child.env("DDB_RECOVERY_CHILD_RETIRE", "1");
            }
            let result = child.output()?;
            assert_eq!(
                result.status.code(),
                Some(86),
                "{step:?}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            verify(&path, "mutations")?;
            drop(Database::open(&path)?);
            assert!(!path.with_extension("duckdb.wal").exists());
            verify(&path, "mutations")?;
        }
    }
    Ok(())
}
