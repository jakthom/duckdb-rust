use super::*;
use duckdb_rust::{
    DatabaseBuilder,
    common::cast::{CastFunction, CastSpec},
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn coalesce_combines_in_source_order_and_preserves_lazy_full_width_values() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut c = DatabaseBuilder::new()
                .expressions(evaluator.clone())
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            for (expression, kind, value) in [
                (
                    "coalesce(1::UHUGEINT,1::INTEGER)",
                    "BIGINT",
                    Value::Integer(1),
                ),
                (
                    "coalesce(NULL,1::UHUGEINT,1::INTEGER)",
                    "BIGINT",
                    Value::Integer(1),
                ),
                (
                    "coalesce(1,1::UHUGEINT,NULL)",
                    "UHUGEINT",
                    Value::Unsigned(1),
                ),
                ("coalesce(1,NULL,1::UHUGEINT)", "BIGINT", Value::Integer(1)),
                ("coalesce(NULL,1,1::UHUGEINT)", "BIGINT", Value::Integer(1)),
                (
                    "coalesce(340282366920938463463374607431768211455,NULL,1)",
                    "UHUGEINT",
                    Value::Unsigned(u128::MAX),
                ),
                (
                    "coalesce(1::UHUGEINT,'bad'::INTEGER)",
                    "BIGINT",
                    Value::Integer(1),
                ),
                ("coalesce('2',1::INTEGER)", "INTEGER", Value::Integer(2)),
                (
                    "coalesce(NULL,TRUE,1::UTINYINT)",
                    "UTINYINT",
                    Value::Unsigned(1),
                ),
                ("coalesce(NULL,NULL)", "\"NULL\"", Value::Null),
            ] {
                let result = c.query(&format!("SELECT typeof({expression}),{expression}"))?;
                assert_eq!(
                    result.rows,
                    vec![vec![Value::Varchar(kind.into()), value]],
                    "{expression}"
                );
            }
            assert_eq!(c.query("SELECT typeof(coalesce([1::UHUGEINT],[1::INTEGER])),coalesce([1::UHUGEINT],[1::INTEGER])::VARCHAR,coalesce(NULL,{'d':1.25})::VARCHAR,coalesce(NULL,TIMESTAMP '2024-01-02 03:04:05')::VARCHAR")?.rows,
                vec![vec![Value::Varchar("BIGINT[]".into()),Value::Varchar("[1]".into()),Value::Varchar("{'d': 1.25}".into()),Value::Varchar("2024-01-02 03:04:05".into())]]);
            for expression in [
                "coalesce(340282366920938463463374607431768211455,1)",
                "coalesce('bad',1::INTEGER)",
                "TRY_CAST(coalesce(340282366920938463463374607431768211455,1) AS BIGINT)",
            ] {
                assert!(
                    matches!(
                        c.query(&format!("SELECT {expression}")),
                        Err(Error::Conversion(_))
                    ),
                    "{expression}"
                );
            }
            assert!(matches!(
                c.query("SELECT coalesce('1'::VARCHAR,1::INTEGER)"),
                Err(Error::Bind(_))
            ));
            let p = c.prepare("SELECT typeof(coalesce($1,1)),coalesce($1,1)")?;
            assert_eq!(
                c.execute_prepared(&p, &[Value::Unsigned(u128::MAX)])?.rows,
                vec![vec![
                    Value::Varchar("UHUGEINT".into()),
                    Value::Unsigned(u128::MAX)
                ]]
            );
            assert_eq!(
                c.execute_prepared(&p, &[Value::Null])?.rows,
                vec![vec![Value::Varchar("INTEGER".into()), Value::Integer(1)]]
            );
            assert!(matches!(c.query("SELECT coalesce(a,b,c) FROM (VALUES (1::UHUGEINT,'bad'::VARCHAR,2::INTEGER),(NULL,'3',4),(NULL,NULL,5))t(a,b,c)"),Err(Error::Bind(_))));
            assert_eq!(c.query("SELECT coalesce(a,b::INTEGER,c) FROM (VALUES (1::UHUGEINT,'bad'::VARCHAR,2::INTEGER),(NULL,'3',4),(NULL,NULL,5))t(a,b,c)")?.rows,vec![vec![Value::Integer(1)],vec![Value::Integer(3)],vec![Value::Integer(5)]]);
        }
    }
    Ok(())
}

