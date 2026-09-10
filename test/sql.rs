#[path = "runner/mod.rs"]
mod runner;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sql_logic_corpus() -> duckdb_rust::Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test/sql");
    let mut paths = std::fs::read_dir(root)?
        .map(|p| p.map(|p| p.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    assert!(!paths.is_empty());
    for path in paths {
        if path.extension().is_some_and(|s| s == "test") {
            runner::run_file(&duckdb_rust::Database::memory()?, &path)?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn run_source(database: &duckdb_rust::Database, source: &str) -> duckdb_rust::Result<usize> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("case.test");
    std::fs::write(&path, source)?;
    runner::run_file(database, &path)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sql_harness_keeps_named_transactions_and_default_session_independent() -> duckdb_rust::Result<()>
{
    let database = duckdb_rust::Database::memory()?;
    assert_eq!(
        run_source(
            &database,
            "statement ok\nCREATE TABLE t(i INTEGER); INSERT INTO t VALUES (2),(1)\n\n\
             statement ok writer\nBEGIN; INSERT INTO t VALUES (3)\n\n\
             query I writer\nSELECT count(*) FROM t\n----\n3\n\n\
             query I reader\nSELECT count(*) FROM t\n----\n2\n\n\
             query I rowsort\nSELECT i FROM t\n----\n1\n2\n\n\
             statement ok writer\nCOMMIT\n\n\
             query I reader\nSELECT count(*) FROM t\n----\n3\n\n\
             query I\nSELECT count(*) FROM t\n----\n3\n",
        )?,
        8
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sql_harness_rejects_wrong_results_errors_and_unavailable_capabilities() -> duckdb_rust::Result<()>
{
    let database = duckdb_rust::Database::memory()?;
    for source in [
        "query I named\nSELECT 1\n----\n2\n",
        "query II named\nSELECT 1\n----\n1\n",
        "query I named\nSELECT 1\n----\n",
        "statement error named\nSELECT 1\n",
        "statement error named\nSELECT missing\n----\nnot the actual error\n",
        "query I nosort unimplemented_label\nSELECT 1\n----\n1\n",
    ] {
        assert!(run_source(&database, source).is_err(), "{source}");
    }

    struct UnavailableParser;
    impl duckdb_rust::parser::Parser for UnavailableParser {
        fn name(&self) -> &'static str {
            "unavailable-parser"
        }
        fn parse(&self, _: &str) -> duckdb_rust::Result<Vec<duckdb_rust::parser::Statement>> {
            Err(duckdb_rust::Error::Unsupported("parser unavailable".into()))
        }
    }
    let database = duckdb_rust::DatabaseBuilder::new()
        .parser(std::sync::Arc::new(UnavailableParser))
        .build()?;
    assert!(run_source(&database, "statement error named\nSELECT 1\n").is_err());
    Ok(())
}
