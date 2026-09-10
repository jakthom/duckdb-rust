use std::{fs, io::Read, path::Path};

use duckdb_rust::{Database, Error, Result, Value};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn fixture(name: &str, path: &Path) -> Result<()> {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test/data/duckdb")
        .join(format!("{name}.duckdb.gz"));
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(source)?).read_to_end(&mut bytes)?;
    fs::write(path, bytes)?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_duckdb_files_are_read_without_native_dependencies() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for (name, sql, expected) in [
        (
            "scalar",
            "SELECT count(*), sum(id), count(name), count(value), count(active) FROM t",
            vec![
                Value::Integer(3),
                Value::Integer(6),
                Value::Integer(2),
                Value::Integer(2),
                Value::Integer(2),
            ],
        ),
        (
            "bitpacking",
            "SELECT count(*), sum(id), sum(k) FROM t",
            vec![
                Value::Integer(10000),
                Value::Integer(49995000),
                Value::Integer(29994),
            ],
        ),
        (
            "rle",
            "SELECT count(*), sum(id) FROM t",
            vec![Value::Integer(10000), Value::Integer(495000)],
        ),
        (
            "overflow",
            "SELECT count(*), sum(length(text)) FROM t",
            vec![Value::Integer(4), Value::Integer(308000)],
        ),
    ] {
        let path = directory.path().join(format!("{name}.duckdb"));
        fixture(name, &path)?;
        let original = fs::read(&path)?;
        let mut c = Database::open_read_only(&path)?.connect();
        assert_eq!(c.query(sql)?.rows, vec![expected], "fixture {name}");
        assert!(matches!(
            c.execute("DELETE FROM t"),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(
            fs::read(&path)?,
            original,
            "reference file must remain unchanged"
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn exact_scalar_values_and_types_survive_checkpoint_read() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("scalar.duckdb");
    fixture("scalar", &path)?;
    let mut c = Database::open_read_only(&path)?.connect();
    let result = c.query("SELECT * FROM t ORDER BY id")?;
    assert_eq!(
        result
            .columns
            .iter()
            .map(|f| f.data_type.clone())
            .collect::<Vec<_>>(),
        vec![
            duckdb_rust::DataType::Integer,
            duckdb_rust::DataType::Varchar,
            duckdb_rust::DataType::Double,
            duckdb_rust::DataType::Boolean
        ]
    );
    assert_eq!(
        result.rows,
        vec![
            vec![
                Value::Integer(1),
                Value::Varchar("one".into()),
                Value::Double(1.5),
                Value::Boolean(true)
            ],
            vec![
                Value::Integer(2),
                Value::Null,
                Value::Double(2.5),
                Value::Boolean(false)
            ],
            vec![
                Value::Integer(3),
                Value::Varchar("three".into()),
                Value::Null,
                Value::Null
            ],
        ]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn alp_floating_values_survive_groups_exceptions_and_publication() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("alp.duckdb");
    fixture("alp", &path)?;
    for publish in [true, false] {
        let mut connection = Database::open(&path)?.connect();
        let rows = connection.query("SELECT * FROM t")?.rows;
        assert_eq!(rows.len(), 125013);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row[0], Value::Integer(i as i128));
            if i % 29 == 0 {
                assert_eq!(row[1], Value::Null);
            } else {
                let value = row[1].as_f64()?;
                let expected = match i % 1001 {
                    1 => f64::NAN,
                    2 => f64::INFINITY,
                    3 => f64::NEG_INFINITY,
                    4 => f64::MAX,
                    5 => f64::from_bits(1),
                    _ => (i as f64 - 65000.0) / 100.0,
                };
                if expected.is_nan() {
                    assert!(value.is_nan());
                } else {
                    assert_eq!(value.to_bits(), expected.to_bits(), "row {i}");
                }
            }
            assert_eq!(
                row[2].as_f64()?.to_bits(),
                (1000000000000000.0 + (i % 13) as f64).to_bits()
            );
            assert_eq!(
                row[3].as_f64()?.to_bits(),
                ((i % 1000) as f64 / 1e18).to_bits()
            );
        }
        if publish {
            connection
                .execute("CREATE TABLE published(i INTEGER); INSERT INTO published VALUES (42)")?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn truncated_corrupt_locked_and_unrecovered_files_are_rejected() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("bad.duckdb");
    fixture("scalar", &path)?;
    let original = fs::read(&path)?;
    for length in [0, 8, 12, 4096, 8192, 12288, original.len() - 1] {
        fs::write(&path, &original[..length])?;
        assert!(Database::open_read_only(&path).is_err(), "length {length}");
    }
    fs::write(&path, &original)?;
    let mut corrupt = original.clone();
    corrupt[12310] ^= 1;
    fs::write(&path, &corrupt)?;
    assert!(matches!(
        Database::open_read_only(&path),
        Err(Error::Corrupt(_))
    ));
    fs::write(&path, &original)?;
    let database = Database::open(&path)?;
    assert!(Database::open_read_only(&path).is_err());
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_duckdb-rust"))
        .arg(&path)
        .args(["--read-only", "-c", "SELECT 1"])
        .output()?;
    assert!(
        !child.status.success(),
        "a second process must honor the writer lock"
    );
    drop(database);
    fs::write(directory.path().join("bad.duckdb.wal"), [1, 2, 3])?;
    assert!(matches!(
        Database::open_read_only(&path),
        Err(Error::Unsupported(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn compressed_strings_match_every_logical_value() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for name in ["dictionary", "fsst"] {
        let path = directory.path().join(format!("{name}.duckdb"));
        fixture(name, &path)?;
        let rows = Database::open_read_only(&path)?
            .connect()
            .query("SELECT text FROM t")?
            .rows;
        assert_eq!(rows.len(), 10000);
        for (i, row) in rows.iter().enumerate() {
            let value = if i % 11 == 0 {
                Value::Null
            } else {
                Value::Varchar(if name == "dictionary" {
                    format!("category-{}", i % 7)
                } else {
                    format!("{}{i}", "duckdb-".repeat(10))
                })
            };
            assert_eq!(*row, vec![value], "{name} row {i}");
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn rust_updates_existing_duckdb_checkpoints_and_reopens() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for name in [
        "scalar",
        "bitpacking",
        "rle",
        "dictionary",
        "fsst",
        "overflow",
    ] {
        let path = directory.path().join(format!("{name}.duckdb"));
        fixture(name, &path)?;
        let expected;
        {
            let mut c = Database::open(&path)?.connect();
            expected = c.query("SELECT * FROM t")?.rows;
            c.execute("CREATE TABLE written(i INTEGER); INSERT INTO written VALUES (42)")?;
        }
        let mut c = Database::open_read_only(&path)?.connect();
        assert_eq!(
            c.query("SELECT * FROM t")?.rows,
            expected,
            "reencoded {name}"
        );
        assert_eq!(
            c.query("SELECT * FROM written")?.rows,
            vec![vec![Value::Integer(42)]]
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn unsupported_publication_preserves_the_original_database() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("database.duckdb");
    let mut c = Database::open(&path)?.connect();
    c.execute("CREATE TABLE t(i INTEGER); INSERT INTO t VALUES (1)")?;
    let before = fs::read(&path)?;
    assert!(matches!(
        // SQL CTAS now correctly normalizes NULL to INTEGER. Submit an explicit
        // unresolved storage type through the plan API to keep this a native
        // publication failure, rather than requiring that fixed SQL gap.
        c.execute_plan(BoundStatement::CreateTable {
            definition: TableDefinition {
                name: TableName::main("constrained"),
                columns: vec![ColumnDefinition::new("i", DataType::Null)],
                unique_keys: vec![],
            },
            if_not_exists: false,
            source: None,
        }),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(fs::read(&path)?, before);
    assert!(c.query("SELECT * FROM constrained").is_err());
    use duckdb_rust::{
        DataType,
        catalog::{ColumnDefinition, TableDefinition, TableName},
        planner::BoundStatement,
    };
    let large_default = TableDefinition {
        name: TableName::main("oversized"),
        columns: vec![ColumnDefinition {
            default: Value::Varchar("x".repeat(16_777_217)),
            ..ColumnDefinition::new("v", DataType::Varchar)
        }],
        unique_keys: vec![],
    };
    assert!(matches!(
        c.execute_plan(BoundStatement::CreateTable {
            definition: large_default,
            if_not_exists: false,
            source: None
        }),
        Err(Error::Resource(_))
    ));
    assert_eq!(fs::read(&path)?, before);
    assert!(c.query("SELECT * FROM oversized").is_err());
    assert_eq!(
        c.query("SELECT * FROM t")?.rows,
        vec![vec![Value::Integer(1)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_rowgroups_and_empty_schemas_are_preserved() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("rowgroups.duckdb");
    fixture("rowgroups", &path)?;
    {
        let mut c = Database::open(&path)?.connect();
        assert_eq!(
            c.query("SELECT count(*), sum(id) FROM t")?.rows,
            vec![vec![Value::Integer(125000), Value::Integer(7812437500)]]
        );
        c.execute("INSERT INTO t VALUES (125000)")?;
    }
    assert_eq!(
        Database::open_read_only(&path)?
            .connect()
            .query("SELECT count(*), sum(id) FROM t")?
            .rows,
        vec![vec![Value::Integer(125001), Value::Integer(7812562500)]]
    );
    let path = directory.path().join("schemas.duckdb");
    fixture("schemas", &path)?;
    {
        let mut c = Database::open(&path)?.connect();
        assert_eq!(
            c.query("SELECT * FROM nested.extra")?.rows,
            vec![vec![Value::Integer(99)]]
        );
        c.execute("INSERT INTO t VALUES (2)")?;
    }
    let mut c = Database::open(&path)?.connect();
    c.execute("CREATE TABLE empty.created(i INTEGER)")?;
    assert!(c.execute("DROP SCHEMA empty").is_err());
    c.execute("DROP TABLE empty.created; DROP SCHEMA empty")?;
    assert!(c.execute("CREATE TABLE empty.invalid(i INTEGER)").is_err());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn a_damaged_unused_header_does_not_hide_a_valid_checkpoint() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("header.duckdb");
    {
        Database::open(&path)?
            .connect()
            .execute("CREATE TABLE t(i INTEGER); INSERT INTO t VALUES (42)")?;
    }
    let mut bytes = fs::read(&path)?;
    bytes[8192 + 30] ^= 1;
    fs::write(&path, &bytes)?;
    assert_eq!(
        Database::open_read_only(&path)?
            .connect()
            .query("SELECT * FROM t")?
            .rows,
        vec![vec![Value::Integer(42)]]
    );
    bytes[4096 + 30] ^= 1;
    fs::write(&path, bytes)?;
    assert!(matches!(
        Database::open_read_only(&path),
        Err(Error::Corrupt(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn checkpoint_deletions_preserve_committed_visibility() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("deletions.duckdb");
    fixture("deletions", &path)?;
    let expected: Vec<_> = (0..130000)
        .filter(|&i| {
            !(i == 1
                || (2048..4096).contains(&i)
                || ((4096..6144).contains(&i) && i % 2 == 0)
                || ((6144..8192).contains(&i) && i != 7000)
                || i == 125000)
        })
        .map(|i| vec![Value::Integer(i)])
        .collect();
    {
        let mut c = Database::open(&path)?.connect();
        assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, expected);
        c.execute("CREATE TABLE changed(i INTEGER)")?;
    }
    assert_eq!(
        Database::open_read_only(&path)?
            .connect()
            .query("SELECT * FROM t ORDER BY id")?
            .rows,
        expected
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn persisted_unique_and_primary_keys_survive_mutation_and_restart() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("indexes.duckdb");
    fixture("indexes", &path)?;
    {
        let mut c = Database::open(&path)?.connect();
        c.execute("UPDATE t SET u='changed' WHERE id=10; DELETE FROM t WHERE id=4; INSERT INTO t VALUES (7000,'new',7000,'new')")?;
        assert!(
            c.execute("INSERT INTO t VALUES (10,'unused',7001,'unused')")
                .is_err()
        );
        assert!(
            c.execute("INSERT INTO t VALUES (7001,'changed',7001,'unused')")
                .is_err()
        );
        assert!(
            c.execute("INSERT INTO t VALUES (7001,'unused',7000,'new')")
                .is_err()
        );
        assert!(
            c.execute("INSERT INTO t VALUES (NULL,'unused',7001,'unused')")
                .is_err()
        );
        c.execute("BEGIN; DELETE FROM t WHERE id=10; ROLLBACK")?;
    }
    let mut c = Database::open(&path)?.connect();
    assert_eq!(
        c.query("SELECT count(*) FROM t")?.rows,
        vec![vec![Value::Integer(6000)]]
    );
    assert!(
        c.execute("INSERT INTO t VALUES (10,'unused',7001,'unused')")
            .is_err()
    );
    c.execute("CREATE TABLE empty_pk(i INTEGER PRIMARY KEY); CREATE TABLE compound(a INTEGER, b VARCHAR, PRIMARY KEY(a,b)); INSERT INTO compound VALUES (1,'a'),(1,'b'),(2,'a')")?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn existing_literal_defaults_are_evaluated_after_publication_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("defaults.duckdb");
    fixture("defaults", &path)?;
    let expected = vec![
        Value::Integer(-128),
        Value::Integer(32767),
        Value::Integer(i32::MIN.into()),
        Value::Integer(i64::MAX.into()),
        Value::Integer(i128::MIN),
        Value::Double(0.5),
        Value::Varchar("quack'🦆".into()),
        Value::Boolean(true),
        Value::Null,
    ];
    {
        let mut c = Database::open(&path)?.connect();
        assert_eq!(c.query("SELECT * FROM t")?.rows, vec![expected.clone()]);
        c.execute("INSERT INTO t DEFAULT VALUES; CREATE TABLE created(i HUGEINT DEFAULT '-170141183460469231731687303715884105728'::HUGEINT, f DOUBLE DEFAULT 'Infinity'::DOUBLE, v VARCHAR DEFAULT 'duck'); INSERT INTO created DEFAULT VALUES")?;
    }
    let mut c = Database::open(&path)?.connect();
    c.execute("CREATE TABLE try_defaults(i INTEGER DEFAULT try_cast('bad' AS INTEGER), n INTEGER DEFAULT -NULL); INSERT INTO try_defaults DEFAULT VALUES")?;
    assert_eq!(
        c.query("SELECT * FROM try_defaults")?.rows,
        vec![vec![Value::Null, Value::Null]]
    );
    assert!(
        c.execute("CREATE TABLE bad_default(v VARCHAR DEFAULT +'bad')")
            .is_err()
    );
    c.execute("INSERT INTO t DEFAULT VALUES; INSERT INTO created DEFAULT VALUES")?;
    assert_eq!(c.query("SELECT * FROM t")?.rows, vec![expected; 3]);
    assert_eq!(
        c.query("SELECT * FROM created")?.rows,
        vec![
            vec![
                Value::Integer(i128::MIN),
                Value::Double(f64::INFINITY),
                Value::Varchar("duck".into())
            ];
            2
        ]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn alp_and_alprd_preserve_both_floating_widths_and_ieee_extrema() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for name in ["alp_float", "alprd"] {
        let path = directory.path().join(format!("{name}.db"));
        fixture(name, &path)?;
        for publish in [true, false] {
            let mut connection = Database::open(&path)?.connect();
            let result = connection.query("SELECT * FROM t")?;
            assert_eq!(result.columns[2].data_type, duckdb_rust::DataType::Float);
            assert_eq!(result.rows.len(), 125013);
            for (i, row) in result.rows.iter().enumerate() {
                assert_eq!(row[0], Value::Integer(i as i128));
                if i % 29 == 0 {
                    assert_eq!(&row[1..], [Value::Null, Value::Null]);
                    continue;
                }
                let (d, f) = match i % 1001 {
                    1 => (f64::NAN, f32::NAN),
                    2 => (f64::INFINITY, f32::INFINITY),
                    3 => (f64::NEG_INFINITY, f32::NEG_INFINITY),
                    4 => (f64::MAX, f32::MAX),
                    5 => (f64::from_bits(1), f32::from_bits(1)),
                    6 => (-0.0, -0.0),
                    _ => ((i as f64 - 65000.0) / 17.0, (i as f32 - 65000.0) / 17.0),
                };
                if d.is_nan() {
                    assert!(row[1].as_f64()?.is_nan());
                    assert!(row[2].as_f32()?.is_nan());
                } else {
                    assert_eq!(row[1].as_f64()?.to_bits(), d.to_bits(), "{name} double {i}");
                    assert_eq!(row[2].as_f32()?.to_bits(), f.to_bits(), "{name} float {i}");
                }
            }
            if publish {
                connection
                    .execute("CREATE TABLE written(i INTEGER); INSERT INTO written VALUES (42)")?;
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn historical_chimp_and_patas_files_keep_both_tables_and_values() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for name in ["chimp", "patas"] {
        let path = directory.path().join(format!("{name}.db"));
        fixture(name, &path)?;
        let mut connection = Database::open_read_only(&path)?.connect();
        let double = connection.query("SELECT temperature FROM temperatures_double")?;
        let single = connection.query("SELECT temperature FROM temperatures_float")?;
        assert_eq!(double.rows.len(), 245000);
        assert_eq!(single.rows.len(), 245000);
        assert_eq!(double.columns[0].data_type, duckdb_rust::DataType::Double);
        assert_eq!(single.columns[0].data_type, duckdb_rust::DataType::Float);
        for (d, f) in double.rows.iter().zip(&single.rows) {
            assert_eq!((d[0].as_f64()? as f32).to_bits(), f[0].as_f32()?.to_bits());
        }
        assert_eq!(
            connection
                .query("SELECT sum(temperature) FROM temperatures_float")?
                .rows,
            vec![vec![Value::Double(13112811.599449158)]]
        );
    }
    Ok(())
}
