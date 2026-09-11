use super::*;
use duckdb_rust::execution::expression_executor::{
    BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator,
};
use duckdb_rust::optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sequence_mechanics_match_pinned_alias_null_index_and_result_contracts() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut connection = DatabaseBuilder::new()
            .expressions(expressions)
            .build()?
            .connect();
        assert_eq!(
            connection
                .query(
                    "SELECT
                        list_contains([1,NULL,2],2),
                        list_contains([1,NULL,2],NULL),
                        list_has([1,NULL,2],3),
                        list_position([1,NULL,2],NULL),
                        list_position([1,2,1],1),
                        list_indexof([1,2],9),
                        array_position([1,2]::INTEGER[2],2),
                        array_indexof([1,2],1),
                        list_position([[1,NULL],[2,3]],[1,NULL]),
                        list_contains([{'a':1,'b':NULL}],{'a':1,'b':NULL})",
                )?
                .rows,
            vec![vec![
                Value::Boolean(true),
                Value::Null,
                Value::Boolean(false),
                Value::Integer(2),
                Value::Integer(1),
                Value::Null,
                Value::Integer(2),
                Value::Integer(1),
                Value::Integer(1),
                Value::Boolean(true),
            ]],
        );
        assert_eq!(
            connection
                .query(
                    "SELECT
                        list_select([10,NULL,30],[3,1,3,0,-1,9])::VARCHAR,
                        array_select([10,20]::INTEGER[2],[2,1])::VARCHAR,
                        typeof(array_select([10,20]::INTEGER[2],[2])),
                        list_select(NULL::INTEGER[],[1,NULL]),
                        list_resize([1,2],5)::VARCHAR,
                        list_resize([1,2],5,9)::VARCHAR,
                        list_resize([1,2],1,9)::VARCHAR,
                        list_resize([1,2],NULL,9)::VARCHAR,
                        list_resize(NULL::INTEGER[],3,9),
                        typeof(list_resize([1,2]::INTEGER[2],3)),
                        list_reverse([1,NULL,3])::VARCHAR,
                        array_reverse([1,2]::INTEGER[2])::VARCHAR,
                        typeof(array_reverse([1,2]::INTEGER[2]))",
                )?
                .rows,
            vec![vec![
                Value::Varchar("[30, 10, 30, NULL, NULL, NULL]".into()),
                Value::Varchar("[20, 10]".into()),
                Value::Varchar("INTEGER[]".into()),
                Value::Null,
                Value::Varchar("[1, 2, NULL, NULL, NULL]".into()),
                Value::Varchar("[1, 2, 9, 9, 9]".into()),
                Value::Varchar("[1]".into()),
                Value::Varchar("[]".into()),
                Value::Null,
                Value::Varchar("INTEGER[]".into()),
                Value::Varchar("[3, NULL, 1]".into()),
                Value::Varchar("[2, 1]".into()),
                Value::Varchar("INTEGER[]".into()),
            ]],
        );
        assert_eq!(
            connection
                .query(
                    "SELECT
                        typeof(list_contains(NULL,1)),
                        typeof(list_position(NULL,1)),
                        typeof(list_select(NULL,[1])),
                        typeof(list_select([1],NULL)),
                        typeof(list_resize(NULL,3)),
                        typeof(list_reverse(NULL))",
                )?
                .rows,
            vec![vec![
                Value::Varchar("BOOLEAN".into()),
                Value::Varchar("INTEGER".into()),
                Value::Varchar("\"NULL\"".into()),
                Value::Varchar("\"NULL\"".into()),
                Value::Varchar("\"NULL\"".into()),
                Value::Varchar("\"NULL\"".into()),
            ]],
        );
        assert_eq!(
            connection
                .query(
                    "SELECT
                        list_contains([DATE '2020-01-01'],'2020-01-01'),
                        list_contains([1],true),
                        list_contains([1::TINYINT],1000),
                        typeof(list_resize([1::TINYINT],3,2::SMALLINT))",
                )?
                .rows,
            vec![vec![
                Value::Boolean(true),
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Varchar("TINYINT[]".into()),
            ]],
        );
        for sql in [
            "SELECT list_select([1,2],[1,NULL])",
            "SELECT list_select([1,2],['1','2'])",
            "SELECT list_resize([1::TINYINT],3,1000)",
            "SELECT list_resize([1],-1)",
            "SELECT list_contains([DATE '2020-01-01'],x) FROM (VALUES ('2020-01-01')) t(x)",
        ] {
            assert!(connection.query(sql).is_err(), "{sql}");
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sequence_mechanics_cross_parameters_and_multi_vector_batches() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut connection = DatabaseBuilder::new()
            .expressions(expressions)
            .build()?
            .connect();
        let input = connection.query("SELECT [1,NULL,3]")?.rows[0][0].clone();
        let indices = connection.query("SELECT [3,1,9]")?.rows[0][0].clone();
        let prepared = connection.prepare(
            "SELECT
                list_contains($1,$2),
                list_position($1,$3),
                list_select($1,$4)::VARCHAR,
                list_resize($1,$5,$2)::VARCHAR,
                list_reverse($1)::VARCHAR",
        )?;
        assert_eq!(
            connection
                .execute_prepared(
                    &prepared,
                    &[
                        input,
                        Value::Integer(3),
                        Value::Null,
                        indices,
                        Value::Integer(5),
                    ],
                )?
                .rows,
            vec![vec![
                Value::Boolean(true),
                Value::Integer(2),
                Value::Varchar("[3, 1, NULL]".into()),
                Value::Varchar("[1, NULL, 3, 3, 3]".into()),
                Value::Varchar("[3, NULL, 1]".into()),
            ]],
        );

        let rows = connection
            .query(
                "SELECT
                    i,
                    list_contains([i,NULL,i+1],i+1),
                    list_position([i,NULL,i+1],NULL),
                    list_select([i,i+1],[2,1,9])::VARCHAR,
                    list_resize([i],3,i+2)::VARCHAR,
                    list_reverse([i,NULL,i+1])::VARCHAR
                 FROM range(2050) t(i)",
            )?
            .rows;
        assert_eq!(rows.len(), 2050);
        for (index, row) in rows.iter().enumerate() {
            let index = index as i128;
            assert_eq!(
                row,
                &vec![
                    Value::Integer(index),
                    Value::Boolean(true),
                    Value::Integer(2),
                    Value::Varchar(format!("[{}, {index}, NULL]", index + 1)),
                    Value::Varchar(format!("[{index}, {}, {}]", index + 2, index + 2)),
                    Value::Varchar(format!("[{}, NULL, {index}]", index + 1)),
                ]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sequence_aliases_and_constant_null_demand_match_pinned_functions() -> Result<()> {
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
                            array_contains([1,2]::INTEGER[2],2),
                            array_has([1,2]::INTEGER[2],3),
                            array_resize([1,2]::INTEGER[2],4,9)::VARCHAR,
                            typeof(array_resize([1,2]::INTEGER[2],3))",
                    )?
                    .rows,
                vec![vec![
                    Value::Boolean(true),
                    Value::Boolean(false),
                    Value::Varchar("[1, 2, 9, 9]".into()),
                    Value::Varchar("INTEGER[]".into()),
                ]],
            );
            assert_eq!(
                connection
                    .query(
                        "SELECT
                            list_contains(NULL::INTEGER[],'bad'::INTEGER),
                            list_contains(['bad'::INTEGER],NULL::INTEGER),
                            list_select(NULL::INTEGER[],['bad'::BIGINT]),
                            list_select(['bad'::INTEGER],NULL::BIGINT[])"
                    )?
                    .rows,
                vec![vec![Value::Null, Value::Null, Value::Null, Value::Null]],
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn positional_where_and_zip_match_pinned_array_null_and_shape_contracts() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut connection = DatabaseBuilder::new()
            .expressions(expressions)
            .build()?
            .connect();
        assert_eq!(
            connection
                .query(
                    "SELECT
                        list_where([10,NULL,30],[true,true,false])::VARCHAR,
                        list_where([1],[true,false,true])::VARCHAR,
                        array_where([1,2,3]::INTEGER[3],[true,false,true]::BOOLEAN[3])::VARCHAR,
                        typeof(array_where([1,2]::INTEGER[2],[true]::BOOLEAN[1])),
                        list_zip([1,2],['a'])::VARCHAR,
                        list_zip([1,2],['a'],true)::VARCHAR,
                        list_zip([1,2],NULL)::VARCHAR,
                        list_zip([1,2],NULL,true)::VARCHAR,
                        list_zip(NULL::BOOLEAN[],[1],NULL::BOOLEAN)::VARCHAR,
                        array_zip([1,2]::INTEGER[2],[true]::BOOLEAN[1])::VARCHAR,
                        typeof(list_zip(NULL))",
                )?
                .rows,
            vec![vec![
                Value::Varchar("[10, NULL]".into()),
                Value::Varchar("[1, NULL]".into()),
                Value::Varchar("[1, 3]".into()),
                Value::Varchar("INTEGER[]".into()),
                Value::Varchar("[(1, a), (2, NULL)]".into()),
                Value::Varchar("[(1, a)]".into()),
                Value::Varchar("[(1, NULL), (2, NULL)]".into()),
                Value::Varchar("[]".into()),
                Value::Varchar("[(NULL, 1)]".into()),
                Value::Varchar("[(1, true), (2, NULL)]".into()),
                Value::Varchar("TUPLE(\"NULL\")[]".into()),
            ]],
        );
        assert_eq!(
            connection
                .query(
                    "SELECT
                        list_where(NULL::INTEGER[],['bad'::BOOLEAN]),
                        list_where(['bad'::INTEGER],NULL::BOOLEAN[]),
                        typeof(list_where(NULL,[true])),
                        typeof(list_where([1],NULL))",
                )?
                .rows,
            vec![vec![
                Value::Null,
                Value::Null,
                Value::Varchar("\"NULL\"".into()),
                Value::Varchar("\"NULL\"".into()),
            ]],
        );
        for sql in [
            "SELECT list_where([1,2],[true,NULL])",
            "SELECT list_where([1,2],[1,0])",
            "SELECT list_where([1])",
            "SELECT list_zip()",
            "SELECT list_zip(true)",
            "SELECT list_zip([1],2)",
            "SELECT list_zip(NULL::BOOLEAN)",
        ] {
            assert!(connection.query(sql).is_err(), "{sql}");
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn positional_where_and_zip_cross_parameters_and_multi_vector_batches() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut connection = DatabaseBuilder::new()
            .expressions(expressions)
            .build()?
            .connect();
        let input = connection.query("SELECT [1,NULL,3]")?.rows[0][0].clone();
        let labels = connection.query("SELECT ['a','b']")?.rows[0][0].clone();
        let mask = connection.query("SELECT [true,false,true,true]")?.rows[0][0].clone();
        let prepared =
            connection.prepare("SELECT list_where($1,$2)::VARCHAR,list_zip($1,$3,$4)::VARCHAR")?;
        assert_eq!(
            connection
                .execute_prepared(&prepared, &[input, mask, labels, Value::Boolean(false)],)?
                .rows,
            vec![vec![
                Value::Varchar("[1, 3, NULL]".into()),
                Value::Varchar("[(1, a), (NULL, b), (3, NULL)]".into()),
            ]],
        );

        let rows = connection
            .query(
                "SELECT
                    i,
                    list_where([i,NULL,i+1],[true,false,true,true])::VARCHAR,
                    list_zip([i,i+1],[i*2],i%2=0)::VARCHAR
                 FROM range(2050) t(i)",
            )?
            .rows;
        assert_eq!(rows.len(), 2050);
        for (index, row) in rows.iter().enumerate() {
            let index = index as i128;
            let zipped = if index % 2 == 0 {
                format!("[({index}, {})]", index * 2)
            } else {
                format!("[({index}, {}), ({}, NULL)]", index * 2, index + 1)
            };
            assert_eq!(
                row,
                &vec![
                    Value::Integer(index),
                    Value::Varchar(format!("[{index}, {}, NULL]", index + 1)),
                    Value::Varchar(zipped),
                ]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn create_retained_sequence_defaults(connection: &mut duckdb_rust::Connection) -> Result<()> {
    connection.execute(
        "CREATE TABLE seq(id INTEGER PRIMARY KEY,xs INTEGER[]);
         INSERT INTO seq VALUES(1,[1,NULL,3]);
         ALTER TABLE seq ADD COLUMN has BOOLEAN DEFAULT list_contains([1,NULL],1);
         ALTER TABLE seq ADD COLUMN pos INTEGER DEFAULT list_position([1,NULL],NULL);
         ALTER TABLE seq ADD COLUMN selected INTEGER[] DEFAULT list_select([10,20],[2,0]);
         ALTER TABLE seq ADD COLUMN filtered INTEGER[] DEFAULT list_where([10,20],[true,false,true]);
         ALTER TABLE seq ADD COLUMN resized INTEGER[] DEFAULT list_resize([1],3,9);
         ALTER TABLE seq ADD COLUMN reversed INTEGER[] DEFAULT list_reverse([1,NULL,3]);
         ALTER TABLE seq ADD COLUMN zipped STRUCT(a INTEGER,b VARCHAR)[] DEFAULT list_zip([1,2],['a']);
         INSERT INTO seq(id,xs) VALUES(2,[4,5])",
    )?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn mutate_retained_sequences(connection: &mut duckdb_rust::Connection) -> Result<()> {
    assert_sequence_rows(connection, 2)?;
    let before = connection.query("SELECT * FROM seq ORDER BY id")?.rows;
    connection.execute(
        "BEGIN;
         UPDATE seq SET xs=list_resize(list_reverse(xs),5,id),
             zipped=list_zip([id,id+1],['rollback']);
         DELETE FROM seq WHERE id=2;
         ROLLBACK",
    )?;
    assert_eq!(
        connection.query("SELECT * FROM seq ORDER BY id")?.rows,
        before
    );
    connection.execute(
        "UPDATE seq SET xs=list_where(list_reverse(xs),[true,false,true,true]) WHERE id=1;
         INSERT INTO seq(id,xs) VALUES(3,[7,NULL])",
    )?;
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn assert_sequence_rows(
    connection: &mut duckdb_rust::Connection,
    expected_count: i128,
) -> Result<()> {
    assert_eq!(
        connection
            .query(
                "SELECT count(*),count(*) FILTER(WHERE has),min(pos),
                    count(*) FILTER(WHERE selected::VARCHAR='[20, NULL]'),
                    count(*) FILTER(WHERE filtered::VARCHAR='[10, NULL]'),
                    count(*) FILTER(WHERE resized::VARCHAR='[1, 9, 9]'),
                    count(*) FILTER(WHERE reversed::VARCHAR='[3, NULL, 1]'),
                    count(*) FILTER(WHERE zipped::VARCHAR='[{''a'': 1, ''b'': a}, {''a'': 2, ''b'': NULL}]')
                 FROM seq",
            )?
            .rows,
        vec![vec![
            Value::Integer(expected_count),
            Value::Integer(expected_count),
            Value::Integer(2),
            Value::Integer(expected_count),
            Value::Integer(expected_count),
            Value::Integer(expected_count),
            Value::Integer(expected_count),
            Value::Integer(expected_count),
        ]],
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn retained_sequence_defaults_cross_mutation_private_and_native_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;

    let private = directory.path().join("sequence-private.snapshot");
    {
        let database = DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &private,
                OpenMode::ReadWrite,
                Arc::new(JsonSnapshotFormat),
            )?))
            .build()?;
        create_retained_sequence_defaults(&mut database.connect())?;
    }
    {
        let database = DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &private,
                OpenMode::ReadWrite,
                Arc::new(JsonSnapshotFormat),
            )?))
            .build()?;
        let mut connection = database.connect();
        mutate_retained_sequences(&mut connection)?;
        connection.checkpoint()?;
    }
    {
        let database = DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &private,
                OpenMode::ReadOnly,
                Arc::new(JsonSnapshotFormat),
            )?))
            .build()?;
        let mut connection = database.connect();
        assert_sequence_rows(&mut connection, 3)?;
        assert_eq!(
            connection
                .query("SELECT xs::VARCHAR,list_position(xs,NULL) FROM seq WHERE id=1")?
                .rows,
            vec![vec![
                Value::Varchar("[3, 1, NULL]".into()),
                Value::Integer(3),
            ]],
        );
    }

    let native = directory.path().join("sequence-native.duckdb");
    create_retained_sequence_defaults(&mut Database::open(&native)?.connect())?;
    {
        let mut connection = Database::open(&native)?.connect();
        mutate_retained_sequences(&mut connection)?;
        connection.checkpoint()?;
    }
    let mut connection = Database::open_read_only(&native)?.connect();
    assert_sequence_rows(&mut connection, 3)?;
    assert_eq!(
        connection
            .query("SELECT xs::VARCHAR,list_position(xs,NULL) FROM seq WHERE id=1")?
            .rows,
        vec![vec![
            Value::Varchar("[3, 1, NULL]".into()),
            Value::Integer(3),
        ]],
    );
    Ok(())
}
