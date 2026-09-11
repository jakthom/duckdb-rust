use super::*;
use duckdb_rust::{
    DatabaseBuilder,
    common::cast::{CastFunction, CastSpec},
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    storage::{
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn wide_case_and_list_inference_preserve_literal_identity_and_arithmetic_ranking() -> Result<()> {
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
            for (input, target) in [
                ("TINYINT", "SMALLINT"),
                ("SMALLINT", "INTEGER"),
                ("INTEGER", "BIGINT"),
                ("BIGINT", "HUGEINT"),
                ("HUGEINT", "DOUBLE"),
            ] {
                assert_eq!(c.query(&format!("SELECT typeof(CASE WHEN false THEN 1::UHUGEINT ELSE 1::{input} END),typeof([1::UHUGEINT,1::{input}]),typeof(1::UHUGEINT+1::{input}),typeof(1::{input}+1::UHUGEINT)"))?.rows,
                    vec![vec![Value::Varchar(target.into()),Value::Varchar(format!("{target}[]")),Value::Varchar("DOUBLE".into()),Value::Varchar("DOUBLE".into())]]);
            }
            for (expression, target) in [
                (
                    "CASE WHEN false THEN 340282366920938463463374607431768211455 ELSE 1 END",
                    "BIGINT",
                ),
                (
                    "CASE WHEN false THEN 340282366920938463463374607431768211455::UHUGEINT ELSE 1::INTEGER END",
                    "BIGINT",
                ),
                ("[340282366920938463463374607431768211455,1]", "BIGINT[]"),
                ("CASE WHEN false THEN 1::UHUGEINT ELSE 1 END", "UHUGEINT"),
                (
                    "[340282366920938463463374607431768211455,340282366920938463463374607431768211455]",
                    "UHUGEINT[]",
                ),
                (
                    "[340282366920938463463374607431768211455,NULL,1]",
                    "BIGINT[]",
                ),
                (
                    "[340282366920938463463374607431768211455,1::UHUGEINT]",
                    "UHUGEINT[]",
                ),
                (
                    "[340282366920938463463374607431768211455,NULL]",
                    "UHUGEINT[]",
                ),
                (
                    "MAP {340282366920938463463374607431768211455:1,1:2}",
                    "MAP(BIGINT, INTEGER)",
                ),
            ] {
                assert_eq!(
                    c.query(&format!("SELECT typeof({expression})"))?.rows,
                    vec![vec![Value::Varchar(target.into())]],
                    "{expression}"
                );
            }
            assert_eq!(
                c.query(
                    "SELECT CASE WHEN false THEN 340282366920938463463374607431768211455 ELSE 1 END"
                )?
                .rows,
                vec![vec![Value::Integer(1)]]
            );
            for expression in [
                "CASE WHEN true THEN 340282366920938463463374607431768211455 ELSE 1 END",
                "[340282366920938463463374607431768211455,1]",
                "MAP {340282366920938463463374607431768211455:1,1:2}",
            ] {
                assert!(
                    matches!(
                        c.query(&format!("SELECT {expression}")),
                        Err(Error::Conversion(_))
                    ),
                    "{expression}"
                );
            }
            assert_eq!(
                c.query("SELECT CASE WHEN false THEN CAST('bad' AS UHUGEINT) ELSE 1::INTEGER END")?
                    .rows,
                vec![vec![Value::Integer(1)]]
            );
            assert!(matches!(c.query("SELECT TRY_CAST(CASE WHEN true THEN 340282366920938463463374607431768211455 ELSE 1 END AS BIGINT)"),Err(Error::Conversion(_))));
            let p=c.prepare("SELECT typeof(CASE WHEN false THEN $1 ELSE 1 END),typeof([$1,1]),typeof(CASE WHEN false THEN $1 ELSE 1::INTEGER END)")?;
            assert_eq!(
                c.execute_prepared(&p, &[Value::Unsigned(u128::MAX)])?.rows,
                vec![vec![
                    Value::Varchar("UHUGEINT".into()),
                    Value::Varchar("UHUGEINT[]".into()),
                    Value::Varchar("BIGINT".into())
                ]]
            );
            assert!(matches!(
                c.query("SELECT xor(1::UHUGEINT,1::INTEGER)"),
                Err(Error::Bind(_))
            ));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct SelectedWideCast(bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedWideCast {
    fn name(&self) -> &'static str {
        "selected-wide-combination-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::UHugeInt && spec.target == DataType::BigInt
    }
    fn cast(&self, _: &Value, _: &CastSpec, q: &QueryContext) -> Result<Value> {
        q.check()?;
        if self.0 {
            Err(Error::Resource("selected wide cast failure".into()))
        } else {
            Ok(Value::Integer(42))
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn wide_combination_retains_selected_casts_lazy_errors_and_resource_failures() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for fatal in [false, true] {
            let mut casts = CastRegistry::builtins();
            casts.replace(
                CastSpec {
                    source: DataType::UHugeInt,
                    target: DataType::BigInt,
                    mode: CastMode::Explicit,
                },
                Arc::new(SelectedWideCast(fatal)),
            )?;
            let mut c = DatabaseBuilder::new()
                .expressions(evaluator.clone())
                .casts(casts)
                .build()?
                .connect();
            assert_eq!(
                c.query(
                    "SELECT CASE WHEN false THEN 340282366920938463463374607431768211455 ELSE 1 END"
                )?
                .rows,
                vec![vec![Value::Integer(1)]]
            );
            let output=c.query("SELECT TRY_CAST(CASE WHEN true THEN 340282366920938463463374607431768211455 ELSE 1 END AS BIGINT)");
            if fatal {
                assert!(matches!(output, Err(Error::Resource(_))));
            } else {
                assert_eq!(output?.rows, vec![vec![Value::Integer(42)]]);
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn wide_inferred_values_cross_keys_relations_atomic_updates_and_native_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for (index, format) in [
        Arc::new(JsonSnapshotFormat) as Arc<dyn SnapshotFormat>,
        Arc::new(DuckDbFormat::default()),
    ]
    .into_iter()
    .enumerate()
    {
        for evaluator in [
            Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
            Arc::new(BatchedEvaluator),
        ] {
            let path = directory
                .path()
                .join(format!("wide-inference-{index}-{}.db", evaluator.name()));
            let open = || {
                DatabaseBuilder::new()
                    .expressions(evaluator.clone())
                    .batch_size(2)
                    .durability(Arc::new(FileCheckpoint::open(
                        &path,
                        OpenMode::ReadWrite,
                        format.clone(),
                    )?))
                    .build()
            };
            {
                let mut c = open()?.connect();
                c.execute("CREATE TABLE t(k BIGINT PRIMARY KEY,u UHUGEINT,s INTEGER,n BIGINT[]); INSERT INTO t SELECT CASE WHEN i=0 THEN 1::UHUGEINT ELSE 2::INTEGER END,CASE WHEN i=0 THEN 340282366920938463463374607431768211455 ELSE 2::UHUGEINT END,i::INTEGER,[1::UHUGEINT,2::INTEGER,NULL] FROM range(2)t(i)")?;
                assert_eq!(
                    c.query("SELECT CASE WHEN s=1 THEN u ELSE s END FROM t ORDER BY k")?
                        .rows,
                    vec![vec![Value::Integer(0)], vec![Value::Integer(2)]]
                );
                assert_eq!(c.query("SELECT a.k,count(*),sum(CASE WHEN a.s=1 THEN a.u ELSE a.s END) FROM t a JOIN t b ON a.k=CASE WHEN b.s=1 THEN b.u ELSE b.k END GROUP BY a.k ORDER BY a.k")?.rows,
                    vec![vec![Value::Integer(1),Value::Integer(1),Value::Integer(0)],vec![Value::Integer(2),Value::Integer(1),Value::Integer(2)]]);
                assert_eq!(c.query("SELECT sum(CASE WHEN s=1 THEN u ELSE s END) OVER(ORDER BY k ROWS UNBOUNDED PRECEDING) FROM t ORDER BY k")?.rows,vec![vec![Value::Integer(0)],vec![Value::Integer(2)]]);
                assert!(matches!(
                    c.execute("UPDATE t SET k=CASE WHEN s=0 THEN u ELSE s END"),
                    Err(Error::Conversion(_))
                ));
                assert_eq!(
                    c.query("SELECT k FROM t ORDER BY k")?.rows,
                    vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]
                );
                c.execute("BEGIN; DELETE FROM t; ROLLBACK; CHECKPOINT")?;
            }
            let mut c = open()?.connect();
            let p = c.prepare(
                "SELECT k,u::VARCHAR,n::VARCHAR,CASE WHEN s=1 THEN u ELSE s END FROM t WHERE k=$1",
            )?;
            assert_eq!(
                c.execute_prepared(&p, &[Value::Integer(1)])?.rows,
                vec![vec![
                    Value::Integer(1),
                    Value::Varchar(u128::MAX.to_string()),
                    Value::Varchar("[1, 2, NULL]".into()),
                    Value::Integer(0)
                ]]
            );
        }
    }
    let path = directory.path().join("wide-inference-wal.duckdb");
    {
        let mut c = Database::open(&path)?.connect();
        c.execute("CREATE TABLE t AS SELECT CASE WHEN false THEN 340282366920938463463374607431768211455 ELSE 1 END k,[1::UHUGEINT,2::INTEGER] n,340282366920938463463374607431768211455 u; BEGIN; DELETE FROM t; ROLLBACK")?;
    }
    let mut c = Database::open(&path)?.connect();
    assert_eq!(
        c.query("SELECT typeof(k),k,typeof(n),n::VARCHAR,u::VARCHAR FROM t")?
            .rows,
        vec![vec![
            Value::Varchar("BIGINT".into()),
            Value::Integer(1),
            Value::Varchar("BIGINT[]".into()),
            Value::Varchar("[1, 2]".into()),
            Value::Varchar(u128::MAX.to_string())
        ]]
    );
    c.execute("CHECKPOINT")?;
    Ok(())
}
