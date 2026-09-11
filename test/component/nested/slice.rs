use super::*;
use duckdb_rust::{
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn list_and_array_slice_match_pinned_bounds_steps_and_nulls() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut connection = DatabaseBuilder::new()
                .expressions(expressions.clone())
                .optimizer(optimizer)
                .build()?
                .connect();
            assert_eq!(
                connection
                    .query(
                        "SELECT
                        list_slice([1,2,NULL,4],2,4)::VARCHAR,
                        list_slice([1,2,3,4],-3,-1)::VARCHAR,
                        list_slice([1,2,3,4],4,1,-2)::VARCHAR,
                        list_slice([1,2,3],0,2)::VARCHAR,
                        list_slice([1,2,3],-99,2)::VARCHAR,
                        list_slice([1,2,3],2,99)::VARCHAR",
                    )?
                    .rows,
                vec![vec![
                    Value::Varchar("[2, NULL, 4]".into()),
                    Value::Varchar("[2, 3, 4]".into()),
                    Value::Varchar("[4, 2]".into()),
                    Value::Varchar("[1, 2]".into()),
                    Value::Varchar("[1, 2]".into()),
                    Value::Varchar("[2, 3]".into()),
                ]],
            );
            assert_eq!(
                connection
                    .query(
                        "SELECT
                        [1,2,3][2:]::VARCHAR,
                        [1,2,3][:-2]::VARCHAR,
                        [1,2,3][1:3:2]::VARCHAR,
                        [1,2,3][: : -1]::VARCHAR,
                        [1,2,3][1: : -1]::VARCHAR,
                        typeof(([1,2,3]::INTEGER[3])[1:2])",
                    )?
                    .rows,
                vec![vec![
                    Value::Varchar("[2, 3]".into()),
                    Value::Varchar("[1, 2]".into()),
                    Value::Varchar("[1, 3]".into()),
                    Value::Varchar("[3, 2, 1]".into()),
                    Value::Varchar("[1]".into()),
                    Value::Varchar("INTEGER[]".into()),
                ]],
            );
            assert_eq!(
                connection
                    .query(
                        "SELECT
                        list_slice([1,2,3],NULL,2),
                        list_slice([1,2,3],1,NULL),
                        list_slice([1,2,3],1,2,NULL),
                        (NULL::INTEGER[])[1:2]",
                    )?
                    .rows,
                vec![vec![Value::Null, Value::Null, Value::Null, Value::Null]],
            );
            for sql in [
                "SELECT list_slice([1,2,3],1,3,0)",
                "SELECT list_slice([1,2,3],[],2)",
                "SELECT list_slice(1,1,2)",
                "SELECT list_slice([1,2],1)",
            ] {
                assert!(connection.query(sql).is_err(), "{sql}");
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn slices_preserve_parameters_batches_nested_children_and_native_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("nested-slice.duckdb");
    let input = Database::memory()?
        .connect()
        .query("SELECT [[1,NULL],[2,3],NULL,[4]]")?
        .rows[0][0]
        .clone();
    let mut bounded = DatabaseBuilder::new()
        .max_intermediate_rows(2)
        .build()?
        .connect();
    let bounded_slice = bounded.prepare("SELECT list_slice($1,1,4)")?;
    assert!(matches!(
        bounded.execute_prepared(&bounded_slice, std::slice::from_ref(&input)),
        Err(duckdb_rust::Error::Resource(_))
    ));
    {
        let mut connection = Database::open(&path)?.connect();
        let prepared =
            connection.prepare("SELECT list_slice($1,$2,$3,$4)::VARCHAR, $1[$2:$3]::VARCHAR")?;
        assert_eq!(
            connection
                .execute_prepared(
                    &prepared,
                    &[
                        input.clone(),
                        Value::Integer(2),
                        Value::Integer(4),
                        Value::Integer(2),
                    ],
                )?
                .rows,
            vec![vec![
                Value::Varchar("[[2, 3], [4]]".into()),
                Value::Varchar("[[2, 3], NULL, [4]]".into()),
            ]],
        );
        connection.execute(
            "CREATE TABLE slices(id INTEGER PRIMARY KEY,xs INTEGER[],part INTEGER[]);
             INSERT INTO slices
             SELECT i,[i,i+1,NULL,i+3],[i,i+1,NULL,i+3][2:4]
             FROM range(6) t(i);
             BEGIN;
             UPDATE slices SET part=list_slice(xs,4,1,-2);
             ROLLBACK;
             UPDATE slices SET part=xs[:2] WHERE id%2=0",
        )?;
        assert_eq!(
            connection
                .query("SELECT part::VARCHAR FROM slices ORDER BY id")?
                .rows,
            vec![
                vec![Value::Varchar("[0, 1]".into())],
                vec![Value::Varchar("[2, NULL, 4]".into())],
                vec![Value::Varchar("[2, 3]".into())],
                vec![Value::Varchar("[4, NULL, 6]".into())],
                vec![Value::Varchar("[4, 5]".into())],
                vec![Value::Varchar("[6, NULL, 8]".into())],
            ],
        );
    }
    let mut connection = Database::open_read_only(&path)?.connect();
    assert_eq!(
        connection
            .query(
                "SELECT count(*),min(list_extract(part,1)),max(list_extract(part,-1)) FROM slices",
            )?
            .rows,
        vec![vec![
            Value::Integer(6),
            Value::Integer(0),
            Value::Integer(8),
        ]],
    );
    Ok(())
}
