use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn concat_selects_typed_sequences_and_preserves_null_distinctions() -> Result<()> {
    let mut c = Database::memory()?.connect();
    assert_eq!(c.query("SELECT concat([1,NULL],[2],NULL)::VARCHAR,concat(NULL::INTEGER[],NULL::INTEGER[])::VARCHAR,typeof(concat([],NULL)),concat([true],[1])::VARCHAR,concat([{'a':1}],[{'b':2}])::VARCHAR,typeof(concat([1]::INTEGER[1],[2]::BIGINT[1])),typeof(concat([make_timestamp_ns(-9223372036854775806)]))")?.rows,
        vec![vec![Value::Varchar("[1, NULL, 2]".into()),Value::Varchar("[]".into()),Value::Varchar("\"NULL\"[]".into()),Value::Varchar("[1, 1]".into()),Value::Varchar("[{'a': 1, 'b': NULL}, {'a': NULL, 'b': 2}]".into()),Value::Varchar("BIGINT[]".into()),Value::Varchar("TIMESTAMP_NS[]".into())]]);
    assert_eq!(c.query("SELECT concat([{'b':true}],[{'b':2::UTINYINT}])::VARCHAR,typeof(concat([{'b':true}],[{'b':2::UTINYINT}])),concat(NULL::INTEGER[2],[1]::INTEGER[1])::VARCHAR,concat([1],NULL::INTEGER[]) IS NULL,concat(NULL,NULL)")?.rows,
        vec![vec![Value::Varchar("[{'b': 1}, {'b': 2}]".into()),Value::Varchar("STRUCT(b UTINYINT)[]".into()),Value::Varchar("[1]".into()),Value::Boolean(false),Value::Varchar("".into())]]);
    let source = c
        .query("SELECT [make_timestamp_ns(-9223372036854775806)]")?
        .rows;
    assert_eq!(
        c.query("SELECT concat([make_timestamp_ns(-9223372036854775806)])")?
            .rows,
        source
    );
    for sql in [
        "SELECT concat([1],1)",
        "SELECT concat([1],'[2]')",
        "SELECT concat([1],['2'])",
        "SELECT concat([true],[1.0::DOUBLE])",
        "SELECT concat([true],[1.2::DECIMAL(2,1)])",
        "SELECT concat([1],{'n':2})",
    ] {
        assert!(
            matches!(c.query(sql), Err(duckdb_rust::Error::Bind(_))),
            "{sql}"
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn concat_mixed_children_flow_through_parameters_relations_mutations_and_native_reopen()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("concat.duckdb");
    let mut c = Database::open(&path)?.connect();
    c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,xs STRUCT(n DECIMAL(12,2),ts TIMESTAMP_NS,b BIT)[]); INSERT INTO t VALUES(1,concat([{'n':1.25,'ts':TIMESTAMP_NS '2000-01-01 00:00:00.123456789','b':'101'::BIT}],NULL,[{'n':NULL,'ts':NULL,'b':NULL}])),(2,concat(NULL::STRUCT(n DECIMAL(12,2),ts TIMESTAMP_NS,b BIT)[],[]))")?;
    let prepared=c.prepare("INSERT INTO t VALUES($1,concat($2::STRUCT(n DECIMAL(12,2),ts TIMESTAMP_NS,b BIT)[],[NULL]))")?;
    let value = c.query("SELECT xs FROM t WHERE id=1")?.rows[0][0].clone();
    c.execute_prepared(&prepared, &[Value::Integer(3), value])?;
    let before = c.query("SELECT * FROM t ORDER BY id")?.rows;
    c.execute(
        "BEGIN; UPDATE t SET xs=concat(xs,[NULL]) WHERE id=1; DELETE FROM t WHERE id=2; ROLLBACK",
    )?;
    assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, before);
    c.execute("UPDATE t SET xs=concat(xs,[NULL]) WHERE id=1")?;
    assert_eq!(
        c.query("SELECT count(*) FROM t a JOIN t b ON a.xs=b.xs")?
            .rows,
        vec![vec![Value::Integer(5)]]
    );
    assert_eq!(
        c.query("SELECT id,count(*) OVER(PARTITION BY xs) FROM t ORDER BY id")?
            .rows,
        vec![
            vec![Value::Integer(1), Value::Integer(2)],
            vec![Value::Integer(2), Value::Integer(1)],
            vec![Value::Integer(3), Value::Integer(2)]
        ]
    );
    let after = c.query("SELECT * FROM t ORDER BY id")?.rows;
    drop(c);
    let mut c = Database::open_read_only(&path)?.connect();
    assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, after);
    assert_eq!(
        c.query("SELECT xs[1].n,xs[1].b::VARCHAR FROM t WHERE id=3")?
            .rows,
        vec![vec![
            Value::Decimal {
                value: 125,
                width: 12,
                scale: 2
            },
            Value::Varchar("101".into())
        ]]
    );
    Ok(())
}
