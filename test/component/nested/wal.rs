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
    use std::{fs, io::Read};
    for target in ["release", "development"] {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("test/data/wal-nested-{target}"));
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
                        ["i", "s", "l", "a", "m", "u"]
                            .map(|key| match &row[key] {
                                serde_json::Value::Null => Value::Null,
                                serde_json::Value::String(value) => Value::Varchar(value.clone()),
                                value => Value::Integer(value.as_i64().unwrap() as i128),
                            })
                            .to_vec()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    c.query(case["query"].as_str().unwrap())?.rows,
                    expected,
                    "{target} {name} boundary {end}"
                );
                assert_eq!(fs::read(&path)?, checkpoint);
                assert_eq!(fs::read(&wal_path)?, log[..end]);
            }
        }
    }
    Ok(())
}
