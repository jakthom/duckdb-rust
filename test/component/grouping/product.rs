use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn product_matches_the_pinned_double_contract_across_batches_and_groups() -> Result<()> {
    for algorithm in algorithms() {
        for batch_size in [1, 3, 2048] {
            let db = DatabaseBuilder::new()
                .physical_planner(Arc::new(
                    NativePhysicalPlanner::default().with_aggregation(algorithm.clone()),
                ))
                .batch_size(batch_size)
                .build()?;
            let mut connection = db.connect();
            let scalar = connection.query(
                "SELECT product(v),product(v::UTINYINT),product(v::DECIMAL(4,1)) \
                 FROM (VALUES (2),(3),(NULL)) t(v)",
            )?;
            assert_eq!(
                scalar
                    .columns
                    .iter()
                    .map(|c| &c.data_type)
                    .collect::<Vec<_>>(),
                vec![&DataType::Double, &DataType::Double, &DataType::Double],
                "{} batch={batch_size}",
                algorithm.name()
            );
            assert_eq!(
                scalar.rows,
                vec![vec![
                    Value::Double(6.0),
                    Value::Double(6.0),
                    Value::Double(6.0)
                ]],
                "{} batch={batch_size}",
                algorithm.name()
            );

            connection.execute(
                "CREATE TABLE p(g INTEGER, v DOUBLE); \
                 INSERT INTO p VALUES (1,2),(1,NULL),(1,3),(2,NULL),(3,-0.0),(3,2)",
            )?;
            assert_eq!(
                connection
                    .query("SELECT g,product(v),product(DISTINCT v) FROM p GROUP BY g ORDER BY g")?
                    .rows,
                vec![
                    vec![Value::Integer(1), Value::Double(6.0), Value::Double(6.0)],
                    vec![Value::Integer(2), Value::Null, Value::Null],
                    vec![Value::Integer(3), Value::Double(-0.0), Value::Double(-0.0)],
                ],
                "{} batch={batch_size}",
                algorithm.name()
            );
            assert_eq!(
                connection
                    .query("SELECT product(v) FROM p WHERE false UNION ALL SELECT product(NULL)")?
                    .rows,
                vec![vec![Value::Null], vec![Value::Null]],
            );
            assert_eq!(
                connection
                    .query(
                        "SELECT g,product(v) OVER(PARTITION BY g) FROM p ORDER BY g,v NULLS LAST"
                    )?
                    .rows,
                vec![
                    vec![Value::Integer(1), Value::Double(6.0)],
                    vec![Value::Integer(1), Value::Double(6.0)],
                    vec![Value::Integer(1), Value::Double(6.0)],
                    vec![Value::Integer(2), Value::Null],
                    vec![Value::Integer(3), Value::Double(-0.0)],
                    vec![Value::Integer(3), Value::Double(-0.0)],
                ],
                "{} batch={batch_size}",
                algorithm.name()
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn product_rejects_wrong_shapes_and_preserves_ieee_multiplication() -> Result<()> {
    let db = DatabaseBuilder::new().build()?;
    let mut connection = db.connect();
    for sql in [
        "SELECT product()",
        "SELECT product(1,2)",
        "SELECT product('x')",
    ] {
        assert!(
            matches!(connection.query(sql), Err(Error::Bind(_))),
            "{sql}"
        );
    }
    let result = connection.query(
        "SELECT product(x),product(y),product(z) FROM \
         (VALUES (1e308::DOUBLE,1e308::DOUBLE,'NaN'::DOUBLE), \
                 (1e308::DOUBLE,1e-308::DOUBLE,2::DOUBLE)) t(x,y,z)",
    )?;
    let Value::Double(infinite) = result.rows[0][0] else {
        panic!("PRODUCT must return DOUBLE")
    };
    assert!(infinite.is_infinite() && infinite.is_sign_positive());
    assert!(matches!(result.rows[0][1], Value::Double(value) if value == 1e308_f64 * 1e-308_f64));
    assert!(matches!(result.rows[0][2], Value::Double(value) if value.is_nan()));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn prepared_product_uses_the_bound_numeric_contract() -> Result<()> {
    let db = DatabaseBuilder::new().build()?;
    let mut connection = db.connect();
    let prepared = connection.prepare("SELECT product(v) FROM (VALUES (?),(?),(NULL)) t(v)")?;
    assert_eq!(
        connection
            .execute_prepared(&prepared, &[Value::Integer(4), Value::Integer(5)])?
            .rows,
        vec![vec![Value::Double(20.0)]]
    );
    Ok(())
}
