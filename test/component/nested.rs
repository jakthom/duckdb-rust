use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Result, Value,
    common::{
        NestedPayload, NestedType, NestedValue, type_registry::builtin_types, vector::Vector,
    },
    parallel::QueryContext,
    storage::{checkpoint::FileCheckpoint, filesystem::OpenMode, format::JsonSnapshotFormat},
};
use std::{cmp::Ordering, sync::Arc};

#[path = "nested/accessors.rs"]
mod accessors;
#[path = "nested/concat.rs"]
mod concat;
#[path = "nested/native.rs"]
mod native;
#[path = "nested/tuple.rs"]
mod tuple;
#[path = "nested/variant.rs"]
mod variant;
#[path = "nested/variant_native.rs"]
mod variant_native;
#[path = "nested/wal.rs"]
mod wal;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn nested_sql_constructors_casts_and_accessors() -> Result<()> {
    let mut c = Database::memory()?.connect();
    for sql in [
        "CREATE TABLE bad_list(k INTEGER[] PRIMARY KEY)",
        "CREATE TABLE bad_struct(k STRUCT(a INTEGER) UNIQUE)",
        "CREATE TABLE bad_map(k MAP(INTEGER,VARCHAR) PRIMARY KEY)",
        "CREATE TABLE bad_union(k UNION(a INTEGER,b VARCHAR) UNIQUE)",
    ] {
        assert!(c.execute(sql).is_err(), "{sql}");
    }
    assert_eq!(c.query("SELECT list_extract([1,NULL,3],-1), list_extract([1],0), list_extract([1],2), struct_extract({'a':12.50::DECIMAL(5,2),'b':[1,NULL]},'a')")?.rows, vec![vec![Value::Integer(3), Value::Null, Value::Null, Value::Decimal { value: 1250, width:5, scale:2 }]]);
    assert_eq!(c.query("SELECT list_extract([1,2]::BIGINT[],2), list_extract([1,2]::INTEGER[2],1), [1,NULL]=[1,NULL], [1,NULL]>[1,2], {'a':1,'b':NULL}>{'a':1,'b':2}, map([2,1],['a','b'])=map([1,2],['b','a'])")?.rows, vec![vec![Value::Integer(2), Value::Integer(1), Value::Boolean(true), Value::Boolean(true), Value::Boolean(true), Value::Boolean(false)]]);
    for sql in [
        "SELECT [1]::INTEGER[2]",
        "SELECT map([1,1],[2,3])",
        "SELECT map([NULL],[2])",
        "SELECT map([1,2],[2])",
        "SELECT struct_extract({'a':1},'missing')",
        "SELECT {'a':1,'A':2}",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn union_members_and_list_aggregates_preserve_nested_nulls() -> Result<()> {
    let mut c = Database::memory()?.connect();
    assert_eq!(c.query("SELECT (NULL::INTEGER)::UNION(i INTEGER,s VARCHAR) IS NULL, NULL::UNION(i INTEGER,s VARCHAR) IS NULL, union_extract((NULL::INTEGER)::UNION(i INTEGER,s VARCHAR),'i')")?.rows,vec![vec![Value::Boolean(false),Value::Boolean(true),Value::Null]]);
    c.execute("CREATE TABLE union_nulls(i INTEGER); INSERT INTO union_nulls VALUES (NULL),(1)")?;
    assert_eq!(
        c.query("SELECT i::UNION(i INTEGER,s VARCHAR) IS NULL FROM union_nulls")?
            .rows,
        vec![vec![Value::Boolean(false)], vec![Value::Boolean(false)]]
    );
    assert_eq!(c.query("SELECT union_extract(union_value(i:=1)::UNION(s VARCHAR,i BIGINT),'i'),union_extract(union_value(i:=1)::UNION(s VARCHAR,i BIGINT),'s'),union_value(i:=NULL) IS NULL,union_value(i:=NULL)::VARCHAR,union_extract(7::UNION(i INTEGER,s VARCHAR),'i')")?.rows,vec![vec![Value::Integer(1),Value::Null,Value::Boolean(false),Value::Varchar("NULL".into()),Value::Integer(7)]]);
    assert!(c.query("SELECT 1::UNION(a INTEGER,b INTEGER)").is_err());
    assert_eq!(c.query("SELECT list_extract(list(i),2),list_extract(array_agg(i),-1) FROM (VALUES (1),(NULL),(3))t(i)")?.rows,vec![vec![Value::Null,Value::Integer(3)]]);
    assert_eq!(
        c.query("SELECT list(i) FROM range(0)t(i)")?.rows,
        vec![vec![Value::Null]]
    );
    assert_eq!(c.query("SELECT list_extract(list(i) OVER (ORDER BY i ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW),-1) FROM range(3)t(i)")?.rows,vec![vec![Value::Integer(0)],vec![Value::Integer(1)],vec![Value::Integer(2)]]);
    assert_eq!(
        c.query("SELECT [1,2][2],{'a':[1,NULL]}.a[-1],s.a FROM (SELECT struct_pack(a:=9) s)t")?
            .rows,
        vec![vec![Value::Integer(2), Value::Null, Value::Integer(9)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn nested_values_flow_through_relational_operators() -> Result<()> {
    let mut c = Database::memory()?.connect();
    c.execute("CREATE TABLE t(k INTEGER[], s STRUCT(a DECIMAL(8,2), b INTEGER[])); INSERT INTO t VALUES ([1,NULL],{'a':1.25,'b':[1,2]}),([1,NULL],{'a':2.50,'b':[]}),([1,2],{'a':3.75,'b':NULL}),(NULL,NULL)")?;
    assert_eq!(
        c.query("SELECT count(DISTINCT k),count(*) FROM t")?.rows,
        vec![vec![Value::Integer(2), Value::Integer(4)]]
    );
    assert_eq!(
        c.query("SELECT count(*) FROM t a JOIN t b ON a.k=b.k")?
            .rows,
        vec![vec![Value::Integer(5)]]
    );
    assert_eq!(
        c.query("SELECT count(*) FROM (SELECT k,count(*) FROM t GROUP BY k) q")?
            .rows,
        vec![vec![Value::Integer(3)]]
    );
    assert_eq!(
        c.query("SELECT list_extract(min(k),2), list_extract(max(k),2) FROM t")?
            .rows,
        vec![vec![Value::Integer(2), Value::Null]]
    );
    assert_eq!(
        c.query("SELECT count(*) OVER (PARTITION BY k) FROM t ORDER BY k")?
            .rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(2)],
            vec![Value::Integer(1)]
        ]
    );
    assert_eq!(
        c.query("SELECT count(*) FROM (SELECT k FROM t UNION SELECT [1,NULL]) q")?
            .rows,
        vec![vec![Value::Integer(3)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn nested_shape_null_keys_and_vector_encodings() -> Result<()> {
    assert!(std::mem::size_of::<Value>() <= 32);
    assert!(std::mem::size_of::<DataType>() <= 16);
    let unresolved = NestedType::Struct(vec![
        ("xs".into(), NestedType::List(DataType::Null).data_type()),
        ("number".into(), DataType::Decimal { width: 8, scale: 2 }),
    ])
    .data_type();
    assert_eq!(
        duckdb_rust::common::nested::normalize_storage_type(&unresolved)?,
        NestedType::Struct(vec![
            ("xs".into(), NestedType::List(DataType::Integer).data_type()),
            ("number".into(), DataType::Decimal { width: 8, scale: 2 })
        ])
        .data_type()
    );
    let variant = NestedType::Variant.data_type();
    let mut too_deep = Value::Integer(1);
    for _ in 0..66 {
        too_deep = Value::Nested(Arc::new(NestedValue {
            data_type: variant.clone(),
            payload: NestedPayload::Variant {
                data_type: too_deep.data_type(),
                value: too_deep,
            },
        }));
    }
    assert!(!too_deep.fits_type(&variant));
    let ty = NestedType::List(DataType::Integer).data_type();
    let a = NestedValue::value(
        ty.clone(),
        NestedPayload::Sequence(vec![Value::Integer(1), Value::Null]),
    )?;
    let b = NestedValue::value(
        ty.clone(),
        NestedPayload::Sequence(vec![Value::Integer(1), Value::Integer(2)]),
    )?;
    let bound = builtin_types().bind(&ty)?;
    let query = QueryContext::background();
    assert_eq!(bound.compare(&a, &b, &query)?, Ordering::Greater);
    let mut keys = Vec::new();
    for value in [&a, &b, &Value::Null] {
        let mut key = Vec::new();
        bound.append_key(value, &mut key, &query)?;
        keys.push(key);
    }
    assert_ne!(keys[0], keys[1]);
    assert_ne!(keys[0], keys[2]);
    let vector = Vector::flat(ty.clone(), vec![a.clone(), b.clone(), Value::Null])?;
    bound.validate_vector(&vector, &query)?;
    let selected = Arc::new(vector).select(vec![2, 0, 0, 1])?;
    bound.validate_vector(&selected, &query)?;
    assert_eq!(
        selected.values().cloned().collect::<Vec<_>>(),
        vec![Value::Null, a.clone(), a, b]
    );
    assert!(
        NestedValue::value(
            NestedType::Array {
                element: DataType::Integer,
                length: 2
            }
            .data_type(),
            NestedPayload::Sequence(vec![Value::Integer(1)])
        )
        .is_err()
    );
    assert!(
        NestedValue::value(
            ty,
            NestedPayload::Sequence(vec![Value::Varchar("bad".into())])
        )
        .is_err()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn mixed_nested_mutations_rollback_parameters_and_snapshot_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("nested.snapshot");
    {
        let db = DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(JsonSnapshotFormat),
            )?))
            .build()?;
        let mut c = db.connect();
        c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v STRUCT(n DECIMAL(8,2), d DATE, xs UBIGINT[])); INSERT INTO t VALUES (1,{'n':1.25,'d':DATE '2000-02-29','xs':[1::UBIGINT,NULL]})")?;
        c.execute("BEGIN; UPDATE t SET v={'n':2.50,'d':DATE '1970-01-01','xs':[]} WHERE id=1; DELETE FROM t; ROLLBACK")?;
        let prepared = c.prepare("SELECT struct_extract($1,'n')")?;
        let value = c.query("SELECT v FROM t")?.rows[0][0].clone();
        assert_eq!(
            c.execute_prepared(&prepared, &[value])?.rows,
            vec![vec![Value::Decimal {
                value: 125,
                width: 8,
                scale: 2
            }]]
        );
    }
    let db = DatabaseBuilder::new()
        .durability(Arc::new(FileCheckpoint::open(
            &path,
            OpenMode::ReadWrite,
            Arc::new(JsonSnapshotFormat),
        )?))
        .build()?;
    let mut c = db.connect();
    assert_eq!(c.query("SELECT struct_extract(v,'n'),list_extract(struct_extract(v,'xs'),1),struct_extract(v,'d')=DATE '2000-02-29' FROM t WHERE id=1")?.rows,vec![vec![Value::Decimal { value:125,width:8,scale:2 },Value::Unsigned(1),Value::Boolean(true)]]);
    Ok(())
}
