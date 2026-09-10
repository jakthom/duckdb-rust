use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    execution::{
        Executor, MaterializingExecutor, PullExecutor,
        index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
        physical_plan::{NativePhysicalPlanner, ScanFilterStrategy},
        subquery::{MaterializingSubqueries, StreamingSubqueries, SubqueryExecutor},
    },
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
};
use std::sync::Arc;

#[path = "../runner/mod.rs"]
mod runner;

#[path = "subqueries/contracts.rs"]
mod contracts;

#[path = "subqueries/failures.rs"]
mod failures;

#[path = "subqueries/decorrelation.rs"]
mod decorrelation;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn adapters() -> [Arc<dyn SubqueryExecutor>; 2] {
    [
        Arc::new(StreamingSubqueries),
        Arc::new(MaterializingSubqueries),
    ]
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn subquery_adapters_share_sql_scope_null_cardinality_and_mutation_contracts() -> Result<()> {
    let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test/sql/subqueries.test");
    for subqueries in adapters() {
        for executor in [
            Arc::new(PullExecutor) as Arc<dyn Executor>,
            Arc::new(MaterializingExecutor),
        ] {
            for optimizer in [
                Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
                Arc::new(PipelineOptimizer::default()),
            ] {
                for indexes in [
                    Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
                    Arc::new(BTreeIndexFactory),
                ] {
                    for batch_size in [1, 3, 2048] {
                        for scan_filters in
                            [ScanFilterStrategy::Separate, ScanFilterStrategy::Fused]
                        {
                            let db = DatabaseBuilder::new()
                                .subqueries(subqueries.clone())
                                .executor(executor.clone())
                                .optimizer(optimizer.clone())
                                .indexes(indexes.clone())
                                .batch_size(batch_size)
                                .physical_planner(Arc::new(
                                    NativePhysicalPlanner::default()
                                        .with_scan_filters(scan_filters),
                                ))
                                .build()?;
                            assert!(db.adapters().contains(&("subqueries", subqueries.name())));
                            assert!(runner::run_file(&db, &corpus)? >= 30);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn prepared_subqueries_rebind_and_preserve_transaction_visibility_and_atomic_errors() -> Result<()>
{
    for subqueries in adapters() {
        let db = DatabaseBuilder::new().subqueries(subqueries).build()?;
        let mut writer = db.connect();
        writer.execute("CREATE TABLE t(i INTEGER); INSERT INTO t VALUES(1)")?;
        let prepared = writer.prepare("SELECT (SELECT max(i) FROM t WHERE i<=$1) v")?;
        let mut reader = db.connect();
        reader.execute("BEGIN")?;
        writer.execute("BEGIN; INSERT INTO t VALUES(2)")?;
        assert_eq!(
            writer
                .execute_prepared(&prepared, &[Value::Integer(2)])?
                .rows,
            vec![vec![Value::Integer(2)]]
        );
        assert_eq!(
            reader.query("SELECT (SELECT count(*) FROM t)")?.rows,
            vec![vec![Value::Integer(1)]]
        );
        writer.execute("COMMIT")?;
        assert_eq!(
            reader.query("SELECT (SELECT count(*) FROM t)")?.rows,
            vec![vec![Value::Integer(1)]]
        );
        reader.execute("COMMIT")?;
        assert_eq!(
            writer
                .execute_prepared(&prepared, &[Value::Integer(0)])?
                .rows,
            vec![vec![Value::Null]]
        );
        assert_eq!(
            writer
                .execute_prepared(&prepared, &[Value::Integer(1)])?
                .rows,
            vec![vec![Value::Integer(1)]]
        );
        writer.execute("BEGIN; INSERT INTO t VALUES(3)")?;
        assert!(matches!(
            writer.execute("UPDATE t SET i=(SELECT i FROM t)"),
            Err(Error::Execution(_))
        ));
        assert!(writer.query("SELECT * FROM t").is_err());
        writer.execute("ROLLBACK")?;
        assert_eq!(
            writer.query("SELECT count(*) FROM t")?.rows,
            vec![vec![Value::Integer(2)]]
        );
        writer.execute("DROP TABLE t; CREATE TABLE t(i VARCHAR); INSERT INTO t VALUES('text')")?;
        let result = writer.execute_prepared(&prepared, &[Value::Varchar("z".into())])?;
        assert_eq!(result.columns[0].data_type, DataType::Varchar);
        assert_eq!(result.rows, vec![vec![Value::Varchar("text".into())]]);
    }
    Ok(())
}