#[derive(Debug)]
struct SelectedCast {
    modes: Arc<std::sync::Mutex<Vec<CastMode>>>,
    fatal: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedCast {
    fn name(&self) -> &'static str {
        "coalesce-selected-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Integer && spec.target == DataType::BigInt
    }
    fn cast(&self, _: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        self.modes.lock().unwrap().push(spec.mode);
        if self.fatal {
            return Err(Error::Resource("selected coalesce cast".into()));
        }
        Ok(Value::Integer(if spec.mode == CastMode::Implicit {
            11
        } else {
            22
        }))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn coalesce_retains_exact_selected_modes_and_fatal_failures() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for fatal in [false, true] {
            let modes = Arc::new(std::sync::Mutex::new(Vec::new()));
            let mut casts = CastRegistry::builtins();
            for mode in [CastMode::Implicit, CastMode::Explicit] {
                casts.replace(
                    CastSpec {
                        source: DataType::Integer,
                        target: DataType::BigInt,
                        mode,
                    },
                    Arc::new(SelectedCast {
                        modes: modes.clone(),
                        fatal,
                    }),
                )?;
            }
            let mut c = DatabaseBuilder::new()
                .casts(casts)
                .expressions(evaluator.clone())
                .optimizer(Arc::new(IdentityOptimizer))
                .build()?
                .connect();
            assert_eq!(
                c.query("SELECT coalesce(2147483648,1)")?.rows,
                vec![vec![Value::Integer(2147483648)]]
            );
            assert!(modes.lock().unwrap().is_empty());
            let result = c.query("SELECT TRY_CAST(coalesce(1,2147483648) AS BIGINT)");
            if fatal {
                assert!(matches!(result, Err(Error::Resource(_))));
            } else {
                assert_eq!(result?.rows, vec![vec![Value::Integer(11)]]);
            }
            assert_eq!(*modes.lock().unwrap(), vec![CastMode::Implicit]);
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn coalesce_inferred_values_cross_relations_atomic_mutations_and_native_reopen() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("coalesce-native.duckdb");
    {
        let mut c = Database::open(&path)?.connect();
        c.execute("CREATE TABLE t(k BIGINT PRIMARY KEY,u UHUGEINT,s INTEGER,n BIGINT[]); INSERT INTO t VALUES(1,1,9,[1]),(2,NULL,2,[2]),(3,NULL,NULL,NULL)")?;
        assert_eq!(
            c.query("SELECT coalesce(u,s),count(*) FROM t GROUP BY coalesce(u,s) ORDER BY 1")?
                .rows,
            vec![
                vec![Value::Integer(1), Value::Integer(1)],
                vec![Value::Integer(2), Value::Integer(1)],
                vec![Value::Null, Value::Integer(1)]
            ]
        );
        assert_eq!(
            c.query("SELECT a.k FROM t a JOIN t b ON coalesce(a.u,a.s)=b.k ORDER BY a.k")?
                .rows,
            vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
        );
        assert_eq!(c.query("SELECT sum(coalesce(u,s,0)) OVER(ORDER BY k ROWS UNBOUNDED PRECEDING) FROM t ORDER BY k")?.rows,vec![vec![Value::Integer(1)],vec![Value::Integer(3)],vec![Value::Integer(3)]]);
        let p = c.prepare("UPDATE t SET u=$1 WHERE k=1")?;
        c.execute_prepared(&p, &[Value::Unsigned(u128::MAX)])?;
        assert!(matches!(
            c.execute("UPDATE t SET s=coalesce(u,s)"),
            Err(Error::Conversion(_))
        ));
        assert_eq!(
            c.query("SELECT s FROM t WHERE k=1")?.rows,
            vec![vec![Value::Integer(9)]]
        );
        c.execute("BEGIN; DELETE FROM t; ROLLBACK")?;
    }
    let mut c = Database::open(&path)?.connect();
    let p=c.prepare("SELECT coalesce($1,u)::VARCHAR,coalesce(n,[4::UHUGEINT,5::INTEGER])::VARCHAR FROM t WHERE k=$2")?;
    assert_eq!(
        c.execute_prepared(&p, &[Value::Null, Value::Integer(1)])?
            .rows,
        vec![vec![
            Value::Varchar(u128::MAX.to_string()),
            Value::Varchar("[1]".into())
        ]]
    );
    c.execute("CHECKPOINT")?;
    drop(c);
    let mut c = Database::open(&path)?.connect();
    assert_eq!(
        c.query("SELECT coalesce(n,[4::UHUGEINT,5::INTEGER])::VARCHAR FROM t WHERE k=3")?
            .rows,
        vec![vec![Value::Varchar("[4, 5]".into())]]
    );
    Ok(())
}
