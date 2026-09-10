use super::*;
use duckdb_rust::storage::{
    duckdb::{
        DuckDbFormat,
        wal::{DuckDbWalRecovery, writer::DuckDbTransactionLog},
    },
    logged::FileWal,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn open(path: &std::path::Path) -> Result<Database> {
    let checkpoint =
        FileCheckpoint::open(path, OpenMode::ReadWrite, Arc::new(DuckDbFormat::default()))?
            .with_recovery(Arc::new(DuckDbWalRecovery))?;
    DatabaseBuilder::new()
        .durability(Arc::new(FileWal::new(
            checkpoint,
            Arc::new(DuckDbTransactionLog),
        )?))
        .build()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn nested_wal_replay_preserves_typed_children_mutations_and_rollback() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("nested.duckdb");
    Database::open(&path)?.connect().execute("CREATE TABLE t(i INTEGER PRIMARY KEY,s STRUCT(n DECIMAL(12,2),z TIMESTAMP_NS,b BIT),l STRUCT(x INTEGER)[],a INTEGER[2],m MAP(VARCHAR,INTEGER[]),u UNION(n INTEGER,s VARCHAR))")?;
    let before;
    let after;
    {
        let mut c = open(&path)?.connect();
        c.execute("INSERT INTO t VALUES(0,{'n':1.25,'z':TIMESTAMP_NS '2000-01-01 00:00:00.123456789','b':'101'::BIT},[{'x':1},NULL],[1,NULL],map(['x','y'],[[1,NULL],[]]),union_value(n:=NULL)),(1,{'n':NULL,'z':NULL,'b':NULL},[],[NULL,2],map([],[]),union_value(s:='a')),(2,NULL,NULL,NULL,NULL,NULL)")?;
        before = c.query("SELECT * FROM t ORDER BY i")?.rows;
        let prepared = c.prepare("INSERT INTO t VALUES($1,$2,$3,$4,$5,$6)")?;
        let mut values = before[0].to_vec();
        values[0] = Value::Integer(3);
        c.execute_prepared(&prepared, &values)?;
        c.execute("BEGIN; UPDATE t SET l=[{'x':9}] WHERE i=0; DELETE FROM t WHERE i=1; ROLLBACK")?;
        c.execute("UPDATE t SET s={'n':2.50,'z':TIMESTAMP_NS '2001-01-01 00:00:00.000000001','b':'0'::BIT},u=union_value(s:='changed') WHERE i=0; DELETE FROM t WHERE i=2")?;
        after = c.query("SELECT * FROM t ORDER BY i")?.rows;
    }
    let mut c = open(&path)?.connect();
    assert_eq!(c.query("SELECT * FROM t ORDER BY i")?.rows, after);
    assert_eq!(
        c.query("SELECT l[1].x,a[2],u.n FROM t WHERE i=3")?.rows,
        vec![vec![Value::Integer(1), Value::Null, Value::Null]]
    );
    assert_eq!(
        c.query("SELECT count(*) FROM t a JOIN t b ON a.l=b.l")?
            .rows,
        vec![vec![Value::Integer(5)]]
    );
    c.checkpoint()?;
    drop(c);
    let mut c = Database::open_read_only(&path)?.connect();
    assert_eq!(c.query("SELECT * FROM t ORDER BY i")?.rows, after);
    assert_eq!(before[0][2], after[0][2]);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_nested_wal_vectors_preserve_child_validity_and_development_strings() -> Result<()> {
    independent_fixture("wal-nested", &["i", "s", "l", "a", "m", "u"])
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_nested_child_update_paths_preserve_parent_and_child_validity() -> Result<()> {
    independent_fixture("wal-nested-paths", &["i", "s"])
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn independent_fixture(suite: &str, fields: &[&str]) -> Result<()> {
    use std::{fs, io::Read};
    for target in ["release", "development"] {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("test/data/{suite}-{target}"));
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("manifest.json"))?).unwrap();
        for (name, case) in manifest["cases"].as_object().unwrap() {
            let read = |suffix: &str| -> Result<Vec<u8>> {
                let mut bytes = Vec::new();
                flate2::read::GzDecoder::new(fs::File::open(
                    root.join(format!("{name}.{suffix}.gz")),
                )?)
                .read_to_end(&mut bytes)?;
                Ok(bytes)
            };
            let checkpoint = read("duckdb")?;
            let log = read("wal")?;
            for state in case["states"].as_array().unwrap() {
                let directory = tempfile::tempdir()?;
                let path = directory.path().join("nested.duckdb");
                let wal_path = directory.path().join("nested.duckdb.wal");
                let end = state["end"].as_u64().unwrap() as usize;
                fs::write(&path, &checkpoint)?;
                fs::write(&wal_path, &log[..end])?;
                let durability = FileCheckpoint::open(
                    &path,
                    OpenMode::ReadOnly,
                    Arc::new(DuckDbFormat::default()),
                )?
                .with_recovery(Arc::new(DuckDbWalRecovery))?;
                let mut c = DatabaseBuilder::new()
                    .durability(Arc::new(durability))
                    .build()
                    .map_err(|error| {
                        duckdb_rust::Error::Internal(format!(
                            "{target} {name} boundary {end}: {error}"
                        ))
                    })?
                    .connect();
                let expected = state["rows"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| {
                        fields
                            .iter()
                            .map(|key| match &row[*key] {
                                serde_json::Value::Null => Value::Null,
                                serde_json::Value::String(value) => Value::Varchar(value.clone()),
                                value => Value::Integer(value.as_i64().unwrap() as i128),
                            })
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    c.query(case["query"].as_str().unwrap())?.rows,
                    expected,
                    "{target} {name} boundary {end}"
                );
                assert_eq!(fs::read(&path)?, checkpoint);
                assert_eq!(fs::read(&wal_path)?, log[..end]);
                if suite == "wal-nested-paths" && end > 0 {
                    drop(c);
                    let mut c = open(&path)?.connect();
                    assert_eq!(c.query(case["query"].as_str().unwrap())?.rows, expected);
                    c.execute("BEGIN; UPDATE t SET s=NULL WHERE i=1; ROLLBACK")?;
                    assert_eq!(c.query(case["query"].as_str().unwrap())?.rows, expected);
                    c.checkpoint()?;
                    drop(c);
                    assert_eq!(
                        Database::open_read_only(&path)?
                            .connect()
                            .query(case["query"].as_str().unwrap())?
                            .rows,
                        expected
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn physical_child_recovery_is_atomic_and_defers_union_and_null_parent_validation() -> Result<()> {
    use duckdb_rust::{
        catalog::{Catalog, CatalogMut, ColumnDefinition, TableDefinition, TableName, UniqueKey},
        storage::{
            TableStorage, TableStorageMut,
            recovery::{RecoveredChange as Change, RecoveryTarget},
            table::Snapshot,
        },
    };
    let name = TableName::main("t");
    let structure = NestedType::Struct(vec![
        ("a".into(), DataType::Integer),
        (
            "b".into(),
            NestedType::Struct(vec![("s".into(), DataType::Varchar)]).data_type(),
        ),
    ])
    .data_type();
    let union = NestedType::Union(vec![
        ("i".into(), DataType::Integer),
        ("s".into(), DataType::Varchar),
    ])
    .data_type();
    let query = QueryContext::background();
    let mut original = Snapshot::default();
    let mut id_column = ColumnDefinition::new("id", DataType::Integer);
    id_column.nullable = false;
    original.create_table(
        TableDefinition {
            name: name.clone(),
            columns: vec![
                id_column,
                ColumnDefinition::new("v", structure.clone()),
                ColumnDefinition::new("u", union.clone()),
            ],
            unique_keys: vec![UniqueKey {
                columns: vec![0],
                primary: true,
            }],
        },
        false,
    )?;
    original.insert(
        &name,
        vec![vec![
            Value::Integer(1),
            Value::Null,
            NestedValue::value(
                union.clone(),
                NestedPayload::Union {
                    tag: 0,
                    value: Value::Integer(9),
                },
            )?,
        ]],
        &query,
    )?;
    let update = |column, path, value| Change::NestedUpdate {
        table: name.clone(),
        column,
        path,
        values: vec![(0, value)],
    };
    let valid = |column, path, value| Change::NestedValidity {
        table: name.clone(),
        column,
        path,
        values: vec![(0, value)],
    };
    let changes = vec![
        update(1, vec![0], Value::Integer(7)),
        valid(1, vec![0], true),
        update(1, vec![1, 0], Value::Varchar("x".into())),
        valid(1, vec![1, 0], true),
        valid(1, vec![1], true),
        valid(1, vec![], true),
        update(2, vec![0], Value::Unsigned(1)),
        valid(2, vec![1], false),
        update(2, vec![2], Value::Varchar("new".into())),
        valid(2, vec![2], true),
    ];
    let mut expected = None;
    for reverse in [false, true] {
        let mut snapshot = original.clone();
        let mut changes = changes.clone();
        if reverse {
            changes.reverse();
        }
        snapshot.apply_committed(&changes, &query)?;
        let rows = snapshot.scan(&name, &query)?;
        assert_eq!(rows[0].1[1].to_string(), "{'a': 7, 'b': {'s': x}}");
        assert_eq!(
            rows[0].1[2],
            NestedValue::value(
                union.clone(),
                NestedPayload::Union {
                    tag: 1,
                    value: Value::Varchar("new".into())
                }
            )?
        );
        if let Some(expected) = &expected {
            assert_eq!(&rows, expected);
        } else {
            expected = Some(rows);
        }
    }
    let before = original.scan(&name, &query)?;
    for changes in [
        vec![valid(1, vec![0], true), valid(1, vec![], true)],
        vec![update(2, vec![0], Value::Unsigned(1))],
        vec![update(1, vec![7], Value::Integer(1))],
        vec![update(1, vec![0, 0], Value::Integer(1))],
        vec![update(1, vec![0], Value::Varchar("bad".into()))],
    ] {
        let mut snapshot = original.clone();
        let mut all = vec![Change::CreateSchema("not_published".into())];
        all.extend(changes);
        assert!(snapshot.apply_committed(&all, &query).is_err());
        assert_eq!(snapshot.scan(&name, &query)?, before);
        assert_eq!(snapshot.schemas()?, vec!["main"]);
    }
    let mut snapshot = original.clone();
    snapshot.apply_committed(
        &[
            update(1, vec![0], Value::Integer(99)),
            valid(1, vec![0], true),
        ],
        &query,
    )?;
    assert_eq!(
        snapshot.scan(&name, &query)?,
        before,
        "hidden children must not make a NULL parent valid"
    );
    assert_eq!(
        original.scan(&name, &query)?,
        before,
        "retained snapshot must remain unchanged"
    );
    Ok(())
}
