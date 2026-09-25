use super::*;
use duckdb_rust::{
    Error,
    execution::expression_executor::ExpressionEvaluator,
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn specifier_error(result: Result<duckdb_rust::QueryResult>) {
    assert!(
        matches!(result, Err(Error::Conversion(message)) if message == "extract specifier \"bad\" not recognized")
    );
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn difference_dispatch_uses_executed_constants_not_closedness_equality_or_cardinality() -> Result<()>
{
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            for size in [1, 2, 5] {
                let mut c = DatabaseBuilder::new()
                    .expressions(evaluator.clone())
                    .optimizer(optimizer.clone())
                    .batch_size(size)
                    .build()?
                    .connect();
                for function in ["date_diff", "datediff", "date_sub", "datesub"] {
                    for sql in [
                        format!(
                            "SELECT {function}('bad',d,DATE 'epoch') FROM (SELECT NULL::DATE d)"
                        ),
                        format!(
                            "SELECT {function}(p,d,DATE 'epoch') FROM (SELECT 'bad' p,NULL::DATE d)"
                        ),
                        format!(
                            "SELECT {function}(p,CAST('bad' AS DATE),DATE 'epoch') FROM (SELECT NULL::VARCHAR p)"
                        ),
                        format!(
                            "SELECT {function}('bad',d,CAST('bad' AS DATE)) FROM (SELECT NULL::DATE d)"
                        ),
                    ] {
                        assert_eq!(c.query(&sql)?.rows, vec![vec![Value::Null]], "{sql}");
                    }
                    specifier_error(c.query(&format!(
                        "SELECT {function}('bad',d,DATE 'epoch') FROM (VALUES (NULL::DATE)) t(d)"
                    )));
                    for source in [
                        "(SELECT 'bad' p)",
                        "(SELECT 'bad' p FROM range(3))",
                        "(SELECT 'bad' p FROM range(3) t(i) WHERE i<>1)",
                        "(SELECT 'bad' p,concat('x','y') x FROM range(3))",
                    ] {
                        specifier_error(c.query(&format!(
                            "SELECT {function}(p,DATE 'infinity',DATE 'epoch') FROM {source}"
                        )));
                    }
                    for part in [
                        "(SELECT 'bad')",
                        "(SELECT p FROM (VALUES ('bad')) t(p))",
                        "concat('b','ad')",
                    ] {
                        specifier_error(c.query(&format!(
                            "SELECT {function}({part},TIMESTAMP 'infinity',TIMESTAMP 'epoch')"
                        )));
                    }
                    assert_eq!(c.query(&format!("SELECT {function}(p,DATE 'infinity',DATE 'epoch') FROM (VALUES ('bad')) t(p)"))?.rows,vec![vec![Value::Null]]);
                    assert_eq!(c.query(&format!("SELECT {function}(p,DATE 'infinity',DATE 'epoch') FROM (VALUES ('bad'),('bad')) t(p)"))?.rows,vec![vec![Value::Null];2]);
                    assert_eq!(c.query(&format!("SELECT {function}(p,DATE 'infinity',DATE 'epoch') FROM (SELECT 'bad' p FROM range(3) ORDER BY 1)"))?.rows,vec![vec![Value::Null];3]);
                    assert!(c.query(&format!("SELECT {function}((SELECT 'bad'),DATE 'infinity',DATE 'epoch') FROM range(0)"))?.rows.is_empty());
                    assert_eq!(c.query(&format!("SELECT CASE WHEN false THEN {function}((SELECT 'bad'),DATE 'infinity',DATE 'epoch') ELSE 7 END"))?.rows,vec![vec![Value::Integer(7)]]);
                    let prepared = c.prepare(&format!("SELECT {function}(p,DATE 'infinity',DATE 'epoch') FROM (SELECT $1 p FROM range(3))"))?;
                    specifier_error(c.execute_prepared(&prepared, &[Value::Varchar("bad".into())]));
                    assert_eq!(
                        c.execute_prepared(&prepared, &[Value::Varchar("day".into())])?
                            .rows,
                        vec![vec![Value::Null]; 3]
                    );
                }
            }
        }
    }
    Ok(())
}

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn temporal_execution_provenance_survives_mutation_errors_rollback_native_wal_and_reopen()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    for batched in [false, true] {
        let path = directory
            .path()
            .join(format!("provenance-{batched}.duckdb"));
        let open = || {
            let checkpoint = FileCheckpoint::open(
                path.clone(),
                OpenMode::ReadWrite,
                Arc::new(DuckDbFormat::default()),
            )?
            .with_recovery(Arc::new(DuckDbWalRecovery))?;
            DatabaseBuilder::new()
                .optimizer(Arc::new(IdentityOptimizer))
                .batch_size(2)
                .expressions(if batched {
                    Arc::new(BatchedEvaluator)
                } else {
                    Arc::new(ScalarEvaluator)
                })
                .durability(Arc::new(FileWal::new(
                    checkpoint,
                    Arc::new(DuckDbTransactionLog),
                )?))
                .build()
        };
        let mut c = open()?.connect();
        c.execute("CREATE TABLE periods(k INTEGER UNIQUE,p VARCHAR,d DATE,result BIGINT); INSERT INTO periods VALUES(1,'bad',DATE 'infinity',9),(2,'day',DATE '1970-01-02',8)")?;
        let query = "SELECT k,p,d,result FROM periods ORDER BY k";
        let before = c.query(query)?.rows;
        for statement in [
            "INSERT INTO periods SELECT 3,'bad',DATE 'infinity',date_diff((SELECT 'bad'),DATE 'infinity',DATE 'epoch')",
            "UPDATE periods SET result=date_sub((SELECT 'bad'),d,DATE 'epoch')",
        ] {
            assert!(
                matches!(c.execute(statement),Err(Error::Conversion(message)) if message == "extract specifier \"bad\" not recognized")
            );
            assert_eq!(c.query(query)?.rows, before);
        }
        c.execute("BEGIN; UPDATE periods SET result=date_diff(p,d,DATE 'epoch'); ROLLBACK")?;
        assert_eq!(c.query(query)?.rows, before);
        c.execute("UPDATE periods SET result=date_sub(p,d,DATE 'epoch')")?;
        assert_eq!(
            c.query("SELECT result FROM periods ORDER BY k")?.rows,
            vec![vec![Value::Null], vec![Value::Integer(-1)]]
        );
        assert_eq!(
            c.query("SELECT count(*) FROM periods a JOIN periods b ON date_diff('day',a.d,b.d)=0")?
                .rows,
            vec![vec![Value::Integer(1)]]
        );
        assert_eq!(c.query("SELECT date_diff(p,d,DATE 'epoch'),count(*) FROM periods GROUP BY date_diff(p,d,DATE 'epoch') ORDER BY 1 NULLS LAST")?.rows,vec![vec![Value::Integer(-1),Value::Integer(1)],vec![Value::Null,Value::Integer(1)]]);
        assert_eq!(
            c.query(
                "SELECT max(date_sub(p,d,DATE 'epoch')) OVER(ORDER BY k) FROM periods ORDER BY k"
            )?
            .rows,
            vec![vec![Value::Null], vec![Value::Integer(-1)]]
        );
        let after = c.query(query)?.rows;
        drop(c);
        let mut c = open()?.connect();
        assert_eq!(c.query(query)?.rows, after);
        let prepared = c.prepare(
            "SELECT result FROM periods WHERE k=date_diff('day',DATE 'epoch',CAST($1 AS DATE))",
        )?;
        assert_eq!(
            c.execute_prepared(&prepared, &[Value::Varchar("1970-01-03".into())])?
                .rows,
            vec![vec![Value::Integer(-1)]]
        );
        c.checkpoint()?;
        drop(c);
        assert_eq!(
            Database::open_read_only(&path)?
                .connect()
                .query(query)?
                .rows,
            after
        );
    }
    Ok(())
}
