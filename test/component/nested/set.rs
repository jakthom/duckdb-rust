use super::*;
use duckdb_rust::execution::expression_executor::{
    BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn list_set_functions_and_sql_operators_share_null_cast_and_duplicate_contracts() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut connection = DatabaseBuilder::new()
            .batch_size(2)
            .expressions(expressions)
            .build()?
            .connect();
        assert_eq!(
            connection
                .query(
                    "SELECT
                    list_has_any([1,NULL,2],[NULL,2,3]),
                    list_has_all([1,NULL,2],[NULL,2,2]),
                    list_has_any([1,2]::INTEGER[2],[2,3]::BIGINT[2]),
                    [1,2] && [2,3],
                    [1,2] @> [2,2],
                    [2,2] <@ [1,2],
                    list_intersect([3,2,3,NULL,1],[2,3,NULL,2])::VARCHAR,
                    list_intersect([3,2],[2,3,2])::VARCHAR,
                    array_intersect([1,2,2]::INTEGER[3],[2,3]::INTEGER[2])::VARCHAR",
                )?
                .rows,
            vec![vec![
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Varchar("[3, 2]".into()),
                // The shorter left input is hashed, so the right probe order
                // determines result order while values come from the left.
                Value::Varchar("[2, 3]".into()),
                Value::Varchar("[2]".into()),
            ]],
        );
        assert_eq!(
            connection
                .query(
                    "SELECT
                    list_has_any(NULL::INTEGER[],[1]),
                    list_has_all([1],NULL::INTEGER[]),
                    list_intersect(NULL::INTEGER[],[1])::VARCHAR,
                    list_intersect([1],NULL::INTEGER[])::VARCHAR,
                    NULL::INTEGER[] @> [1],
                    [1]::INTEGER[] <@ NULL,
                    list_has_all([1,2],[2,3]),
                    list_has_any([1,NULL,2],[NULL,3]),
                    list_has_all([1,NULL,2],[NULL,2,3]),
                    list_has_any([[1,2],NULL, [3,2]],[[2,3],NULL,[1,2]]),
                    list_has_all([[1,2],NULL, [3,2]],[[2,3],NULL,[1,2]]),
                    list_has_any(['a','b','c'],['x','b']),
                    list_has_all(['a','b','c'],['a','b','c']),
                    list_has_any([INTERVAL '1 month'],[INTERVAL '30 days']),
                    list_intersect([INTERVAL '1 month'],[INTERVAL '30 days'])::VARCHAR,
                    typeof(list_intersect([1]::INTEGER[],[1]::BIGINT[])),
                    typeof(list_intersect([1]::INTEGER[],[1.0]::DOUBLE[]))",
                )?
                .rows,
            vec![vec![
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Varchar("[]".into()),
                Value::Null,
                Value::Null,
                Value::Boolean(false),
                Value::Boolean(false),
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Varchar("[]".into()),
                Value::Varchar("BIGINT[]".into()),
                Value::Varchar("DOUBLE[]".into()),
            ]],
        );
        for sql in [
            "SELECT list_has_any([1],1)",
            "SELECT list_has_all([[1,2]],['x'])",
            "SELECT list_has_any([1],['x'])",
            "SELECT [1] && 1",
        ] {
            assert!(connection.query(sql).is_err(), "{sql}");
        }
        connection.execute(
            "CREATE TABLE retained_set(
                any_match BOOLEAN DEFAULT [1,2] && [2,3],
                contains_match BOOLEAN DEFAULT [1,2] @> [2],
                subset_match BOOLEAN DEFAULT [2] <@ [1,2]
            );
            INSERT INTO retained_set DEFAULT VALUES",
        )?;
        assert_eq!(
            connection.query("SELECT * FROM retained_set")?.rows,
            vec![vec![
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(true),
            ]],
        );
        let left = connection.query("SELECT [3,2]::INTEGER[]")?.rows[0][0].clone();
        let right = connection.query("SELECT [2,3,2]::INTEGER[]")?.rows[0][0].clone();
        let prepared = connection.prepare("SELECT list_intersect($1,$2)::VARCHAR, $1 && $2")?;
        assert_eq!(
            connection.execute_prepared(&prepared, &[left, right])?.rows,
            vec![vec![Value::Varchar("[2, 3]".into()), Value::Boolean(true)]],
        );
    }
    Ok(())
}
