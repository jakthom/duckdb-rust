use super::*;

#[derive(Debug)]
struct SelectedListExtract;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::function::ScalarFunction for SelectedListExtract {
    fn name(&self) -> &str {
        "list_extract"
    }
    fn return_type(
        &self,
        _: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        Ok(DataType::Integer)
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        Ok(Value::Integer(42))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn subscript_syntax_uses_the_selected_scalar_catalog() -> Result<()> {
    let mut functions = duckdb_rust::function::FunctionRegistry::default();
    functions.register_scalar(Arc::new(SelectedListExtract))?;
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    assert_eq!(
        c.query("SELECT [1,2][1],list_extract([1,2],1)")?.rows,
        vec![vec![Value::Integer(42), Value::Integer(42)]]
    );
    let parameter = c.prepare("SELECT t.xs[$1] FROM (VALUES ([1,2]),([3,4])) t(xs)")?;
    assert_eq!(
        c.execute_prepared(&parameter, &[Value::Integer(1)])?.rows,
        vec![vec![Value::Integer(42)]; 2]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn qualified_nested_names_preserve_precedence_grouping_and_correlations() -> Result<()> {
    let mut c = Database::memory()?.connect();
    for (sql, expected) in [
        ("SELECT b.xs[1] FROM (VALUES ([11])) b(xs)", 11),
        ("SELECT a.b.xs[1] FROM (SELECT {'b':{'xs':[12]}} a) t", 12),
        ("SELECT t.a.b.xs[1] FROM (SELECT {'b':{'xs':[12]}} a) t", 12),
        (
            "SELECT t.a.b.c.d FROM (SELECT {'b':{'c':{'d':13}}} a) t",
            13,
        ),
        ("SELECT b.xs[1] FROM (SELECT [11] xs, {'xs':[99]} b) b", 11),
        (
            "SELECT (b).xs[1] FROM (SELECT [11] xs, {'xs':[99]} b) b",
            99,
        ),
        (
            "SELECT b.xs[1] FROM (SELECT [11] xs, {'xs':[99]} b) other",
            99,
        ),
        (
            "SELECT \"t.q\".\"s.x\".\"i.n\".xs[1] FROM (SELECT {'i.n':{'xs':[14]}} AS \"s.x\") AS \"t.q\"",
            14,
        ),
        (
            "SELECT t.a.b[1] FROM (SELECT {'b':[1]} a) t GROUP BY t.a.b[1]",
            1,
        ),
        (
            "SELECT t.a.b[1] FROM (SELECT {'b':[1]} a) t GROUP BY t.a",
            1,
        ),
        ("SELECT (SELECT t.xs[1]) FROM (SELECT [1] xs) t", 1),
        (
            "SELECT (SELECT t.xs[1] FROM (SELECT {'xs':[2]} t) q) FROM (SELECT [1] xs) t",
            2,
        ),
        (
            "SELECT (SELECT t.a.b[1]) FROM (SELECT {'b':[3]} a) t GROUP BY t.a",
            3,
        ),
    ] {
        assert_eq!(
            c.query(sql)?.rows,
            vec![vec![Value::Integer(expected)]],
            "{sql}"
        );
    }
    assert_eq!(
        c.query("SELECT [42] xs,row_number() OVER () n QUALIFY xs[1]=42")?
            .rows[0][1],
        Value::Integer(1)
    );
    for (sql, message) in [
        (
            "SELECT b.xs[1] FROM (SELECT [1] xs) a CROSS JOIN (SELECT [2] xs) b CROSS JOIN (SELECT [3] xs) b",
            "ambiguous",
        ),
        (
            "SELECT s.a.xs[1] FROM (SELECT {'a':{'xs':[1]}} s) a CROSS JOIN (SELECT {'a':{'xs':[2]}} s) b",
            "ambiguous",
        ),
        (
            "SELECT t.a.b[1],count(*) FROM (SELECT {'b':[1]} a) t",
            "GROUP BY",
        ),
        (
            "SELECT (SELECT t.a.b[1]),count(*) FROM (SELECT {'b':[1]} a) t",
            "GROUP BY",
        ),
        (
            "SELECT t.s.missing[1] FROM (SELECT {'xs':[1]} s) t",
            "missing",
        ),
        (
            "SELECT (SELECT t.xs[1] FROM (SELECT 2 t) q) FROM (SELECT [1] xs) t",
            "nested",
        ),
        ("SELECT {'xs':[42]} z,z.xs[1]", "not found"),
    ] {
        let error = c.query(sql).expect_err(sql);
        assert!(
            matches!(error, duckdb_rust::common::Error::Bind(_)),
            "{sql}: {error}"
        );
        assert!(error.to_string().contains(message), "{sql}: {error}");
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn qualified_mixed_children_cross_execution_mutations_and_native_reopen() -> Result<()> {
    use duckdb_rust::execution::expression_executor::{
        BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator,
    };
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("qualified-paths.duckdb");
        let database = DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadWrite,
                Arc::new(duckdb_rust::storage::duckdb::DuckDbFormat::default()),
            )?))
            .expressions(expressions)
            .batch_size(2)
            .build()?;
        let mut c = database.connect();
        c.execute("CREATE TABLE paths(id INTEGER PRIMARY KEY,s STRUCT(inner_value STRUCT(xs INTEGER[],d DECIMAL(12,2),ts TIMESTAMP_NS))); INSERT INTO paths VALUES(1,{'inner_value':{'xs':[1,NULL],'d':1.25,'ts':TIMESTAMP_NS '2000-01-01 00:00:00.123456789'}}),(2,{'inner_value':{'xs':[1,4],'d':2.50,'ts':TIMESTAMP_NS '2000-01-01 00:00:00.123456789'}}),(3,NULL)")?;
        assert_eq!(
            c.query("SELECT main.paths.s.inner_value.xs[1] FROM paths ORDER BY id")?
                .rows,
            vec![
                vec![Value::Integer(1)],
                vec![Value::Integer(1)],
                vec![Value::Null]
            ]
        );
        assert_eq!(c.query("SELECT count(*) FROM paths a JOIN paths b ON a.s.inner_value.xs[1]=b.s.inner_value.xs[1] AND a.s.inner_value.ts=b.s.inner_value.ts")?.rows,vec![vec![Value::Integer(4)]]);
        assert_eq!(c.query("SELECT p.s.inner_value.xs[1],count(*) FROM paths p GROUP BY p.s.inner_value.xs[1] ORDER BY 1")?.rows,vec![vec![Value::Integer(1),Value::Integer(2)],vec![Value::Null,Value::Integer(1)]]);
        assert_eq!(c.query("SELECT row_number() OVER (PARTITION BY p.s.inner_value.xs[1] ORDER BY p.s.inner_value.d) FROM paths p ORDER BY id")?.rows,vec![vec![Value::Integer(1)],vec![Value::Integer(2)],vec![Value::Integer(1)]]);
        let prepared = c.prepare("SELECT p.s.inner_value.xs[$1],p.s.inner_value.d,p.s.inner_value.ts::VARCHAR FROM paths p WHERE p.id=$2")?;
        assert_eq!(
            c.execute_prepared(&prepared, &[Value::Integer(2), Value::Integer(1)])?
                .rows,
            vec![vec![
                Value::Null,
                Value::Decimal {
                    value: 125,
                    width: 12,
                    scale: 2
                },
                Value::Varchar("2000-01-01 00:00:00.123456789".into())
            ]]
        );
        let before = c.query("SELECT * FROM paths ORDER BY id")?.rows;
        c.execute("BEGIN; UPDATE paths SET s=NULL WHERE paths.s.inner_value.xs[1]=1; DELETE FROM paths WHERE paths.s IS NULL; ROLLBACK")?;
        assert_eq!(c.query("SELECT * FROM paths ORDER BY id")?.rows, before);
        c.execute("UPDATE paths SET s={'inner_value':{'xs':[7,8],'d':3.75,'ts':TIMESTAMP_NS '2001-01-01 00:00:00.987654321'}} WHERE paths.id=1")?;
        let after = c.query("SELECT p.id,p.s.inner_value.xs[2],p.s.inner_value.d,p.s.inner_value.ts FROM paths p ORDER BY p.s.inner_value.d,p.id")?.rows;
        drop(c);
        drop(database);
        assert_eq!(Database::open_read_only(&path)?.connect().query("SELECT p.id,p.s.inner_value.xs[2],p.s.inner_value.d,p.s.inner_value.ts FROM paths p ORDER BY p.s.inner_value.d,p.id")?.rows,after);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn map_lookup_templates_nulls_nested_keys_and_union_tags() -> Result<()> {
    let mut c = Database::memory()?.connect();
    assert_eq!(c.query("SELECT map([1],[2])['1'],map([1],[2])['2'],map([TIMESTAMP '2000-01-01 00:00:00'],[3])['2000-01-01 00:00:00']")?.rows,vec![vec![Value::Integer(2),Value::Null,Value::Integer(3)]]);
    assert!(
        c.query("SELECT map([1],[2])[k] FROM (VALUES ('1'))t(k)")
            .is_err()
    );
    assert_eq!(c.query("SELECT union_tag(TIMESTAMP_NS '2000-01-01 00:00:00.123456789'::UNION(t TIMESTAMP_NS,i INTEGER))::VARCHAR,union_tag(TIMESTAMPTZ '2000-01-01 00:00:00+00'::UNION(t TIMESTAMPTZ,i INTEGER))::VARCHAR")?.rows,vec![vec![Value::Varchar("t".into()),Value::Varchar("t".into())]]);
    assert_eq!(c.query("SELECT map([1],[2])[1],map([1],[2])[1.1],map_contains(map([1],[NULL]),1),map_extract(map([1],[NULL]),1),map_extract(map([1],[2]),NULL),map_extract(NULL::MAP(INTEGER,INTEGER),1),map_contains(map([1],[2]),NULL),cardinality(map([1],[2]))")?.rows,
        vec![vec![Value::Integer(2),Value::Null,Value::Boolean(true),
            NestedValue::value(NestedType::List(DataType::Null).data_type(),NestedPayload::Sequence(vec![Value::Null]))?,
            NestedValue::value(NestedType::List(DataType::Integer).data_type(),NestedPayload::Sequence(vec![]))?,
            Value::Null,Value::Null,Value::Unsigned(1)]]);
    assert_eq!(c.query("SELECT map([[1,NULL]],['found'])[[1,NULL]],map([[1,NULL]],['found'])[[1,2]],map_keys(map([1,2],[3,4]))[2],map_values(map([1,2],[3,4]))[1],map_entries(map([1],[2]))[1].value")?.rows,
        vec![vec![Value::Varchar("found".into()),Value::Null,Value::Integer(2),Value::Integer(3),Value::Integer(2)]]);
    let result=c.query("SELECT union_tag(union_value(i:=NULL)::UNION(s VARCHAR,i INTEGER)), union_tag(NULL::UNION(i INTEGER,s VARCHAR)),typeof({'a':1,'A A':2,'select':3,'Foo':4}),typeof({'x':NULL}),typeof([])")?;
    let tags = DataType::enumeration(vec!["s".into(), "i".into()])?;
    assert_eq!(result.columns[0].data_type, tags);
    assert_eq!(
        result.rows,
        vec![vec![
            Value::enumeration(&tags, 1)?,
            Value::Null,
            Value::Varchar(
                "STRUCT(a INTEGER, \"A A\" INTEGER, \"select\" INTEGER, Foo INTEGER)".into()
            ),
            Value::Varchar("STRUCT(x \"NULL\")".into()),
            Value::Varchar("\"NULL\"[]".into())
        ]]
    );
    for sql in [
        "SELECT map(['1'],[2])[1]",
        "SELECT map([1],[2])[DATE '2000-01-01']",
        "SELECT union_tag(1)",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn declared_map_children_flow_through_parameters_mutations_joins_and_native_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("mixed-map.duckdb");
    let mut c = Database::open(&path)?.connect();
    c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,m MAP(INTEGER,STRUCT(d DECIMAL(12,2),ts TIMESTAMP,xs INTEGER[])),u UNION(i INTEGER,s VARCHAR)); INSERT INTO t VALUES(1,map([1],[{'d':1.25,'ts':TIMESTAMP '2000-01-01 01:02:03','xs':[1,NULL]}]),union_value(i:=NULL)),(2,map([2],[{'d':2.50,'ts':TIMESTAMP '2001-01-01 00:00:00','xs':[]}]),union_value(s:='two')),(3,NULL,NULL)")?;
    assert_eq!(
        c.query("SELECT m[1].d,m[1].xs[2],union_tag(u)::VARCHAR FROM t WHERE id=1")?
            .rows,
        vec![vec![
            Value::Decimal {
                value: 125,
                width: 12,
                scale: 2
            },
            Value::Null,
            Value::Varchar("i".into())
        ]]
    );
    let prepared = c.prepare("SELECT m[$1].d FROM t WHERE id=1")?;
    assert_eq!(
        c.execute_prepared(&prepared, &[Value::Integer(1)])?.rows,
        vec![vec![Value::Decimal {
            value: 125,
            width: 12,
            scale: 2
        }]]
    );
    assert_eq!(
        c.query("SELECT count(*) FROM t a JOIN t b ON a.m=b.m")?
            .rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        c.query("SELECT count(DISTINCT m) FROM t")?.rows,
        vec![vec![Value::Integer(2)]]
    );
    assert_eq!(
        c.query("SELECT row_number() OVER (PARTITION BY m ORDER BY id) FROM t ORDER BY id")?
            .rows,
        vec![vec![Value::Integer(1)]; 3]
    );
    let before = c.query("SELECT * FROM t ORDER BY id")?.rows;
    c.execute("BEGIN; UPDATE t SET m=map([1],[{'d':9.75,'ts':TIMESTAMP '2002-02-02 02:02:02','xs':[4,5]}]) WHERE id=1; DELETE FROM t WHERE id=2; ROLLBACK")?;
    assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, before);
    c.execute("UPDATE t SET m=map([1],[{'d':3.75,'ts':TIMESTAMP '2002-02-02 02:02:02','xs':[4,5]}]) WHERE id=1")?;
    let after = c.query("SELECT * FROM t ORDER BY id")?.rows;
    drop(c);
    assert_eq!(
        Database::open_read_only(&path)?
            .connect()
            .query("SELECT * FROM t ORDER BY id")?
            .rows,
        after
    );
    Ok(())
}
