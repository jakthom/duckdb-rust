use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use duckdb_rust::{
    DataType, DatabaseBuilder, Error, Result, Value,
    catalog::{CatalogMut, ColumnDefinition, TableDefinition, TableName, UniqueKey},
    common::Row,
    execution::index::{BTreeIndexFactory, HashIndexFactory, IndexFactory, IndexSpec, KeyIndex},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer, RemoveTrueFilters, UseKeyLookup},
    parallel::{InterruptHandle, QueryContext},
    storage::{
        RowId, TableStorage, TableStorageMut,
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
        table::Snapshot,
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn factories() -> Vec<Arc<dyn IndexFactory>> {
    vec![Arc::new(HashIndexFactory), Arc::new(BTreeIndexFactory)]
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn ints(values: &[i128]) -> Row {
    values.iter().copied().map(Value::Integer).collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn index_adapters_share_key_identity_ownership_and_resource_contracts() -> Result<()> {
    let context = QueryContext::background();
    for factory in factories() {
        let spec = IndexSpec {
            key_types: vec![DataType::Double, DataType::Varchar],
            unique: false,
        };
        let key = |v| vec![Value::Double(v), Value::Varchar("🦆\0\u{1}".into())];
        let index = factory.build(
            spec.clone(),
            &mut vec![
                (9, key(-0.0)),
                (2, key(0.0)),
                (7, key(f64::NAN)),
                (1, key(f64::from_bits(0x7ff0000000000001))),
                (3, vec![Value::Null, Value::Null]),
            ]
            .into_iter(),
            &context,
        )?;
        assert_eq!(index.lookup(&key(0.0), &context)?, vec![2, 9]);
        assert_eq!(index.lookup(&key(f64::NAN), &context)?, vec![1, 7]);
        assert!(index.lookup(&key(1.0), &context)?.is_empty());
        assert!(
            index
                .lookup(&vec![Value::Null, Value::Null], &context)?
                .is_empty()
        );
        assert!(index.lookup(&ints(&[0, 1]), &context).is_err());
        assert!(index.lookup(&vec![], &context).is_err());
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let index = index.clone();
                std::thread::spawn(move || {
                    index
                        .lookup(&key(-0.0), &QueryContext::background())
                        .unwrap()
                })
            })
            .collect();
        for reader in readers {
            assert_eq!(reader.join().unwrap(), vec![2, 9]);
        }
        let unique = IndexSpec {
            unique: true,
            ..spec.clone()
        };
        for values in [
            vec![(0, key(-0.0)), (1, key(0.0))],
            vec![(0, key(f64::NAN)), (1, key(f64::NAN))],
        ] {
            assert!(matches!(
                factory.build(unique.clone(), &mut values.into_iter(), &context),
                Err(Error::Constraint(_))
            ));
        }
        factory.build(
            unique,
            &mut vec![
                (0, vec![Value::Null, Value::Null]),
                (1, vec![Value::Null, Value::Null]),
            ]
            .into_iter(),
            &context,
        )?;
        assert!(
            factory
                .build(
                    spec.clone(),
                    &mut vec![(0, key(1.0)), (0, key(2.0))].into_iter(),
                    &context
                )
                .is_err()
        );
        let interrupt = InterruptHandle::default();
        let cancelled = QueryContext::new(interrupt.clone(), None, 1, 100)?;
        interrupt.interrupt();
        assert!(matches!(
            index.lookup(&key(0.0), &cancelled),
            Err(Error::Interrupted)
        ));
        assert!(matches!(
            factory.build(spec.clone(), &mut std::iter::empty(), &cancelled),
            Err(Error::Interrupted)
        ));
        let limited = QueryContext::new(InterruptHandle::default(), None, 1, 1)?;
        assert!(matches!(
            index.lookup(&key(0.0), &limited),
            Err(Error::Resource(_))
        ));
        assert!(matches!(
            factory.build(
                spec,
                &mut vec![(0, key(1.0)), (1, key(2.0))].into_iter(),
                &limited
            ),
            Err(Error::Resource(_))
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn snapshot_indexes_publish_atomically_with_rows() -> Result<()> {
    let context = QueryContext::background();
    for factory in factories() {
        let name = TableName::main("t");
        let mut snapshot = Snapshot::default().with_indexes(factory, &context)?;
        snapshot.create_table(
            TableDefinition {
                name: name.clone(),
                columns: vec![
                    ColumnDefinition::new("a", DataType::Integer),
                    ColumnDefinition::new("b", DataType::Integer),
                ],
                unique_keys: vec![UniqueKey {
                    columns: vec![1, 0],
                    primary: false,
                }],
            },
            false,
        )?;
        assert!(snapshot.capabilities().key_lookup);
        assert_eq!(snapshot.key_columns(&name)?, vec![vec![1, 0]]);
        snapshot.insert(&name, vec![ints(&[1, 10]), ints(&[2, 20])], &context)?;
        let before = snapshot.clone();
        assert!(
            snapshot
                .insert(&name, vec![ints(&[3, 30]), ints(&[1, 10])], &context)
                .is_err()
        );
        assert!(
            snapshot
                .lookup(&name, &[1, 0], &ints(&[30, 3]), &context)?
                .is_empty()
        );
        assert!(matches!(
            snapshot.lookup(&name, &[0], &ints(&[1]), &context),
            Err(Error::Unsupported(_))
        ));
        assert!(
            snapshot
                .update(&name, vec![(0, ints(&[2, 20]))], &context)
                .is_err()
        );
        assert_eq!(
            snapshot.lookup(&name, &[1, 0], &ints(&[10, 1]), &context)?,
            vec![(0, ints(&[1, 10]))]
        );
        snapshot.update(&name, vec![(0, ints(&[3, 30]))], &context)?;
        snapshot.delete(&name, &[1, 1], &context)?;
        snapshot.insert(&name, vec![ints(&[2, 20])], &context)?;
        assert_eq!(
            snapshot.lookup(&name, &[1, 0], &ints(&[20, 2]), &context)?,
            vec![(2, ints(&[2, 20]))]
        );
        assert_eq!(
            before.lookup(&name, &[1, 0], &ints(&[10, 1]), &context)?,
            vec![(0, ints(&[1, 10]))]
        );
        assert!(
            snapshot
                .lookup(&name, &[1, 0], &ints(&[10, 1]), &context)?
                .is_empty()
        );
    }
    Ok(())
}

struct ObservedFactory {
    inner: Arc<dyn IndexFactory>,
    lookups: Arc<AtomicUsize>,
}
#[derive(Debug)]
struct ObservedIndex {
    inner: Arc<dyn KeyIndex>,
    lookups: Arc<AtomicUsize>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl IndexFactory for ObservedFactory {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn build(
        &self,
        spec: IndexSpec,
        entries: &mut dyn Iterator<Item = (RowId, Row)>,
        context: &QueryContext,
    ) -> Result<Arc<dyn KeyIndex>> {
        Ok(Arc::new(ObservedIndex {
            inner: self.inner.build(spec, entries, context)?,
            lookups: self.lookups.clone(),
        }))
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl KeyIndex for ObservedIndex {
    fn lookup(&self, key: &Row, context: &QueryContext) -> Result<Vec<RowId>> {
        self.lookups.fetch_add(1, Ordering::Relaxed);
        self.inner.lookup(key, context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn planner_selects_indexes_through_contracts_across_restart_and_rollback() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let optimizers: Vec<Arc<dyn Optimizer>> = vec![
        Arc::new(IdentityOptimizer),
        Arc::new(PipelineOptimizer::default()),
        Arc::new(PipelineOptimizer::new(vec![Arc::new(UseKeyLookup)])),
        Arc::new(PipelineOptimizer::new(vec![
            Arc::new(UseKeyLookup),
            Arc::new(RemoveTrueFilters),
        ])),
    ];
    let formats: Vec<Arc<dyn SnapshotFormat>> = vec![
        Arc::new(DuckDbFormat::default()),
        Arc::new(JsonSnapshotFormat),
    ];
    for factory in factories() {
        for format in &formats {
            for (number, optimizer) in optimizers.iter().enumerate() {
                let path = directory.path().join(format!(
                    "{}-{}-{number}.db",
                    factory.name(),
                    format.name()
                ));
                let lookups = Arc::new(AtomicUsize::new(0));
                let open = || {
                    DatabaseBuilder::new()
                        .durability(Arc::new(FileCheckpoint::open(
                            &path,
                            OpenMode::ReadWrite,
                            format.clone(),
                        )?))
                        .indexes(Arc::new(ObservedFactory {
                            inner: factory.clone(),
                            lookups: lookups.clone(),
                        }))
                        .optimizer(optimizer.clone())
                        .build()
                };
                {
                    let db = open()?;
                    assert!(db.adapters().contains(&("indexes", factory.name())));
                    let mut c = db.connect();
                    c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER, b VARCHAR, UNIQUE(b,a)); INSERT INTO t VALUES (1,10,'x'),(2,10,'y'),(3,NULL,NULL),(4,NULL,NULL)")?;
                    assert_eq!(
                        c.query("SELECT id FROM t WHERE b='y' AND a=10")?.rows,
                        vec![ints(&[2])]
                    );
                    assert_eq!(
                        c.query("SELECT b FROM t WHERE id=1")?.rows,
                        vec![vec![Value::Varchar("x".into())]]
                    );
                    assert_eq!(
                        lookups.load(Ordering::Relaxed),
                        if number == 0 { 0 } else { 2 }
                    );
                    let plan =
                        c.query("EXPLAIN SELECT id FROM t WHERE id=1")?.rows[0][0].to_string();
                    assert_eq!(plan.contains("KeyLookup"), number != 0);
                    // Typed parameters remain execution constants for key
                    // lookup, without becoming SQL literal binding hints.
                    let parameter = c.prepare("SELECT id FROM t WHERE id=?")?;
                    let before = lookups.load(Ordering::Relaxed);
                    assert_eq!(
                        c.execute_prepared(&parameter, &[Value::Integer(1)])?.rows,
                        vec![ints(&[1])]
                    );
                    assert_eq!(
                        lookups.load(Ordering::Relaxed) - before,
                        usize::from(number != 0)
                    );
                    let before = lookups.load(Ordering::Relaxed);
                    assert_eq!(
                        c.query("SELECT id FROM t WHERE id=CASE WHEN true THEN 1 ELSE 2 END")?
                            .rows,
                        vec![ints(&[1])]
                    );
                    assert_eq!(
                        lookups.load(Ordering::Relaxed) - before,
                        usize::from(number != 0)
                    );
                    assert!(
                        c.query("SELECT id FROM t WHERE id=1 AND id=2")?
                            .rows
                            .is_empty()
                    );
                    assert!(c.query("SELECT id FROM t WHERE id=NULL")?.rows.is_empty());
                    c.execute("BEGIN; UPDATE t SET id=5 WHERE id=1")?;
                    assert_eq!(
                        c.query("SELECT id FROM t WHERE id=5")?.rows,
                        vec![ints(&[5])]
                    );
                    assert!(c.query("SELECT id FROM t WHERE id=1")?.rows.is_empty());
                    c.execute("ROLLBACK")?;
                    assert_eq!(
                        c.query("SELECT id FROM t WHERE id=1")?.rows,
                        vec![ints(&[1])]
                    );
                    assert!(c.execute("INSERT INTO t VALUES (5,10,'y')").is_err());
                    c.execute("DELETE FROM t WHERE id=2; INSERT INTO t VALUES (6,10,'y')")?;
                    let scans = lookups.load(Ordering::Relaxed);
                    // This cast errors before the impossible id comparison in
                    // the declared evaluator. An index must not hide it.
                    assert!(matches!(
                        c.query("SELECT id FROM t WHERE CAST(b AS INTEGER)=1 AND id=999"),
                        Err(Error::Conversion(_))
                    ));
                    assert_eq!(lookups.load(Ordering::Relaxed), scans);
                }
                {
                    let db = open()?;
                    let mut c = db.connect();
                    assert_eq!(
                        c.query("SELECT id FROM t WHERE b='y' AND a=10")?.rows,
                        vec![ints(&[6])]
                    );
                    assert!(c.query("SELECT id FROM t WHERE id=2")?.rows.is_empty());
                    assert!(c.execute("INSERT INTO t VALUES (6,20,'z')").is_err());
                    c.execute("BEGIN; UPDATE t SET id=9 WHERE id=6")?;
                    let mut reader = db.connect();
                    assert_eq!(
                        reader.query("SELECT id FROM t WHERE id=6")?.rows,
                        vec![ints(&[6])]
                    );
                    c.execute("COMMIT")?;
                    assert!(reader.query("SELECT id FROM t WHERE id=6")?.rows.is_empty());
                    assert_eq!(
                        reader.query("SELECT id FROM t WHERE id=9")?.rows,
                        vec![ints(&[9])]
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn a_point_lookup_does_not_materialize_the_table() -> Result<()> {
    for factory in factories() {
        let manager = Arc::new(
            duckdb_rust::transaction::SnapshotTransactions::with_indexes(
                Arc::new(duckdb_rust::storage::checkpoint::MemoryDurability),
                factory,
            )?,
        );
        let writer = DatabaseBuilder::new()
            .transactions(manager.clone())
            .build()?;
        writer.connect().execute(
            "CREATE TABLE t(id BIGINT PRIMARY KEY); INSERT INTO t SELECT range FROM range(5000)",
        )?;
        let db = DatabaseBuilder::new()
            .transactions(manager)
            .max_intermediate_rows(1)
            .build()?;
        assert_eq!(
            db.connect().query("SELECT id FROM t WHERE id=4999")?.rows,
            vec![ints(&[4999])]
        );
        assert!(matches!(
            db.connect().query("SELECT id FROM t"),
            Err(Error::Resource(_))
        ));
    }
    Ok(())
}

struct FailingBuild {
    inner: Arc<dyn IndexFactory>,
    fail: Arc<AtomicBool>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl IndexFactory for FailingBuild {
    fn name(&self) -> &'static str {
        "injected-build-failure"
    }
    fn build(
        &self,
        spec: IndexSpec,
        entries: &mut dyn Iterator<Item = (RowId, Row)>,
        context: &QueryContext,
    ) -> Result<Arc<dyn KeyIndex>> {
        let index = self.inner.build(spec, entries, context)?;
        if self.fail.load(Ordering::Relaxed) {
            Err(Error::Resource("injected index allocation failure".into()))
        } else {
            Ok(index)
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn failed_index_replacement_keeps_the_previous_rows_and_keys() -> Result<()> {
    let context = QueryContext::background();
    for inner in factories() {
        let fail = Arc::new(AtomicBool::new(false));
        let mut snapshot = Snapshot::default().with_indexes(
            Arc::new(FailingBuild {
                inner,
                fail: fail.clone(),
            }),
            &context,
        )?;
        let name = TableName::main("t");
        snapshot.create_table(
            TableDefinition {
                name: name.clone(),
                columns: vec![ColumnDefinition::new("k", DataType::Integer)],
                unique_keys: vec![UniqueKey {
                    columns: vec![0],
                    primary: false,
                }],
            },
            false,
        )?;
        snapshot.insert(&name, vec![ints(&[1]), ints(&[2])], &context)?;
        fail.store(true, Ordering::Relaxed);
        assert!(matches!(
            snapshot.insert(&name, vec![ints(&[3])], &context),
            Err(Error::Resource(_))
        ));
        assert!(matches!(
            snapshot.update(&name, vec![(0, ints(&[3]))], &context),
            Err(Error::Resource(_))
        ));
        assert!(matches!(
            snapshot.delete(&name, &[0], &context),
            Err(Error::Resource(_))
        ));
        assert_eq!(
            snapshot.scan(&name, &context)?,
            vec![(0, ints(&[1])), (1, ints(&[2]))]
        );
        assert_eq!(
            snapshot.lookup(&name, &[0], &ints(&[1]), &context)?,
            vec![(0, ints(&[1]))]
        );
        assert!(
            snapshot
                .lookup(&name, &[0], &ints(&[3]), &context)?
                .is_empty()
        );
        fail.store(false, Ordering::Relaxed);
        snapshot.insert(&name, vec![ints(&[3])], &context)?;
        assert_eq!(
            snapshot.lookup(&name, &[0], &ints(&[3]), &context)?,
            vec![(2, ints(&[3]))]
        );
    }
    Ok(())
}
