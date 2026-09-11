use super::*;
use duckdb_rust::{
    DatabaseBuilder,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::{FunctionEffects, FunctionRegistry, ScalarFunction},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
};

#[derive(Debug)]
struct Occurrence(Arc<std::sync::atomic::AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Occurrence {
    fn name(&self) -> &str {
        "nullif_occurrence"
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: true,
            external_access: false,
        }
    }
    fn return_type(
        &self,
        _: &[DataType],
        _: &duckdb_rust::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        Ok(DataType::BigInt)
    }
    fn evaluate(&self, _: &[Value], q: &QueryContext) -> Result<Value> {
        q.check()?;
        Ok(Value::Integer(
            (self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1) as i128,
        ))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn nullif_retains_first_type_comparison_casts_parameters_and_effectful_occurrences() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mut functions = FunctionRegistry::builtins();
            functions.register_scalar(Arc::new(Occurrence(count.clone())))?;
            let mut c = DatabaseBuilder::new()
                .functions(functions)
                .expressions(evaluator.clone())
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            for (expression, kind, value) in [
                (
                    "nullif(1::UHUGEINT,2::INTEGER)",
                    "UHUGEINT",
                    Value::Unsigned(1),
                ),
                ("nullif(1::UHUGEINT,1::INTEGER)", "UHUGEINT", Value::Null),
                (
                    "nullif(1::UTINYINT,2::BIGINT)",
                    "UTINYINT",
                    Value::Unsigned(1),
                ),
                (
                    "nullif(1.25::DECIMAL(4,2),2::INTEGER)",
                    "DECIMAL(4,2)",
                    decimal(125, 4, 2)?,
                ),
                ("nullif('2',2)", "VARCHAR", Value::Null),
                ("nullif('3',2)", "VARCHAR", Value::Varchar("3".into())),
                ("nullif(NULL::INTEGER,2)", "INTEGER", Value::Null),
                ("nullif(2,NULL::INTEGER)", "INTEGER", Value::Integer(2)),
            ] {
                assert_eq!(
                    c.query(&format!("SELECT typeof({expression}),{expression}"))?
                        .rows,
                    vec![vec![Value::Varchar(kind.into()), value]],
                    "{expression}"
                );
            }
            assert_eq!(c.query("SELECT typeof(nullif([1::UHUGEINT],[2::INTEGER])),nullif([1::UHUGEINT],[2::INTEGER])::VARCHAR,typeof(nullif({'d':1.25},{'d':2.5})),nullif({'d':1.25},{'d':2.5})::VARCHAR")?.rows,vec![vec![Value::Varchar("UHUGEINT[]".into()),Value::Varchar("[1]".into()),Value::Varchar("STRUCT(d DECIMAL(3,2))".into()),Value::Varchar("{'d': 1.25}".into())]]);
            assert!(matches!(
                c.query("SELECT nullif(340282366920938463463374607431768211455,1)"),
                Err(Error::Conversion(_))
            ));
            let p = c.prepare("SELECT typeof(nullif($1,2::UHUGEINT)),nullif($1,2::UHUGEINT)")?;
            assert_eq!(
                c.execute_prepared(&p, &[Value::Unsigned(u128::MAX)])?.rows,
                vec![vec![
                    Value::Varchar("UHUGEINT".into()),
                    Value::Unsigned(u128::MAX)
                ]]
            );
            assert_eq!(
                c.query("SELECT nullif(nullif_occurrence(),99)")?.rows,
                vec![vec![Value::Integer(2)]]
            );
            assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
            assert_eq!(
                c.query("SELECT nullif(nullif_occurrence(),3)")?.rows,
                vec![vec![Value::Null]]
            );
            assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 3);
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn nullif_values_cross_joins_windows_atomic_mutations_and_native_reopen() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("nullif-native.duckdb");
    {
        let mut c = Database::open(&path)?.connect();
        c.execute("CREATE TABLE t(k INTEGER PRIMARY KEY,u UHUGEINT,s INTEGER,n UHUGEINT[]); INSERT INTO t VALUES(1,1,1,[1]),(2,2,9,[2]),(3,NULL,NULL,NULL)")?;
        assert_eq!(
            c.query("SELECT nullif(u,s),count(*) FROM t GROUP BY nullif(u,s) ORDER BY 1")?
                .rows,
            vec![
                vec![Value::Unsigned(2), Value::Integer(1)],
                vec![Value::Null, Value::Integer(2)]
            ]
        );
        assert_eq!(
            c.query("SELECT a.k FROM t a JOIN t b ON nullif(a.u,a.s)=b.u ORDER BY a.k")?
                .rows,
            vec![vec![Value::Integer(2)]]
        );
        assert_eq!(c.query("SELECT sum(nullif(u,s)) OVER(ORDER BY k ROWS UNBOUNDED PRECEDING) FROM t ORDER BY k")?.rows,vec![vec![Value::Null],vec![Value::Double(2.0)],vec![Value::Double(2.0)]]);
        assert!(matches!(
            c.execute("UPDATE t SET s=nullif(340282366920938463463374607431768211455,s)"),
            Err(Error::Conversion(_))
        ));
        assert_eq!(
            c.query("SELECT s FROM t WHERE k=1")?.rows,
            vec![vec![Value::Integer(1)]]
        );
        c.execute("BEGIN; DELETE FROM t; ROLLBACK")?;
    }
    let mut c = Database::open(&path)?.connect();
    let p = c.prepare(
        "SELECT typeof(nullif(u,s)),nullif(u,s),nullif(n,[9::INTEGER])::VARCHAR FROM t WHERE k=$1",
    )?;
    assert_eq!(
        c.execute_prepared(&p, &[Value::Integer(2)])?.rows,
        vec![vec![
            Value::Varchar("UHUGEINT".into()),
            Value::Unsigned(2),
            Value::Varchar("[2]".into())
        ]]
    );
    c.execute("CHECKPOINT")?;
    drop(c);
    let mut c = Database::open(&path)?.connect();
    assert_eq!(
        c.query("SELECT nullif(u,s) FROM t WHERE k=1")?.rows,
        vec![vec![Value::Null]]
    );
    Ok(())
}
