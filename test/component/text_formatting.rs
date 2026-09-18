use std::sync::Arc;

use duckdb_rust::{
    DatabaseBuilder, Result, Value,
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn printf_and_format_cover_values_nulls_nuls_and_errors() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut connection = DatabaseBuilder::new()
            .expressions(expressions)
            .batch_size(2)
            .build()?
            .connect();
        assert_eq!(
            connection.query("SELECT printf('%s:%d:%x:%.2f:%c', 'a' || chr(0) || 'é', -12, 255, 1.25, 65), format('{}:{}:{:04d}:{:b}:{:.2}', 'a' || chr(0) || 'é', 12, 9, 5, 1.234)")?.rows,
            vec![vec![
                Value::Varchar("a\0é:-12:ff:1.25:A".into()),
                Value::Varchar("a\0é:12:0009:101:1.2".into()),
            ]]
        );
        assert_eq!(
            connection.query("SELECT printf('%*d', 4, 12), printf('%d', 7, 999), format('{:.2}', 0.000234), format('{:.2}', 0.0), format('{:.3}', 0.0)")?.rows,
            vec![vec![Value::Varchar("  12".into()), Value::Varchar("7".into()), Value::Varchar("0.00023".into()), Value::Varchar("0.0".into()), Value::Varchar("0.00".into())]]
        );
        assert_eq!(
            connection
                .query("SELECT printf('%08d:%s', -12, 'é'), format('{1}:{0}', 12, 'x')")?
                .rows,
            vec![vec![
                Value::Varchar("-0000012:é".into()),
                Value::Varchar("x:12".into()),
            ]]
        );
        assert_eq!(
            connection.query("SELECT printf('%d:%#o', 18446744073709551615::UBIGINT, 100), printf('floats: %4.2f %+.0e %E', 3.1416, 3.1416, 3.1416), printf('%s', DATE '1992-01-01'), format('{}', DATE '1992-01-01')")?.rows,
            vec![vec![
                Value::Varchar("18446744073709551615:0144".into()),
                Value::Varchar("floats: 3.14 +3e+00 3.141600E+00".into()),
                Value::Varchar("1992-01-01".into()),
                Value::Varchar("1992-01-01".into()),
            ]]
        );
        assert_eq!(
            connection.query("SELECT printf('%u:%o:%X:%b:%#x:%d', 18446744073709551615::UBIGINT, 8, 255, 5, 15, true), printf('%s', NULL), format('{}', NULL)")?.rows,
            vec![vec![Value::Varchar("18446744073709551615:10:FF:101:0xf:1".into()), Value::Null, Value::Null]]
        );
        connection.execute("CREATE TABLE formatting(f VARCHAR, n BIGINT)")?;
        connection.execute("INSERT INTO formatting VALUES ('x=%d', 7), ('%04d', 9)")?;
        assert_eq!(
            connection
                .query("SELECT printf(f, n) FROM formatting ORDER BY n")?
                .rows,
            vec![
                vec![Value::Varchar("x=7".into())],
                vec![Value::Varchar("0009".into())]
            ]
        );
        let prepared = connection.prepare("SELECT printf($1, $2), format($3, $2)")?;
        assert_eq!(
            connection
                .execute_prepared(
                    &prepared,
                    &[
                        Value::Varchar("%d".into()),
                        Value::Integer(42),
                        Value::Varchar("{:x}".into())
                    ]
                )?
                .rows,
            vec![vec![
                Value::Varchar("42".into()),
                Value::Varchar("2a".into())
            ]]
        );
        for sql in [
            "SELECT printf('%d')",
            "SELECT printf('%q', 1)",
            "SELECT printf('%c', -1)",
            "SELECT printf('%c', 128)",
            "SELECT printf('%s', 42)",
            "SELECT format('{:s}', 42)",
            "SELECT format('{', 1)",
        ] {
            assert!(
                connection.query(sql).is_err(),
                "{sql} unexpectedly succeeded"
            );
        }
    }
    Ok(())
}
