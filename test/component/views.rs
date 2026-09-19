use duckdb_rust::storage::{
    checkpoint::FileCheckpoint, filesystem::OpenMode, format::JsonSnapshotFormat,
};
use duckdb_rust::{Database, DatabaseBuilder, Error, Result, Value};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn ints(values: &[i128]) -> Vec<Vec<Value>> {
    values
        .iter()
        .map(|value| vec![Value::Integer(*value)])
        .collect()
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn persistent_views_bind_alias_stack_and_drop_dependencies() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    connection.execute(
        "CREATE TABLE base(i INTEGER); INSERT INTO base VALUES (1),(2),(3); \
         CREATE VIEW filtered(x) AS SELECT i FROM base WHERE i > 1; \
         CREATE VIEW stacked AS SELECT x + 10 AS y FROM filtered",
    )?;
    assert_eq!(
        connection.query("SELECT x FROM filtered ORDER BY x")?.rows,
        ints(&[2, 3])
    );
    assert_eq!(
        connection.query("SELECT y FROM stacked ORDER BY y")?.rows,
        ints(&[12, 13])
    );
    connection.execute("CREATE VIEW partial_alias(a) AS SELECT i, i + 1 AS b FROM base")?;
    assert_eq!(
        connection
            .query("SELECT a, b FROM partial_alias WHERE a = 2")?
            .rows,
        vec![vec![Value::Integer(2), Value::Integer(3)]]
    );
    connection.execute("DROP VIEW filtered")?;
    assert!(connection.query("SELECT * FROM filtered").is_err());
    assert!(connection.query("SELECT * FROM stacked").is_err());
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn view_create_replace_conflicts_recursion_and_failures_are_atomic() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    connection.execute(
        "CREATE TABLE t(i INTEGER); INSERT INTO t VALUES (7); CREATE VIEW v AS SELECT i FROM t",
    )?;
    assert!(connection.execute("CREATE VIEW t AS SELECT 1").is_err());
    assert!(connection.execute("CREATE TABLE v(i INTEGER)").is_err());
    assert!(
        connection
            .execute("CREATE VIEW self_ref AS SELECT * FROM self_ref")
            .is_err()
    );
    assert!(
        connection
            .execute("CREATE OR REPLACE VIEW v(a,b) AS SELECT i FROM t")
            .is_err()
    );
    assert_eq!(connection.query("SELECT * FROM v")?.rows, ints(&[7]));
    connection.execute("CREATE VIEW dependent AS SELECT * FROM v")?;
    assert!(
        connection
            .execute("CREATE OR REPLACE VIEW v AS SELECT * FROM dependent")
            .is_err()
    );
    assert_eq!(
        connection.query("SELECT * FROM dependent")?.rows,
        ints(&[7])
    );
    connection.execute("CREATE OR REPLACE VIEW v AS SELECT i + 1 AS i FROM t")?;
    assert_eq!(connection.query("SELECT * FROM v")?.rows, ints(&[8]));
    assert_eq!(
        connection.query("SELECT * FROM dependent")?.rows,
        ints(&[8])
    );
    assert!(connection.execute("DROP VIEW missing").is_err());
    assert!(connection.execute("DROP VIEW t").is_err());
    assert!(connection.execute("DROP VIEW IF EXISTS t").is_err());
    connection.execute("DROP VIEW IF EXISTS missing")?;
    connection.execute("CREATE VIEW duplicate_names AS SELECT i, i FROM t")?;
    let duplicate = connection.query("SELECT * FROM duplicate_names")?;
    assert_eq!(
        duplicate
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        vec!["i", "i_1"]
    );
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn view_ddl_is_transactional_and_prepared_queries_rebind() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    connection.execute(
        "CREATE TABLE t(i INTEGER); INSERT INTO t VALUES (4); CREATE VIEW v AS SELECT i FROM t",
    )?;
    let prepared = connection.prepare("SELECT * FROM v")?;
    assert_eq!(
        connection.execute_prepared(&prepared, &[])?.rows,
        ints(&[4])
    );
    connection.execute("BEGIN; CREATE OR REPLACE VIEW v AS SELECT i + 1 AS i FROM t")?;
    assert_eq!(connection.query("SELECT * FROM v")?.rows, ints(&[5]));
    connection.execute("ROLLBACK")?;
    assert_eq!(connection.query("SELECT * FROM v")?.rows, ints(&[4]));
    connection.execute("BEGIN; DROP VIEW v")?;
    assert!(connection.query("SELECT * FROM v").is_err());
    connection.execute("ROLLBACK")?;
    assert_eq!(connection.query("SELECT * FROM v")?.rows, ints(&[4]));
    connection.execute("BEGIN; CREATE OR REPLACE VIEW v AS SELECT i + 2 AS i FROM t; COMMIT")?;
    assert_eq!(
        connection.execute_prepared(&prepared, &[])?.rows,
        ints(&[6])
    );
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn view_uses_creation_schema_for_unqualified_relations() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    connection.execute(
        "CREATE SCHEMA s; \
         CREATE TABLE main.t(i INTEGER); INSERT INTO main.t VALUES (8); \
         CREATE TABLE s.t(i INTEGER); INSERT INTO s.t VALUES (9); \
         SET search_path='s'; \
         CREATE VIEW s.v AS SELECT i FROM t; \
         CREATE VIEW main.v AS SELECT i FROM t; \
         CREATE VIEW s.copy AS SELECT * FROM main.v; \
         SET search_path='main'",
    )?;
    assert_eq!(connection.query("SELECT * FROM s.v")?.rows, ints(&[9]));
    assert_eq!(connection.query("SELECT * FROM main.v")?.rows, ints(&[8]));
    assert_eq!(connection.query("SELECT * FROM s.copy")?.rows, ints(&[8]));
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn old_transaction_keeps_view_definition_and_private_checkpoint_reopens() -> Result<()> {
    let db = Database::memory()?;
    let mut old = db.connect();
    let mut writer = db.connect();
    writer.execute(
        "CREATE TABLE t(i INTEGER); INSERT INTO t VALUES (3); CREATE VIEW v AS SELECT i FROM t",
    )?;
    old.execute("BEGIN")?;
    assert_eq!(old.query("SELECT * FROM v")?.rows, ints(&[3]));
    writer.execute("CREATE OR REPLACE VIEW v AS SELECT i + 5 AS i FROM t")?;
    assert_eq!(old.query("SELECT * FROM v")?.rows, ints(&[3]));
    assert_eq!(writer.query("SELECT * FROM v")?.rows, ints(&[8]));
    old.execute("COMMIT")?;
    assert_eq!(old.query("SELECT * FROM v")?.rows, ints(&[8]));

    old.execute("BEGIN")?;
    assert_eq!(old.query("SELECT * FROM v")?.rows, ints(&[8]));
    writer.execute("DROP VIEW v")?;
    assert!(writer.query("SELECT * FROM v").is_err());
    assert_eq!(old.query("SELECT * FROM v")?.rows, ints(&[8]));
    old.execute("COMMIT")?;
    assert!(old.query("SELECT * FROM v").is_err());

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("views.snapshot");
    {
        let db = DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(JsonSnapshotFormat),
            )?))
            .build()?;
        db.connect().execute(
            "CREATE TABLE base(i INTEGER); INSERT INTO base VALUES (11); \
             CREATE VIEW first AS SELECT i FROM base; \
             CREATE VIEW second AS SELECT i + 1 AS i FROM first",
        )?;
    }
    let db = DatabaseBuilder::new()
        .durability(Arc::new(FileCheckpoint::open(
            &path,
            OpenMode::ReadWrite,
            Arc::new(JsonSnapshotFormat),
        )?))
        .build()?;
    let mut reopened = db.connect();
    assert_eq!(reopened.query("SELECT * FROM second")?.rows, ints(&[12]));
    reopened.execute("DROP TABLE base")?;
    assert!(reopened.query("SELECT * FROM second").is_err());
    reopened.execute("CREATE TABLE base(j VARCHAR); INSERT INTO base VALUES ('changed')")?;
    assert!(reopened.query("SELECT * FROM second").is_err());
    reopened.execute("DROP TABLE base")?;
    reopened.execute("CREATE TABLE base(i INTEGER); INSERT INTO base VALUES (20)")?;
    assert_eq!(reopened.query("SELECT * FROM second")?.rows, ints(&[21]));
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn view_output_schema_rebinds_after_base_relation_changes() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    connection.execute(
        "CREATE TABLE base(i INTEGER); INSERT INTO base VALUES (1); \
         CREATE VIEW dynamic AS SELECT * FROM base",
    )?;
    connection.execute(
        "DROP TABLE base; CREATE TABLE base(i VARCHAR); INSERT INTO base VALUES ('changed')",
    )?;
    let result = connection.query("SELECT * FROM dynamic")?;
    assert_eq!(result.columns[0].name, "i");
    assert_eq!(result.rows, vec![vec![Value::Varchar("changed".into())]]);
    connection.execute(
        "DROP TABLE base; CREATE TABLE base(k INTEGER, j INTEGER); INSERT INTO base VALUES (2,3)",
    )?;
    let result = connection.query("SELECT * FROM dynamic")?;
    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        vec!["k", "j"]
    );
    assert_eq!(
        result.rows,
        vec![vec![Value::Integer(2), Value::Integer(3)]]
    );
    Ok(())
}

