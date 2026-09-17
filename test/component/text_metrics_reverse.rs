use std::sync::Arc;

use duckdb_rust::{
    DatabaseBuilder, Error, Result, Value,
    common::{
        DataType,
        vector::{DataChunk, Vector},
    },
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::FunctionRegistry,
    parallel::QueryContext,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn strlen_reverse_and_existing_binary_lengths_match_text_contracts() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut connection = DatabaseBuilder::new()
            .expressions(evaluator)
            .batch_size(2)
            .build()?
            .connect();
        let clusters = "S\u{308}🤦🏼\u{200d}♂\u{fe0f}x";
        assert_eq!(
            connection
                .query(&format!(
                    "SELECT strlen(''),strlen('é'),strlen('a' || chr(0) || 'é'),reverse('{clusters}'),reverse('a' || chr(0) || 'é'),reverse(NULL),bit_length('é'),octet_length(encode('a' || chr(0))),bit_length('001'::BIT),octet_length('001'::BIT)"
                ))?
                .rows,
            vec![vec![
                Value::Integer(0),
                Value::Integer(2),
                Value::Integer(4),
                Value::Varchar("x🤦🏼\u{200d}♂\u{fe0f}S\u{308}".into()),
                Value::Varchar("é\0a".into()),
                Value::Null,
                Value::Integer(16),
                Value::Integer(2),
                Value::Integer(3),
                Value::Integer(1),
            ]]
        );
        connection.execute("CREATE TABLE metrics(v VARCHAR)")?;
        connection
            .execute("INSERT INTO metrics VALUES ('S̈a'), ('🤦🏼‍♂️'), ('a' || chr(0) || 'é'), (NULL)")?;
        assert_eq!(
            connection
                .query("SELECT reverse(v),strlen(v) FROM metrics ORDER BY strlen(v) NULLS LAST")?
                .rows,
            vec![
                vec![Value::Varchar("aS̈".into()), Value::Integer(4)],
                vec![Value::Varchar("é\0a".into()), Value::Integer(4)],
                vec![Value::Varchar("🤦🏼‍♂️".into()), Value::Integer(17)],
                vec![Value::Null, Value::Null],
            ]
        );
        let prepared = connection.prepare("SELECT strlen($1),reverse($1)")?;
        assert_eq!(
            connection
                .execute_prepared(&prepared, &[Value::Varchar("é\0x".into())])?
                .rows,
            vec![vec![Value::Integer(5), Value::Varchar("x\0é".into())]]
        );
        assert!(matches!(
            connection.query("SELECT strlen(42)"),
            Err(Error::Bind(_))
        ));
        assert!(matches!(
            connection.query("SELECT reverse('a','b')"),
            Err(Error::Bind(_))
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn metrics_batch_paths_cover_flat_constant_dictionary_and_selected_vectors() -> Result<()> {
    let query = QueryContext::background();
    let functions = FunctionRegistry::builtins();
    let strlen = functions.scalar("strlen")?;
    let reverse = functions.scalar("reverse")?;
    let flat = Vector::flat(
        DataType::Varchar,
        vec![
            Value::Varchar("a\0é".into()),
            Value::Null,
            Value::Varchar("S̈a".into()),
        ],
    )?;
    let constant = Vector::constant(DataType::Varchar, Value::Varchar("🤦🏼‍♂️".into()), 3)?;
    let parent = Arc::new(Vector::flat(
        DataType::Varchar,
        vec![Value::Varchar("é".into()), Value::Varchar("x\0y".into())],
    )?);
    let dictionary = parent.select(vec![1, 0, 1])?;
    let selected = Arc::new(flat.clone()).select(vec![2, 0])?;
    for (input, lengths, reversed) in [
        (
            flat,
            vec![Value::Integer(4), Value::Null, Value::Integer(4)],
            vec![
                Value::Varchar("é\0a".into()),
                Value::Null,
                Value::Varchar("aS̈".into()),
            ],
        ),
        (
            constant,
            vec![Value::Integer(17); 3],
            vec![Value::Varchar("🤦🏼‍♂️".into()); 3],
        ),
        (
            dictionary,
            vec![Value::Integer(3), Value::Integer(3), Value::Integer(3)],
            vec![
                Value::Varchar("y\0x".into()),
                Value::Varchar("é".into()),
                Value::Varchar("y\0x".into()),
            ],
        ),
        (
            selected,
            vec![Value::Integer(4), Value::Integer(4)],
            vec![Value::Varchar("aS̈".into()), Value::Varchar("é\0a".into())],
        ),
    ] {
        let input = DataChunk::new(vec![input], lengths.len())?;
        assert_eq!(
            strlen
                .evaluate_batch(&input, &query)?
                .expect("strlen batch result")
                .values()
                .collect::<Vec<_>>(),
            lengths
        );
        assert_eq!(
            reverse
                .evaluate_batch(&input, &query)?
                .expect("reverse batch result")
                .values()
                .collect::<Vec<_>>(),
            reversed
        );
    }
    Ok(())
}
