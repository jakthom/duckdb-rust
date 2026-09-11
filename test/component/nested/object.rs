//! Internal dynamic OBJECT metadata, not a new SQL STRUCT spelling.
use super::*;
use duckdb_rust::common::{
    TemporalValue,
    cast::{CastFunction, CastMode, CastRegistry, CastSpec},
    type_registry::TypeRegistry,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn object(fields: Vec<(&str, DataType, Value)>) -> Result<Value> {
    let ty = NestedType::Object(
        fields
            .iter()
            .map(|(name, ty, _)| ((*name).into(), ty.clone()))
            .collect(),
    )
    .data_type();
    NestedValue::value(
        ty,
        NestedPayload::Struct(fields.into_iter().map(|(_, _, value)| value).collect()),
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn variant(value: Value) -> Result<Value> {
    NestedValue::value(
        NestedType::Variant.data_type(),
        NestedPayload::Variant {
            data_type: value.data_type(),
            value,
        },
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn internal_object_names_are_exact_without_weakening_struct_metadata() -> Result<()> {
    let value = object(vec![
        ("", DataType::Integer, Value::Integer(1)),
        ("A", DataType::Integer, Value::Integer(2)),
        ("a", DataType::Integer, Value::Null),
    ])?;
    let types = builtin_types();
    types
        .bind(&value.data_type())?
        .validate(&value, &QueryContext::background())?;
    assert!(!types.bind(&value.data_type())?.supports_index());
    for metadata in [
        NestedType::Object(vec![
            ("a".into(), DataType::Integer),
            ("a".into(), DataType::Integer),
        ]),
        NestedType::Struct(vec![("".into(), DataType::Integer)]),
        NestedType::Struct(vec![
            ("A".into(), DataType::Integer),
            ("a".into(), DataType::Integer),
        ]),
    ] {
        assert!(types.bind(&metadata.data_type()).is_err());
    }
    let mut c = Database::memory()?.connect();
    let parameter = c.prepare("SELECT variant_typeof($1::VARIANT),variant_extract($1::VARIANT,'')::INTEGER,variant_extract($1::VARIANT,'A')::INTEGER,variant_extract($1::VARIANT,'a')::INTEGER,variant_exists($1::VARIANT,[''])[1],variant_exists($1::VARIANT,['a'])[1],variant_exists($1::VARIANT,['missing'])[1]")?;
    assert_eq!(
        c.execute_prepared(&parameter, std::slice::from_ref(&value))?
            .rows,
        vec![vec![
            Value::Varchar("OBJECT(, A, a)".into()),
            Value::Integer(1),
            Value::Integer(2),
            Value::Null,
            Value::Boolean(true),
            Value::Boolean(true),
            Value::Boolean(false),
        ]]
    );
    let wrapped = variant(value)?;
    let parameter = c.prepare("SELECT variant_keys($1)::VARCHAR,($1)::VARCHAR")?;
    assert_eq!(
        c.execute_prepared(&parameter, &[wrapped])?.rows,
        vec![vec![
            Value::Varchar("['', A, a]".into()),
            Value::Varchar("{'': 1, 'A': 2, 'a': NULL}".into()),
        ]]
    );
    assert!(c.execute("CREATE TABLE forbidden(v OBJECT)").is_err());
    assert!(std::mem::size_of::<Value>() <= 32);
    assert!(std::mem::size_of::<DataType>() <= 16);
    Ok(())
}

#[derive(Debug)]
struct SelectedText(&'static str);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedText {
    fn name(&self) -> &'static str {
        "object-selected-child-text"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Integer
            && spec.target == DataType::Varchar
            && spec.mode == CastMode::Explicit
    }
    fn cast(&self, _: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        Ok(Value::Varchar(self.0.into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn internal_object_text_retains_selected_children_and_batch_validity() -> Result<()> {
    let value = object(vec![
        ("", DataType::Integer, Value::Integer(1)),
        ("a", DataType::Integer, Value::Null),
    ])?;
    let mut casts = CastRegistry::builtins();
    let spec = CastSpec {
        source: DataType::Integer,
        target: DataType::Varchar,
        mode: CastMode::Explicit,
    };
    casts.replace(spec.clone(), Arc::new(SelectedText("retained")))?;
    let types = builtin_types();
    let direct = casts.bind(
        &value.data_type(),
        &DataType::Varchar,
        CastMode::Explicit,
        &types,
    )?;
    let nested = casts.bind(
        &NestedType::Variant.data_type(),
        &DataType::Varchar,
        CastMode::Explicit,
        &types,
    )?;
    casts.replace(spec, Arc::new(SelectedText("replacement")))?;
    let query = QueryContext::background().with_types(Arc::new(TypeRegistry::default()));
    let expected = Value::Varchar("{'': retained, 'a': NULL}".into());
    assert_eq!(direct.apply(&value, &query)?, expected);
    let wrapped = variant(value)?;
    assert_eq!(nested.apply(&wrapped, &query)?, expected);
    let batch = Vector::flat(NestedType::Variant.data_type(), vec![wrapped, Value::Null])?;
    assert_eq!(
        nested
            .apply_batch(&batch, &query)?
            .values()
            .cloned()
            .collect::<Vec<_>>(),
        vec![expected, Value::Null]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn internal_objects_carry_mixed_children_through_keys_mutations_and_private_reopen() -> Result<()> {
    let fields = vec![
        (
            "",
            DataType::Decimal {
                width: 12,
                scale: 2,
            },
            Value::Decimal {
                value: 125,
                width: 12,
                scale: 2,
            },
        ),
        (
            "A",
            DataType::TimestampNs,
            Value::Temporal(TemporalValue::parse(
                "2000-01-01 00:00:00.123456789",
                &DataType::TimestampNs,
            )?),
        ),
        ("a", DataType::Integer, Value::Null),
    ];
    let first = variant(object(fields.clone())?)?;
    let second = variant(object(fields.into_iter().rev().collect())?)?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("object.json");
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
    c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,v VARIANT)")?;
    let insert = c.prepare("INSERT INTO t VALUES($1,$2)")?;
    c.execute_prepared(&insert, &[Value::Integer(1), first])?;
    c.execute_prepared(&insert, &[Value::Integer(2), second])?;
    // This pre-existing shape is intentionally still supported; introducing
    // Object must not invalidate private VARIANTs carrying declared STRUCT.
    c.execute("INSERT INTO t VALUES(3,{'legacy':1}::VARIANT),(4,NULL)")?;
    assert_eq!(
        c.query("SELECT count(*) FROM t a JOIN t b ON a.v=b.v")?
            .rows,
        vec![vec![Value::Integer(5)]]
    );
    assert_eq!(
        c.query("SELECT count(*) FROM (SELECT v FROM t GROUP BY v) q")?
            .rows,
        vec![vec![Value::Integer(3)]]
    );
    assert_eq!(
        c.query("SELECT count(*) OVER(PARTITION BY v) FROM t ORDER BY id")?
            .rows,
        [2, 2, 1, 1]
            .into_iter()
            .map(|n| vec![Value::Integer(n)])
            .collect::<Vec<_>>()
    );
    assert_eq!(c.query("SELECT variant_extract(v,'')::DECIMAL(12,2),variant_extract(v,'A')::TIMESTAMP_NS::VARCHAR,variant_exists(v,['a'])[1] FROM t WHERE id=1")?.rows,
        vec![vec![Value::Decimal {value:125,width:12,scale:2},Value::Varchar("2000-01-01 00:00:00.123456789".into()),Value::Boolean(true)]]);
    let before = c.query("SELECT * FROM t ORDER BY id")?.rows;
    c.execute("BEGIN; UPDATE t SET v=NULL; DELETE FROM t WHERE id=1; ROLLBACK")?;
    assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, before);
    c.execute("UPDATE t SET id=id+10")?;
    let expected = c.query("SELECT * FROM t ORDER BY id")?.rows;
    drop(c);
    let mut c = open()?.connect();
    assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, expected);
    assert_eq!(
        c.query("SELECT count(*) FROM t a JOIN t b ON a.v=b.v")?
            .rows,
        vec![vec![Value::Integer(5)]]
    );
    assert_eq!(
        c.query("SELECT v::VARCHAR FROM t WHERE id=11")?.rows,
        vec![vec![Value::Varchar(
            "{'': 1.25, 'A': '2000-01-01 00:00:00.123456789', 'a': NULL}".into()
        )]]
    );
    Ok(())
}
