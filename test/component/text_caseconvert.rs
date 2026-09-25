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

#[test]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn case_conversion_uses_duckdb_pinned_unicode_15_1_tables() -> Result<()> {
    assert_eq!(utf8proc::unicode_version(), "15.1.0");
    let capital_b = utf8proc::properties::CharProperties::for_char('B');
    assert_eq!(capital_b.as_ffi_property().comb_index, 2784);
    assert_eq!(capital_b.char_width(), Some(1));

    // These code points gained simple case mappings after Unicode 15.1. Both
    // DuckDB pins leave them unchanged, while a current Unicode table does not.
    let drift = "\u{019B}\u{0264}\u{1C89}\u{A7CB}\u{10D50}\u{16EBC}";
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let db = DatabaseBuilder::new()
            .expressions(expressions)
            .batch_size(2)
            .build()?;
        let mut connection = db.connect();
        let sql = "SELECT lower($1), upper($1), lcase($1), ucase($1)";
        let prepared = connection.prepare(sql)?;
        assert_eq!(
            connection
                .execute_prepared(&prepared, &[Value::Varchar(drift.into())])?
                .rows,
            vec![vec![
                Value::Varchar(drift.into()),
                Value::Varchar(drift.into()),
                Value::Varchar(drift.into()),
                Value::Varchar(drift.into()),
            ]]
        );
    }
    Ok(())
}
