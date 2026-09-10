use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn variant_dynamic_width_null_members_and_path_presence() -> Result<()> {
    let mut c = Database::memory()?.connect();
    assert_eq!(c.query("SELECT variant_typeof('101'::BIT::VARIANT),('101'::BIT::VARIANT)::BIT::VARCHAR,'101'::BIT::VARIANT<'1010'::BIT::VARIANT,variant_typeof(({'b':'1'::BIT,'a':'0'::BIT}::VARIANT).b)")?.rows,
        vec![vec![Value::Varchar("BITSTRING".into()),Value::Varchar("101".into()),Value::Boolean(true),Value::Varchar("BITSTRING".into())]]);
    assert_eq!(c.query("SELECT variant_typeof({'b':1,'a':2}::VARIANT),variant_typeof({'b':1,'a':NULL}::VARIANT),variant_typeof({'b':1,'a':NULL::INTEGER}::VARIANT),variant_typeof(([{'b':1,'a':2}]::VARIANT)[1]),variant_typeof('x'),variant_typeof(NULL)")?.rows,
        vec![vec![Value::Varchar("OBJECT(a, b)".into()),Value::Varchar("OBJECT(b, a)".into()),Value::Varchar("OBJECT(a, b)".into()),Value::Varchar("OBJECT(b, a)".into()),Value::Varchar("VARCHAR".into()),Value::Varchar("VARIANT_NULL".into())]]);
    assert_eq!(c.query("SELECT map(['x'],[1])::VARIANT::VARCHAR,{'b':'x,y','a':'NULL'}::VARIANT::VARCHAR,[NULL,'NULL','','a,b','a b',' x ']::VARIANT::VARCHAR")?.rows,
        vec![vec![Value::Varchar("[{'key': x, 'value': 1}]".into()),Value::Varchar("{'a': 'NULL', 'b': 'x,y'}".into()),Value::Varchar("[NULL, NULL, , a,b, a b,  x ]".into())]]);
    assert_eq!(c.query("SELECT variant_typeof(1::VARIANT),variant_typeof(1::BIGINT::VARIANT),variant_typeof(1.0::VARIANT),variant_typeof(NULL::VARIANT),variant_typeof(union_value(i:=NULL)::VARIANT),union_value(i:=NULL)::VARIANT IS NULL")?.rows,
        vec![vec![Value::Varchar("INT32".into()),Value::Varchar("INT64".into()),Value::Varchar("DECIMAL(2, 1)".into()),Value::Varchar("VARIANT_NULL".into()),Value::Varchar("VARIANT_NULL".into()),Value::Boolean(true)]]);
    assert_eq!(c.query("SELECT ({'x':NULL::INTEGER,'y':2}::VARIANT).x IS NULL,({'x':1}::VARIANT).missing IS NULL,variant_exists({'x':NULL}::VARIANT,'x'),variant_exists({'x':NULL}::VARIANT,'missing'),variant_type({'x':NULL}::VARIANT,'x'),variant_type({'x':NULL}::VARIANT,'missing'),([union_value(i:=NULL)]::VARIANT)[1] IS NULL")?.rows,
        vec![vec![Value::Boolean(true),Value::Boolean(true),Value::Boolean(true),Value::Boolean(false),Value::Varchar("VARIANT_NULL".into()),Value::Null,Value::Boolean(true)]]);
    assert_eq!(c.query("SELECT ([1,2]::VARIANT)[2]::INTEGER,variant_typeof(map(['x'],[1])::VARIANT),variant_typeof((map(['x'],[1])::VARIANT)[1]),variant_keys({'z':1,'a':2}::VARIANT)[1],variant_array_length([1,2]::VARIANT),variant_array_length(1::VARIANT),variant_exists({'x':1}::VARIANT,['x','missing'])[2]")?.rows,
        vec![vec![Value::Integer(2),Value::Varchar("ARRAY(1)".into()),Value::Varchar("OBJECT(key, value)".into()),Value::Varchar("a".into()),Value::Unsigned(2),Value::Unsigned(0),Value::Boolean(false)]]);
    for sql in [
        "SELECT variant_typeof(1)",
        "SELECT variant_typeof([1,2])",
        "SELECT ([1]::VARIANT)[0]",
        "SELECT ([1]::VARIANT)[-1]",
        "SELECT variant_extract([1]::VARIANT,i) FROM range(1)t(i)",
        "SELECT variant_keys(1::VARIANT,[NULL])",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn variant_comparison_keys_casts_and_mixed_relational_execution() -> Result<()> {
    let mut c = Database::memory()?.connect();
    assert_eq!(c.query("SELECT 'infinity'::DATE::VARIANT='infinity'::TIMESTAMP::VARIANT,'infinity'::TIMESTAMP::VARIANT='infinity'::TIMESTAMP_NS::VARIANT,typeof([100::VARIANT,1.2])")?.rows,
        vec![vec![Value::Boolean(false),Value::Boolean(false),Value::Varchar("VARIANT[]".into())]]);
    assert_eq!(c.query("SELECT 1::VARIANT=1.0::VARIANT,1::VARIANT=1.0::DOUBLE::VARIANT,1.0::FLOAT::VARIANT=1.0::DOUBLE::VARIANT,{'a':1,'b':2}::VARIANT={'b':2.0,'a':1.0}::VARIANT,[1,NULL]::VARIANT>[1,2]::VARIANT,DATE '2000-01-01'::VARIANT=TIMESTAMP_NS '2000-01-01 00:00:00'::VARIANT,map(['x'],[1])::VARIANT=[{'key':'x','value':1.0}]::VARIANT")?.rows,
        vec![vec![Value::Boolean(true),Value::Boolean(false),Value::Boolean(true),Value::Boolean(true),Value::Boolean(true),Value::Boolean(true),Value::Boolean(true)]]);
    assert_eq!(c.query("SELECT (({'xs':[1,NULL],'d':1.25,'ts':TIMESTAMP '2000-01-01'}::VARIANT)::STRUCT(xs BIGINT[],d DECIMAL(12,2),ts TIMESTAMP_NS)).xs[1],((map([1],[2])::VARIANT)::MAP(BIGINT,BIGINT))[1],1::VARIANT::VARCHAR,'x'::VARIANT::VARCHAR,try_cast('x'::VARIANT AS INTEGER)")?.rows,
        vec![vec![Value::Integer(1),Value::Integer(2),Value::Varchar("1".into()),Value::Varchar("x".into()),Value::Null]]);
    c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,v VARIANT); INSERT INTO t VALUES(1,1::VARIANT),(2,1.00::VARIANT),(3,2::VARIANT),(4,1.0::DOUBLE::VARIANT),(5,NULL)")?;
    assert_eq!(
        c.query("SELECT count(DISTINCT v),count(v),count(*) FROM t")?
            .rows,
        vec![vec![
            Value::Integer(3),
            Value::Integer(4),
            Value::Integer(5)
        ]]
    );
    assert_eq!(
        c.query("SELECT count(*) FROM t a JOIN t b ON a.v=b.v")?
            .rows,
        vec![vec![Value::Integer(6)]]
    );
    assert_eq!(
        c.query("SELECT count(*) OVER (PARTITION BY v) FROM t ORDER BY id")?
            .rows,
        vec![
            vec![Value::Integer(2)],
            vec![Value::Integer(2)],
            vec![Value::Integer(1)],
            vec![Value::Integer(1)],
            vec![Value::Integer(1)]
        ]
    );
    assert_eq!(
        c.query("SELECT id FROM t ORDER BY v,id")?.rows,
        (1..=5).map(|i| vec![Value::Integer(i)]).collect::<Vec<_>>()
    );
    let prepared = c.prepare("SELECT count(*) FROM t WHERE v=$1::VARIANT")?;
    assert_eq!(
        c.execute_prepared(&prepared, &[Value::Integer(1)])?.rows,
        vec![vec![Value::Integer(2)]]
    );
    let before = c.query("SELECT * FROM t ORDER BY id")?.rows;
    c.execute("BEGIN; UPDATE t SET v={'d':3.75,'xs':[1,NULL]}::VARIANT WHERE id=1; DELETE FROM t WHERE id=2; ROLLBACK")?;
    assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, before);
    assert!(
        c.execute("CREATE TABLE bad(v VARIANT PRIMARY KEY)")
            .is_err()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn variant_private_reopen_preserves_dynamic_child_metadata() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("variant.json");
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
    c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,v VARIANT); INSERT INTO t VALUES(1,{'d':1.25::DECIMAL(12,2),'xs':[1,NULL],'ts':TIMESTAMP_NS '2000-01-01 00:00:00.123456789'}::VARIANT),(2,1::BIGINT::VARIANT),(3,union_value(i:=NULL)::VARIANT)")?;
    let before = c.query("SELECT * FROM t ORDER BY id")?.rows;
    drop(c);
    let mut c = open()?.connect();
    assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, before);
    assert_eq!(
        c.query("SELECT variant_typeof(v) FROM t ORDER BY id")?.rows,
        vec![
            vec![Value::Varchar("OBJECT(d, xs, ts)".into())],
            vec![Value::Varchar("INT64".into())],
            vec![Value::Varchar("VARIANT_NULL".into())]
        ]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn bignum_variant_and_union_preserve_exact_values_across_typed_execution() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("bignum-variant.json");
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
    assert_eq!(c.query("SELECT variant_typeof('340282366920938463463374607431768211456'::BIGNUM::VARIANT),(-0.5::DOUBLE)::BIGNUM::VARIANT::BIGNUM::VARCHAR,((-0.5::DOUBLE)::BIGNUM::VARIANT)=(0::BIGNUM::VARIANT),(1::BIGNUM::VARIANT)=(1::INTEGER::VARIANT),(1::BIGNUM::VARIANT)=(1::DOUBLE::VARIANT),('340282366920938463463374607431768211456'::BIGNUM::VARIANT)=(340282366920938463463374607431768211456.0::DOUBLE::VARIANT),(1::BIGNUM::UNION(n BIGNUM)).n::VARCHAR,variant_typeof({'z':1::BIGNUM,'a':2}::VARIANT)")?.rows,
        vec![vec![Value::Varchar("BIGNUM".into()),Value::Varchar("-0".into()),Value::Boolean(true),Value::Boolean(true),Value::Boolean(false),Value::Boolean(false),Value::Varchar("1".into()),Value::Varchar("OBJECT(z, a)".into())]]);
    assert_eq!(c.query("SELECT ({'xs':['340282366920938463463374607431768211456'::BIGNUM,(-0.5::DOUBLE)::BIGNUM]}::VARIANT).xs[2]::BIGNUM::VARCHAR,([1::BIGNUM,NULL]::VARIANT)::BIGNUM[]::VARCHAR,(union_value(n:=(-0.5::DOUBLE)::BIGNUM)::VARIANT)::BIGNUM::VARCHAR")?.rows,
        vec![vec![Value::Varchar("-0".into()),Value::Varchar("[1, NULL]".into()),Value::Varchar("-0".into())]]);
    c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,v VARIANT); INSERT INTO t VALUES(1,(-0.5::DOUBLE)::BIGNUM::VARIANT),(2,0::BIGNUM::VARIANT),(3,0::INTEGER::VARIANT),(4,1::BIGNUM::VARIANT),(5,1.00::DECIMAL(12,2)::VARIANT),(6,'340282366920938463463374607431768211456'::BIGNUM::VARIANT),(7,1.0::DOUBLE::VARIANT),(8,NULL)")?;
    assert_eq!(
        c.query("SELECT count(*) FROM(SELECT DISTINCT v FROM t)")?
            .rows,
        vec![vec![Value::Integer(5)]]
    );
    assert_eq!(
        c.query("SELECT count(*) FROM t a JOIN t b ON a.v=b.v")?
            .rows,
        vec![vec![Value::Integer(15)]]
    );
    assert_eq!(
        c.query("SELECT id,count(*) OVER(PARTITION BY v) FROM t ORDER BY v,id")?
            .rows,
        [3, 3, 3, 2, 2, 1, 1, 1]
            .into_iter()
            .enumerate()
            .map(|(i, n)| vec![Value::Integer(i as i128 + 1), Value::Integer(n)])
            .collect::<Vec<_>>()
    );
    let prepared = c.prepare("SELECT count(*) FROM t WHERE v=$1::VARIANT")?;
    let minus = duckdb_rust::common::BignumValue::from_f64(-0.5)?.value();
    assert_eq!(
        c.execute_prepared(&prepared, &[minus])?.rows,
        vec![vec![Value::Integer(3)]]
    );
    let before = c.query("SELECT * FROM t ORDER BY id")?.rows;
    c.execute("BEGIN; UPDATE t SET v={'n':'340282366920938463463374607431768211456'::BIGNUM}::VARIANT WHERE id=1; DELETE FROM t WHERE id=2; ROLLBACK")?;
    assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, before);
    c.execute("UPDATE t SET v={'n':'340282366920938463463374607431768211456'::BIGNUM}::VARIANT WHERE id=8")?;
    let after = c.query("SELECT * FROM t ORDER BY id")?.rows;
    drop(c);
    let mut c = open()?.connect();
    assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, after);
    assert_eq!(
        c.query("SELECT v::BIGNUM::VARCHAR FROM t WHERE id=1")?.rows,
        vec![vec![Value::Varchar("-0".into())]]
    );
    assert_eq!(
        c.query("SELECT v.n::BIGNUM::VARCHAR FROM t WHERE id=8")?
            .rows,
        vec![vec![Value::Varchar(
            "340282366920938463463374607431768211456".into()
        )]]
    );
    Ok(())
}
