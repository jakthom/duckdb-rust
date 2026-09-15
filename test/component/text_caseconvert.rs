use std::sync::Arc;

use duckdb_rust::{
    DatabaseBuilder, Result, Value,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn unicode_case_conversion_aliases_nulls_nuls_batches_and_prepared_execution() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let db = DatabaseBuilder::new()
            .expressions(expressions)
            .batch_size(2)
            .build()?;
        let mut connection = db.connect();

        assert_eq!(
            connection.query("SELECT upper('áaaá'),upper('ö'),lower('S̈'),upper('ω'),upper('ß'),lower('İ'),upper('ﬀ'),upper('ΐ'),ucase('MotörHead'),lcase('MotörHead'),upper(''),upper(NULL)")?.rows,
            vec![vec![
                Value::Varchar("ÁAAÁ".into()),
                Value::Varchar("Ö".into()),
                Value::Varchar("s̈".into()),
                Value::Varchar("Ω".into()),
                Value::Varchar("ẞ".into()),
                Value::Varchar("i".into()),
                Value::Varchar("ﬀ".into()),
                Value::Varchar("ΐ".into()),
                Value::Varchar("MOTÖRHEAD".into()),
                Value::Varchar("motörhead".into()),
                Value::Varchar("".into()),
                Value::Null,
            ]]
        );

        connection.execute("CREATE TABLE text_case(v VARCHAR)")?;
        let insert = connection.prepare("INSERT INTO text_case VALUES ($1)")?;
        for value in [
            Value::Varchar("Αα Ββ Σσς".into()),
            Value::Varchar("A\0b".into()),
            Value::Null,
            Value::Varchar("MotörHead".into()),
        ] {
            connection.execute_prepared(&insert, &[value])?;
        }
        let sql = "SELECT upper(v),lower(v),ucase(v),lcase(v) FROM text_case";
        let expected = vec![
            vec![
                Value::Varchar("ΑΑ ΒΒ ΣΣΣ".into()),
                Value::Varchar("αα ββ σσς".into()),
                Value::Varchar("ΑΑ ΒΒ ΣΣΣ".into()),
                Value::Varchar("αα ββ σσς".into()),
            ],
            vec![
                Value::Varchar("A\0B".into()),
                Value::Varchar("a\0b".into()),
                Value::Varchar("A\0B".into()),
                Value::Varchar("a\0b".into()),
            ],
            vec![Value::Null, Value::Null, Value::Null, Value::Null],
            vec![
                Value::Varchar("MOTÖRHEAD".into()),
                Value::Varchar("motörhead".into()),
                Value::Varchar("MOTÖRHEAD".into()),
                Value::Varchar("motörhead".into()),
            ],
        ];
        assert_eq!(connection.query(sql)?.rows, expected);
        let prepared = connection.prepare(sql)?;
        assert_eq!(connection.execute_prepared(&prepared, &[])?.rows, expected);
    }
    Ok(())
}