struct CountingParser {
    calls: Arc<AtomicUsize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::parser::Parser for CountingParser {
    fn name(&self) -> &'static str {
        "counting-selected-parser"
    }

    fn parse(&self, sql: &str) -> Result<Vec<duckdb_rust::parser::Statement>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        duckdb_rust::parser::Parser::parse(&duckdb_rust::parser::DuckDbParser, sql)
    }
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn view_rebinding_uses_the_selected_parser_without_native_reparse() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let database = DatabaseBuilder::new()
        .parser(Arc::new(CountingParser {
            calls: calls.clone(),
        }))
        .build()?;
    let mut connection = database.connect();
    connection.execute(
        "CREATE TABLE base(i INTEGER); INSERT INTO base VALUES (1); \
         CREATE VIEW v AS SELECT i + 1 AS i FROM base",
    )?;
    let after_create = calls.load(Ordering::SeqCst);
    assert_eq!(connection.query("SELECT * FROM v")?.rows, ints(&[2]));
    assert_eq!(calls.load(Ordering::SeqCst), after_create + 2);
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn native_projection_filter_and_stacked_views_reopen_in_rust() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("views.duckdb");
    {
        let mut connection = Database::open(&path)?.connect();
        connection.execute(
            "CREATE TABLE base(i INTEGER); INSERT INTO base VALUES (30),(31); \
             CREATE VIEW v(x) AS SELECT i + 1 FROM base WHERE i > 30; \
             CREATE VIEW stacked AS SELECT x + 10 AS y FROM v; \
             CREATE VIEW wide AS SELECT i + 2147483648 AS n FROM base WHERE i = 30",
        )?;
    }
    let mut reopened = Database::open(&path)?.connect();
    assert_eq!(reopened.query("SELECT * FROM v")?.rows, ints(&[32]));
    assert_eq!(reopened.query("SELECT * FROM stacked")?.rows, ints(&[42]));
    assert_eq!(
        reopened.query("SELECT * FROM wide")?.rows,
        ints(&[2_147_483_678])
    );
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn unsupported_native_view_shape_fails_without_publication_or_catalog_change() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("unsupported-view.duckdb");
    let mut connection = Database::open(&path)?.connect();
    connection.execute("CREATE TABLE base(i INTEGER); INSERT INTO base VALUES (2),(1)")?;
    let checkpoint = std::fs::read(&path)?;
    assert!(
        connection
            .execute("CREATE VIEW ordered AS SELECT i FROM base ORDER BY i")
            .is_err()
    );
    assert!(connection.query("SELECT * FROM ordered").is_err());
    assert_eq!(std::fs::read(&path)?, checkpoint);
    drop(connection);
    assert!(
        Database::open(&path)?
            .connect()
            .query("SELECT * FROM ordered")
            .is_err()
    );
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn native_view_wal_create_replace_drop_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("views-wal.duckdb");
    {
        let mut connection = Database::open_logged(&path)?.connect();
        connection.execute(
            "CREATE TABLE base(i INTEGER); INSERT INTO base VALUES (5); \
             CREATE VIEW v AS SELECT i + 1 AS x FROM base WHERE i = 5",
        )?;
    }
    {
        let mut reopened = Database::open_logged(&path)?.connect();
        assert_eq!(reopened.query("SELECT * FROM v")?.rows, ints(&[6]));
        reopened.execute("CREATE OR REPLACE VIEW v AS SELECT i + 2 AS x FROM base; CHECKPOINT")?;
    }
    {
        let mut reopened = Database::open_logged(&path)?.connect();
        assert_eq!(reopened.query("SELECT * FROM v")?.rows, ints(&[7]));
        reopened.execute("DROP VIEW v")?;
    }
    assert!(
        Database::open_logged(&path)?
            .connect()
            .query("SELECT * FROM v")
            .is_err()
    );
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn torn_or_corrupt_native_view_wal_never_half_publishes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("source.duckdb");
    {
        let mut connection = Database::open_logged(&source)?.connect();
        connection.execute(
            "CREATE TABLE base(i INTEGER); INSERT INTO base VALUES (5); CHECKPOINT; \
             CREATE VIEW v AS SELECT i + 1 AS x FROM base",
        )?;
    }
    let checkpoint = std::fs::read(&source)?;
    let wal = std::fs::read(source.with_extension("duckdb.wal"))?;
    let frame_start = |end: usize| {
        (0..end.saturating_sub(16))
            .rev()
            .find(|start| {
                let Some(length) = wal
                    .get(*start..*start + 8)
                    .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
                    .map(u64::from_le_bytes)
                    .and_then(|length| usize::try_from(length).ok())
                else {
                    return false;
                };
                start
                    .checked_add(16)
                    .and_then(|start| start.checked_add(length))
                    == Some(end)
            })
            .expect("native WAL frame ending at selected offset")
    };
    let commit_start = frame_start(wal.len());
    let view_start = frame_start(commit_start);
    assert_eq!(
        wal.get(commit_start + 16..commit_start + 19),
        Some(&[100, 0, 100][..])
    );
    assert_eq!(
        wal.get(view_start + 16..view_start + 19),
        Some(&[100, 0, 5][..])
    );

    let view_payload_start = view_start + 16;
    let view_midpoint = view_payload_start + (commit_start - view_payload_start) / 2;
    let commit_payload_start = commit_start + 16;
    let commit_midpoint = commit_payload_start + (wal.len() - commit_payload_start) / 2;
    for (name, end) in [
        ("torn-view", view_midpoint),
        ("torn-commit", commit_midpoint),
    ] {
        let case = directory.path().join(format!("{name}.duckdb"));
        std::fs::write(&case, &checkpoint)?;
        std::fs::write(case.with_extension("duckdb.wal"), &wal[..end])?;
        let before_checkpoint = std::fs::read(&case)?;
        let before_wal = std::fs::read(case.with_extension("duckdb.wal"))?;
        let mut reopened = Database::open_read_only(&case)?.connect();
        assert_eq!(reopened.query("SELECT sum(i) FROM base")?.rows, ints(&[5]));
        assert!(reopened.query("SELECT * FROM v").is_err());
        drop(reopened);
        assert_eq!(std::fs::read(&case)?, before_checkpoint);
        assert_eq!(
            std::fs::read(case.with_extension("duckdb.wal"))?,
            before_wal
        );
    }

    let corrupt = directory.path().join("corrupt-view.duckdb");
    std::fs::write(&corrupt, &checkpoint)?;
    let mut corrupt_wal = wal.clone();
    corrupt_wal[view_midpoint] ^= 1;
    std::fs::write(corrupt.with_extension("duckdb.wal"), &corrupt_wal)?;
    assert!(matches!(
        Database::open_read_only(&corrupt),
        Err(Error::Corrupt(message)) if message.contains("checksum")
    ));
    assert_eq!(std::fs::read(&corrupt)?, checkpoint);
    assert_eq!(
        std::fs::read(corrupt.with_extension("duckdb.wal"))?,
        corrupt_wal
    );
    Ok(())
}
