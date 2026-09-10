use super::*;
use duckdb_rust::{
    common::{
        cast::{CastFunction, CastMode, CastRegistry, CastSpec, PrimitiveCast},
        type_registry::{KeyWriter, PrimitiveTypes, TypeAdapter, TypeRegistry},
    },
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::{FunctionRegistry, ScalarBindArguments, ScalarFunction},
};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn map_literals_and_sequence_templates_preserve_order_nulls_and_combination_context() -> Result<()>
{
    let mut c = Database::memory()?.connect();
    for (sql, expected) in [
        ("SELECT (MAP {'x':1,'y':NULL})::VARCHAR", "{x=1, y=NULL}"),
        ("SELECT (MAP {1:'a','2':'b'})::VARCHAR", "{1=a, 2=b}"),
        ("SELECT (MAP {true:1,2:3})::VARCHAR", "{1=1, 2=3}"),
        (
            "SELECT (MAP {['x',NULL]:{'n':[1,NULL]},['y']:NULL})::VARCHAR",
            "{[x, NULL]={'n': [1, NULL]}, [y]=NULL}",
        ),
        ("SELECT typeof(MAP {})", "MAP(\"NULL\", \"NULL\")"),
        ("SELECT [true,1,NULL]::VARCHAR", "[1, 1, NULL]"),
        ("SELECT [1,false]::VARCHAR", "[1, 0]"),
        ("SELECT ['1',NULL,'2',3]::VARCHAR", "[1, NULL, 2, 3]"),
        ("SELECT ['1','2',3]::VARCHAR", "[1, 2, 3]"),
        ("SELECT [3,'1','2']::VARCHAR", "[3, 1, 2]"),
        ("SELECT ['1',NULL]::VARCHAR", "[1, NULL]"),
        ("SELECT [NULL,'1']::VARCHAR", "[NULL, 1]"),
        ("SELECT typeof([NULL,'1'])", "VARCHAR[]"),
        ("SELECT typeof([NULL::VARCHAR,'1'])", "VARCHAR[]"),
        ("SELECT typeof([1::TINYINT,2::UTINYINT])", "SMALLINT[]"),
        (
            "SELECT typeof([1::INTEGER,1.25::DECIMAL(5,2)])",
            "DECIMAL(12,2)[]",
        ),
        (
            "SELECT typeof([{'a':NULL::DECIMAL(5,2)},{'b':1::TINYINT}])",
            "STRUCT(a DECIMAL(5,2), b TINYINT)[]",
        ),
        (
            "SELECT typeof(['red'::ENUM('red','blue'),'green'])",
            "VARCHAR[]",
        ),
    ] {
        assert_eq!(
            c.query(sql)?.rows,
            vec![vec![Value::Varchar(expected.into())]],
            "{sql}"
        );
    }
    for sql in [
        "SELECT [NULL,'1',2]",
        "SELECT [NULL::VARCHAR,'1',2]",
        "SELECT ['1'::VARCHAR,2]",
        "SELECT [s,2] FROM (VALUES ('1')) t(s)",
        "SELECT [true,1.25::DECIMAL(5,2)]",
        "SELECT [true,1.0::DOUBLE]",
        "SELECT concat([true],[1.0::DOUBLE])",
        "SELECT MAP {NULL:1}",
        "SELECT MAP {'x':1,'x':2}",
        "SELECT MAP {1:1,'1':2}",
        "SELECT MAP {true:1,1:2}",
        "SELECT MAP {1:1,'bad':2}",
    ] {
        assert!(c.query(sql).is_err(), "{sql}");
    }
    let parameter = c.prepare("SELECT [$1,1]")?;
    assert!(
        c.execute_prepared(&parameter, &[Value::Varchar("2".into())])
            .is_err()
    );
    for sql in [
        "SELECT MAP {NULL:1}",
        "SELECT MAP {'x':1,'x':2}",
        "SELECT MAP {1:1,'1':2}",
        "SELECT MAP {true:1,1:2}",
        "SELECT map([1,2],[3])",
    ] {
        assert!(
            matches!(c.query(sql), Err(duckdb_rust::Error::InvalidInput(_))),
            "{sql}"
        );
    }
    assert_eq!(c.query("SELECT TRY_CAST(map(['01','1'],['a','b']) AS MAP(INTEGER,VARCHAR)) IS NULL,TRY_CAST(map(['x'],['1']) AS MAP(INTEGER,INTEGER)) IS NULL,(TRY_CAST(map(['1'],['x']) AS MAP(INTEGER,INTEGER)))::VARCHAR")?.rows,vec![vec![Value::Boolean(true),Value::Boolean(true),Value::Varchar("{1=NULL}".into())]]);
    Ok(())
}

