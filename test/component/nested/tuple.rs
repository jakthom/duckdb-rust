use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn tuple_literals_empty_shapes_positional_casts_and_accessors() -> Result<()> {
    use duckdb_rust::parser::{DuckDbParser, Parser, Statement};
    let parsed = DuckDbParser.parse("SELECT (42,),(),(42),(1,'a')")?;
    let Statement::Sql(sql) = &parsed[0] else {
        panic!("tuple SQL AST");
    };
    assert!(sql.to_string().contains("(42,)"));
    assert_eq!(DuckDbParser.parse(&sql.to_string())?.len(), 1);
    let mut c = Database::memory()?.connect();
    assert_eq!(c.query("SELECT typeof(()),typeof((42,)),typeof((42)),typeof((1,'a')),typeof(row()),typeof(struct_pack()),(42,)::VARCHAR,()::VARCHAR,struct_pack()::VARCHAR")?.rows,
        vec![vec![Value::Varchar("TUPLE".into()),Value::Varchar("TUPLE(INTEGER)".into()),Value::Varchar("INTEGER".into()),Value::Varchar("TUPLE(INTEGER, VARCHAR)".into()),Value::Varchar("TUPLE".into()),Value::Varchar("STRUCT".into()),Value::Varchar("(42,)".into()),Value::Varchar("()".into()),Value::Varchar("{}".into())]]);
    assert_eq!(c.query("SELECT row(1,'a')[2],struct_extract(row(1,'a'),1),array_extract(row(1,'a'),2),struct_extract_at({'a':1,'b':'x'},2),struct_values({'a':1,'b':'x'})[2],struct_values(row(1,'a'))[1],row()::STRUCT=struct_pack(),typeof([row(1,2),{'x':3,'y':4}]),typeof([{'x':3,'y':4},row(1,2)]),row(1,2)={'x':1,'y':2}")?.rows,
        vec![vec![Value::Varchar("a".into()),Value::Integer(1),Value::Varchar("a".into()),Value::Varchar("x".into()),Value::Varchar("x".into()),Value::Integer(1),Value::Boolean(true),Value::Varchar("STRUCT(x INTEGER, y INTEGER)[]".into()),Value::Varchar("STRUCT(x INTEGER, y INTEGER)[]".into()),Value::Boolean(true)]]);
    assert_eq!(c.query("SELECT (row(1,'a')::STRUCT(x BIGINT,y VARCHAR)).x,({'y':1,'x':2}::TUPLE(BIGINT,INTEGER))[2],variant_typeof(row(1,'a')::VARIANT),(row(1,'a')::VARIANT)[2]::VARCHAR,([1,'a'::VARIANT]::VARIANT::TUPLE(BIGINT,VARCHAR))[1],struct_keys(struct_pack())::VARCHAR")?.rows,
        vec![vec![Value::Integer(1),Value::Integer(2),Value::Varchar("ARRAY(2)".into()),Value::Varchar("a".into()),Value::Integer(1),Value::Varchar("[]".into())]]);
    for sql in [
        "SELECT row(1)['x']",
        "SELECT row(1)[0]",
        "SELECT row(1)[2]",
        "SELECT row(1)[-1]",
        "SELECT struct_keys(row(1))",
        "SELECT row(1,2)::TUPLE(INTEGER)",
        "SELECT row(1)[i] FROM range(1)t(i)",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn tuple_mixed_relational_parameters_mutations_and_private_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("tuple.json");
    let open = || -> Result<Database> {
        DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(JsonSnapshotFormat),
            )?))
            .build()
    };
    let mut c = open()?.connect();
    c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,v TUPLE(DECIMAL(12,2),TIMESTAMP_NS,INTEGER[],BIT)); INSERT INTO t VALUES(1,row(1.25,TIMESTAMP_NS '2000-01-01 00:00:00.123456789',[1,NULL],'101'::BIT)),(2,row(1.25,TIMESTAMP_NS '2000-01-01 00:00:00.123456789',[1,NULL],'101'::BIT)),(3,row(NULL,NULL,[],NULL)),(4,NULL)")?;
    assert_eq!(
        c.query("SELECT count(DISTINCT v),count(v) FROM t")?.rows,
        vec![vec![Value::Integer(2), Value::Integer(3)]]
    );
    assert_eq!(
        c.query("SELECT count(*) FROM t a JOIN t b ON a.v=b.v")?
            .rows,
        vec![vec![Value::Integer(5)]]
    );
    assert_eq!(
        c.query("SELECT count(*) OVER(PARTITION BY v) FROM t ORDER BY id")?
            .rows,
        vec![
            vec![Value::Integer(2)],
            vec![Value::Integer(2)],
            vec![Value::Integer(1)],
            vec![Value::Integer(1)]
        ]
    );
    let value = c.query("SELECT v FROM t WHERE id=1")?.rows[0][0].clone();
    let prepared = c.prepare("SELECT count(*) FROM t WHERE v=$1")?;
    assert_eq!(
        c.execute_prepared(&prepared, &[value])?.rows,
        vec![vec![Value::Integer(2)]]
    );
    let before = c.query("SELECT * FROM t ORDER BY id")?.rows;
    c.execute("BEGIN; UPDATE t SET v=row(2.50,TIMESTAMP_NS '2001-01-01 00:00:00',[7],'0'::BIT) WHERE id=1; DELETE FROM t WHERE id=2; ROLLBACK")?;
    assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, before);
    c.execute(
        "UPDATE t SET v=row(2.50,TIMESTAMP_NS '2001-01-01 00:00:00',[7],'0'::BIT) WHERE id=1",
    )?;
    let after = c.query("SELECT * FROM t ORDER BY id")?.rows;
    drop(c);
    let mut c = open()?.connect();
    assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, after);
    assert_eq!(
        c.query("SELECT v[3][1],v[4]::VARCHAR FROM t WHERE id=1")?
            .rows,
        vec![vec![Value::Integer(7), Value::Varchar("0".into())]]
    );
    assert!(
        c.execute("CREATE TABLE bad(k TUPLE(INTEGER) PRIMARY KEY)")
            .is_err()
    );
    Ok(())
}
