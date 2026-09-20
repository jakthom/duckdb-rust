use duckdb_rust::{
    DatabaseBuilder, Error, Result, Value,
    parallel::{InterruptHandle, MemoryPool, QueryContext},
};
use std::sync::Arc;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn memory_limit_is_global_and_aliases_share_one_publication() -> Result<()> {
    let database = DatabaseBuilder::new()
        .memory_limit_base(Arc::new(
            duckdb_rust::main::settings::host_memory::FixedMemoryLimitBase {
                bytes: 10_001,
                fallback: false,
                label: "test",
            },
        ))
        .build()?;
    let mut first = database.connect();
    let mut second = database.connect();
    first.execute("SET memory_limit='2 MB'")?;
    assert_eq!(
        second.query("SELECT current_setting('max_memory')")?.rows,
        vec![vec![Value::Varchar("1.9 MiB".into())]],
    );
    second.execute("SET max_memory='1 KiB'")?;
    assert_eq!(
        first.query("SELECT current_setting('memory_limit')")?.rows,
        vec![vec![Value::Varchar("1.0 KiB".into())]],
    );
    first.execute("RESET memory_limit")?;
    assert_eq!(
        second.query("SELECT current_setting('max_memory')")?.rows,
        vec![vec![Value::Varchar("7.8 KiB".into())]],
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn percentage_and_reset_use_the_injected_total_physical_basis() -> Result<()> {
    use duckdb_rust::main::settings::host_memory::FixedMemoryLimitBase;
    let base = Arc::new(FixedMemoryLimitBase {
        bytes: 10_001,
        fallback: false,
        label: "test-total-physical",
    });
    let database = DatabaseBuilder::new().memory_limit_base(base).build()?;
    let mut connection = database.connect();
    connection.execute("SET memory_limit='0.9%'")?;
    // Pinned development truncates the percentage before multiplication.
    assert_eq!(
        connection
            .query("SELECT current_setting('memory_limit')")?
            .rows,
        vec![vec![Value::Varchar("0 bytes".into())]]
    );
    connection.execute("SET memory_limit='100%'")?;
    connection.execute("RESET memory_limit")?;
    assert_eq!(
        connection
            .query("SELECT current_setting('memory_limit')")?
            .rows,
        vec![vec![Value::Varchar("7.8 KiB".into())]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn linux_memory_provider_precedence_is_pure_and_deterministic() {
    use duckdb_rust::main::settings::host_memory::resolve_linux_base;
    let read = |path: &str| match path {
        "/proc/self/cgroup" => Some("0::/slice/job\n".into()),
        "/sys/fs/cgroup/slice/job/memory.max" => Some("4096\n".into()),
        _ => None,
    };
    assert_eq!(resolve_linux_base(10_000, 4, |_| None, read), 4096);
    assert_eq!(
        resolve_linux_base(
            10_000,
            4,
            |key| (key == "SLURM_MEM_PER_NODE").then(|| "2".into()),
            |_| None
        ),
        2 * 1000 * 1000
    );
    assert_eq!(
        resolve_linux_base(
            10_000,
            4,
            |key| (key == "SLURM_MEM_PER_CPU").then(|| "3".into()),
            |_| None
        ),
        12 * 1000 * 1000
    );
    let v1 = |path: &str| match path {
        "/proc/self/cgroup" => Some("7:memory:/parent/child\n".into()),
        "/sys/fs/cgroup/memory/parent/child/memory.limit_in_bytes" => Some("8192".into()),
        _ => None,
    };
    assert_eq!(resolve_linux_base(10_000, 1, |_| None, v1), 8192);
    // `max` and malformed values are unlimited/invalid, therefore physical.
    assert_eq!(
        resolve_linux_base(
            10_000,
            1,
            |_| None,
            |path| match path {
                "/proc/self/cgroup" => Some("0::/x".into()),
                "/sys/fs/cgroup/x/memory.max" => Some("max".into()),
                _ => None,
            }
        ),
        10_000
    );
    assert_eq!(
        resolve_linux_base(
            10_000,
            1,
            |_| None,
            |path| match path {
                "/proc/self/cgroup" => Some("0::/x".into()),
                "/sys/fs/cgroup/x/memory.max" => Some("bad".into()),
                _ => None,
            }
        ),
        10_000
    );
    assert_eq!(
        resolve_linux_base(
            10_000,
            1,
            |_| None,
            |path| match path {
                "/proc/self/cgroup" => Some("7:cpu,memory,io:/multi".into()),
                "/sys/fs/cgroup/memory/multi/memory.limit_in_bytes" => Some("2048".into()),
                _ => None,
            }
        ),
        2048
    );
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn reservation_releases_only_after_final_clone_and_honors_cancellation() -> Result<()> {
    let pool = Arc::new(MemoryPool::default());
    pool.publish_limit(Some(8))?;
    let interrupt = InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 1, 1)?.with_memory_pool(pool.clone());
    let reservation = pool.reserve(8, &query)?;
    let retained = reservation.clone();
    assert!(matches!(pool.reserve(1, &query), Err(Error::Resource(_))));
    drop(reservation);
    assert_eq!(pool.used()?, 8);
    drop(retained);
    assert_eq!(pool.used()?, 0);
    interrupt.interrupt();
    assert!(matches!(pool.reserve(1, &query), Err(Error::Interrupted)));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sorted_batch_and_independent_column_keep_quota_after_connection_drop() -> Result<()> {
    use duckdb_rust::execution::StreamControl;
    let database = DatabaseBuilder::new().build()?;
    let mut connection = database.connect();
    connection.execute("CREATE TABLE quota_strings(v VARCHAR)")?;
    connection.execute("INSERT INTO quota_strings VALUES ('z'), ('a'), ('m')")?;
    connection.execute("SET memory_limit='1 MB'")?;
    let mut retained = None;
    connection.query_batches("SELECT v FROM quota_strings ORDER BY v", |_, batch| {
        retained = Some(batch.project(&[0])?.select(&[2, 0])?.slice(0, 1)?);
        Ok(StreamControl::Stop)
    })?;
    drop(connection);
    let retained = retained.expect("sort delivered a batch");
    let column = retained.columns()[0].clone();
    assert_eq!(column.value(0), Some(Value::Varchar("z".into())));
    let mut observer = database.connect();
    let previous_limit = observer
        .query("SELECT current_setting('memory_limit')")?
        .rows;
    assert!(matches!(
        observer.execute("PRAGMA memory_limit='1B'"),
        Err(Error::Resource(_))
    ));
    assert_eq!(
        observer
            .query("SELECT current_setting('memory_limit')")?
            .rows,
        previous_limit
    );
    drop(retained);
    assert!(matches!(
        observer.execute("SET memory_limit='1B'"),
        Err(Error::Resource(_))
    ));
    drop(column);
    observer.execute("SET memory_limit='1B'")?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn memory_limit_rejects_nonportable_or_malformed_inputs() -> Result<()> {
    let mut connection = DatabaseBuilder::new().build()?.connect();
    for setting in ["'10'", "'0.01BG'", "'1 XB'", "NULL", "'101%'"] {
        assert!(matches!(
            connection.execute(&format!("SET memory_limit={setting}")),
            Err(Error::InvalidInput(_)) | Err(Error::Parse(_)) | Err(Error::Conversion(_))
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn defaults_fallbacks_byte_spellings_and_failed_publication_are_atomic() -> Result<()> {
    use duckdb_rust::main::settings::host_memory::FixedMemoryLimitBase;
    for (fallback, expected) in [(false, "7.8 KiB"), (true, "9.7 KiB")] {
        let database = DatabaseBuilder::new()
            .memory_limit_base(Arc::new(FixedMemoryLimitBase {
                bytes: 10_001,
                fallback,
                label: "test",
            }))
            .build()?;
        let mut connection = database.connect();
        assert_eq!(
            connection
                .query("SELECT current_setting('max_memory')")?
                .rows,
            vec![vec![Value::Varchar(expected.into())]]
        );
        for spelling in [
            "1000B",
            "1000 bytes",
            "1k",
            "1KB",
            "1 kilobyte",
            "0.001 megabytes",
            "1e-3 megabytes",
        ] {
            connection.execute(&format!("SET memory_limit='{spelling}'"))?;
            assert_eq!(
                connection
                    .query("SELECT current_setting('memory_limit')")?
                    .rows,
                vec![vec![Value::Varchar("1000 bytes".into())]],
                "{spelling}"
            );
        }
    }
    let pool = Arc::new(MemoryPool::default());
    pool.publish_limit(Some(100))?;
    let query = QueryContext::background().with_memory_pool(pool.clone());
    assert!(
        pool.publish_with(Some(200), || Err::<(), _>(Error::Interrupted))
            .is_err()
    );
    assert_eq!(pool.limit()?, Some(100));
    let charge = pool.reserve(75, &query)?;
    assert!(
        pool.publish_with(Some(50), || -> Result<()> {
            panic!("rejected publication must not run")
        })
        .is_err()
    );
    assert_eq!(pool.limit()?, Some(100));
    drop(charge);
    assert_eq!(pool.used()?, 0);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn memory_pragmas_share_global_settings_and_preserve_failed_publications() -> Result<()> {
    let database = DatabaseBuilder::new().build()?;
    let mut first = database.connect();
    let mut second = database.connect();
    for (sql, expected) in [
        ("PRAGMA memory_limit='2 KiB'", "2.0 KiB"),
        ("PRAGMA max_memory='1024 bytes'", "1.0 KiB"),
    ] {
        first.execute(sql)?;
        assert_eq!(
            second.query("SELECT current_setting('memory_limit')")?.rows,
            vec![vec![Value::Varchar(expected.into())]],
            "{sql}"
        );
        assert_eq!(
            first.query("SELECT current_setting('max_memory')")?.rows,
            vec![vec![Value::Varchar(expected.into())]],
            "{sql}"
        );
    }
    for sql in [
        "PRAGMA memory_limit",
        "PRAGMA memory_limit()",
        "PRAGMA memory_limit('2 KiB')",
        "PRAGMA max_memory('2 KiB')",
        "PRAGMA memory_limit(1, 2)",
        "PRAGMA memory_limit=100",
        "PRAGMA memory_limit='0.01BG'",
        "PRAGMA memory_limit=NULL",
        "PRAGMA memory_limit=?",
    ] {
        assert!(first.execute(sql).is_err(), "{sql}");
        assert_eq!(
            second.query("SELECT current_setting('max_memory')")?.rows,
            vec![vec![Value::Varchar("1.0 KiB".into())]],
            "{sql}"
        );
    }
    for sql in ["PRAGMA memory_limit=-1", "PRAGMA max_memory='none'"] {
        first.execute(sql)?;
        let actual = second.query("SELECT current_setting('memory_limit')")?.rows;
        first.execute("SET max_memory='-1'")?;
        assert_eq!(
            actual,
            second.query("SELECT current_setting('max_memory')")?.rows
        );
    }
    assert!(
        matches!(first.execute("PRAGMA memory_limit()"), Err(Error::Parse(message))
        if message == "syntax error at or near \")\"")
    );
    Ok(())
}