#[derive(Debug)]
struct MapKeys(bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for MapKeys {
    fn name(&self) -> &'static str {
        "selected-map-constructor-keys"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(ty)
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(left, right)
    }
    fn validate_value(&self, ty: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        PrimitiveTypes.validate_value(ty, value, query)
    }
    fn compare(
        &self,
        _: &DataType,
        _: &Value,
        _: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        query.check()?;
        Ok(Ordering::Equal)
    }
    fn write_key(
        &self,
        _: &DataType,
        _: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        query.check()?;
        if self.0 {
            return Err(duckdb_rust::Error::Resource("selected key failure".into()));
        }
        output.push(0)
    }
}

struct MapArguments(DataType);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarBindArguments for MapArguments {
    fn len(&self) -> usize {
        2
    }
    fn data_type(&self, index: usize) -> Result<DataType> {
        assert!(index < 2);
        Ok(self.0.clone())
    }
    fn constant(&self, _: usize) -> Result<Value> {
        panic!("MAP must not request constants")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn map_constructor_retains_selected_key_semantics_without_reclassifying_failures() -> Result<()> {
    let list = NestedType::List(DataType::Integer).data_type();
    let values = NestedValue::value(
        list.clone(),
        NestedPayload::Sequence(vec![Value::Integer(1), Value::Integer(2)]),
    )?;
    let ambient = QueryContext::background().with_types(Arc::new(TypeRegistry::default()));
    for failure in [false, true] {
        let mut types = TypeRegistry::builtins();
        types.replace("builtin.integer", Arc::new(MapKeys(failure)))?;
        let query = QueryContext::background().with_types(Arc::new(types));
        let bound = FunctionRegistry::builtins()
            .scalar("map")?
            .bind(&MapArguments(list.clone()), &query)?
            .expect("bound MAP");
        let result = bound.evaluate(&[values.clone(), values.clone()], &ambient);
        if failure {
            assert!(
                matches!(result,Err(duckdb_rust::Error::Resource(message)) if message=="selected key failure")
            );
        } else {
            assert!(matches!(result, Err(duckdb_rust::Error::InvalidInput(_))));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct SelectedMap;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for SelectedMap {
    fn name(&self) -> &str {
        "map"
    }
    fn return_type(&self, args: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        assert_eq!(args.len(), 2);
        assert!(args.iter().all(|ty| matches!(ty,DataType::Nested(metadata) if matches!(metadata.as_ref(),NestedType::List(_)))));
        Ok(DataType::Integer)
    }
    fn evaluate(&self, _: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        Ok(Value::Integer(42))
    }
}

#[derive(Debug)]
struct SelectedBooleanCast(Arc<AtomicUsize>, CastMode);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedBooleanCast {
    fn name(&self) -> &'static str {
        "selected-sequence-boolean-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Boolean && spec.target == DataType::Integer && spec.mode == self.1
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        self.0.fetch_add(1, AtomicOrdering::Relaxed);
        assert_eq!(spec.mode, self.1);
        PrimitiveCast.cast(
            value,
            &CastSpec {
                mode: CastMode::Explicit,
                ..spec.clone()
            },
            query,
        )
    }
}

#[derive(Debug)]
struct SelectedCombination(Arc<AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for SelectedCombination {
    fn name(&self) -> &'static str {
        "selected-sequence-common-type"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(ty)
    }
    fn validate_value(&self, ty: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        PrimitiveTypes.validate_value(ty, value, query)
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        if matches!(
            (left, right),
            (DataType::Integer, DataType::Varchar) | (DataType::Varchar, DataType::Integer)
        ) {
            self.0.fetch_add(1, AtomicOrdering::Relaxed);
            return Ok(Some(DataType::BigInt));
        }
        if matches!(
            (left, right),
            (DataType::Integer, DataType::BigInt) | (DataType::BigInt, DataType::Integer)
        ) {
            self.0.fetch_add(1, AtomicOrdering::Relaxed);
            return Ok(Some(DataType::SmallInt));
        }
        PrimitiveTypes.common_type(left, right)
    }
    fn compare(
        &self,
        ty: &DataType,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        PrimitiveTypes.compare(ty, left, right, query)
    }
    fn write_key(
        &self,
        ty: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.write_key(ty, value, output, query)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn map_literal_lowering_and_sequence_coercion_use_selected_functions_types_and_casts() -> Result<()>
{
    let mut functions = FunctionRegistry::default();
    functions.register_scalar(Arc::new(SelectedMap))?;
    let mut c = DatabaseBuilder::new()
        .functions(functions)
        .build()?
        .connect();
    assert_eq!(
        c.query("SELECT MAP {'x':1},MAP {},map(['x'],[1])")?.rows,
        vec![vec![Value::Integer(42); 3]]
    );
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for mode in [CastMode::Implicit, CastMode::Explicit] {
            let calls = Arc::new(AtomicUsize::new(0));
            let mut casts = CastRegistry::builtins();
            let spec = CastSpec {
                source: DataType::Boolean,
                target: DataType::Integer,
                mode,
            };
            let adapter = Arc::new(SelectedBooleanCast(calls.clone(), mode));
            if mode == CastMode::Implicit {
                casts.register(spec, adapter)?;
            } else {
                casts.replace(spec, adapter)?;
            }
            let mut c = DatabaseBuilder::new()
                .casts(casts)
                .expressions(evaluator.clone())
                .build()?
                .connect();
            let parameter = c.prepare("SELECT [$1,2]::VARCHAR,(MAP {$1:2,3:4})::VARCHAR")?;
            assert_eq!(
                c.execute_prepared(&parameter, &[Value::Boolean(true)])?
                    .rows,
                vec![vec![
                    Value::Varchar("[1, 2]".into()),
                    Value::Varchar("{1=2, 3=4}".into())
                ]]
            );
            assert!(calls.load(AtomicOrdering::Relaxed) > 0);
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let mut types = TypeRegistry::builtins();
    types.replace(
        "builtin.integer",
        Arc::new(SelectedCombination(calls.clone())),
    )?;
    types.replace(
        "builtin.bigint",
        Arc::new(SelectedCombination(calls.clone())),
    )?;
    let mut c = DatabaseBuilder::new()
        .types(Arc::new(types))
        .build()?
        .connect();
    assert_eq!(c.query("SELECT typeof([1::INTEGER,2::BIGINT]),typeof([2::BIGINT,1::INTEGER]),typeof(MAP {1::INTEGER:'a',2::BIGINT:'b'})")?.rows,vec![vec![Value::Varchar("SMALLINT[]".into()),Value::Varchar("SMALLINT[]".into()),Value::Varchar("MAP(SMALLINT, VARCHAR)".into())]]);
    assert!(calls.load(AtomicOrdering::Relaxed) >= 3);
    assert_eq!(
        c.query("SELECT typeof([1::INTEGER,'2']),typeof(['2',1::INTEGER])")?
            .rows,
        vec![vec![Value::Varchar("BIGINT[]".into()); 2]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn map_literal_mixed_children_cross_parameters_relations_mutations_rollback_and_native_reopen()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("map-literals.duckdb");
    let mut c = Database::open(&path)?.connect();
    c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,m MAP(INTEGER,STRUCT(d DECIMAL(12,2),ts TIMESTAMP_NS,items INTEGER[]))); INSERT INTO t VALUES(1,MAP {1:{'d':1.25,'ts':TIMESTAMP_NS '2000-01-01 00:00:00.123456789','items':[true,2]},2:NULL}),(2,MAP {})")?;
    let prepared=c.prepare("INSERT INTO t VALUES($1,MAP {$2:{'d':$3::DECIMAL(12,2),'ts':$4::TIMESTAMP_NS,'items':[$5,2]},2:NULL})")?;
    c.execute_prepared(
        &prepared,
        &[
            Value::Integer(3),
            Value::Integer(1),
            Value::Varchar("1.25".into()),
            Value::Varchar("2000-01-01 00:00:00.123456789".into()),
            Value::Boolean(true),
        ],
    )?;
    assert_eq!(
        c.query("SELECT count(*) FROM t a JOIN t b ON a.m=b.m")?
            .rows,
        vec![vec![Value::Integer(5)]]
    );
    assert_eq!(
        c.query("SELECT count(*) OVER(PARTITION BY m) FROM t ORDER BY id")?
            .rows,
        [2, 1, 2]
            .into_iter()
            .map(|n| vec![Value::Integer(n)])
            .collect::<Vec<_>>()
    );
    let before = c.query("SELECT * FROM t ORDER BY id")?.rows;
    c.execute("BEGIN; UPDATE t SET m=MAP {}; DELETE FROM t WHERE id=1; ROLLBACK")?;
    assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, before);
    for sql in [
        "UPDATE t SET m=MAP {1:NULL,'1':NULL}",
        "UPDATE t SET m=MAP {NULL:NULL}",
        "INSERT INTO t VALUES(4,MAP {1:NULL,1:NULL})",
    ] {
        assert!(c.execute(sql).is_err());
        assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, before);
    }
    c.execute("UPDATE t SET id=id+10 WHERE id=2")?;
    let expected = c.query("SELECT * FROM t ORDER BY id")?.rows;
    drop(c);
    let mut c = Database::open(&path)?.connect();
    assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, expected);
    assert_eq!(
        c.query("SELECT m[1].d::VARCHAR,m[1].ts::VARCHAR,m[1].items::VARCHAR FROM t WHERE id=1")?
            .rows,
        vec![vec![
            Value::Varchar("1.25".into()),
            Value::Varchar("2000-01-01 00:00:00.123456789".into()),
            Value::Varchar("[1, 2]".into())
        ]]
    );
    assert_eq!(
        c.query("SELECT count(*) FROM t a JOIN t b ON a.m=b.m")?
            .rows,
        vec![vec![Value::Integer(5)]]
    );
    Ok(())
}
