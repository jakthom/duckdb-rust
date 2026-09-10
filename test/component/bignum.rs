use std::sync::Arc;

use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    common::{
        BignumValue,
        cast::{CastMode, CastRegistry},
        type_registry::builtin_types,
        vector::Vector,
    },
    parallel::QueryContext,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn number(text: &str) -> Value {
    BignumValue::parse(text, || Ok(())).unwrap().value()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn magnitude_arithmetic_native_encoding_and_decimal_conversion_preserve_full_domains() -> Result<()>
{
    assert!(std::mem::size_of::<Value>() <= 32);
    assert!(std::mem::size_of::<DataType>() <= 16);
    let query = QueryContext::background();
    for left in [
        i128::MIN,
        i128::MIN + 1,
        -4294967296,
        -256,
        -1,
        0,
        1,
        255,
        256,
        4294967296,
        i128::MAX,
    ] {
        let value = BignumValue::from_i128(left);
        assert_eq!(value.to_decimal(|| query.check())?, left.to_string());
        assert_eq!(
            BignumValue::parse(&left.to_string(), || query.check())?,
            value
        );
        assert_eq!(
            BignumValue::from_native(&value.to_native(|| query.check())?, || query.check())?,
            value
        );
        for right in [-65536, -256, -1, 0, 1, 256, 65536] {
            let other = BignumValue::from_i128(right);
            if let Some(sum) = left.checked_add(right) {
                assert_eq!(
                    value.add(&other, || query.check())?.to_string(),
                    sum.to_string()
                );
            }
            assert_eq!(value.compare(&other, || query.check())?, left.cmp(&right));
        }
    }
    for text in [
        "0",
        "-1",
        "18446744073709551616",
        "340282366920938463463374607431768211456",
    ] {
        let value = BignumValue::parse(text, || query.check())?;
        assert_eq!(value.to_decimal(|| query.check())?, text);
        assert_eq!(
            BignumValue::from_native(&value.to_native(|| query.check())?, || query.check())?,
            value
        );
    }
    for length in [1, 2, 8, 9, 10, 100, 1000, 10000] {
        let nines = "9".repeat(length);
        let value = BignumValue::parse(&nines, || query.check())?;
        let successor = value.add(&BignumValue::from_u128(1), || query.check())?;
        assert_eq!(
            successor.to_decimal(|| query.check())?,
            format!("1{}", "0".repeat(length))
        );
        assert_eq!(
            successor.add(&value.negated(), || query.check())?,
            BignumValue::from_u128(1)
        );
        assert_eq!(
            BignumValue::from_native(&successor.to_native(|| query.check())?, || query.check())?,
            successor
        );
    }
    let zero = BignumValue::from_u128(0);
    let negative_zero = BignumValue::from_f64(-0.5)?;
    assert_eq!(negative_zero.to_string(), "-0");
    assert_ne!(negative_zero, zero);
    assert!(negative_zero.compare(&zero, || query.check())?.is_lt());
    assert!(negative_zero.to_f64(|| query.check())?.is_sign_negative());
    assert_eq!(
        negative_zero.to_native(|| query.check())?,
        vec![0x7f, 0xff, 0xfe, 0xff]
    );
    assert_eq!(zero.to_native(|| query.check())?, vec![0x80, 0, 1, 0]);
    assert_eq!(negative_zero.negated(), zero);
    assert_eq!(
        negative_zero.add(&negative_zero, || query.check())?,
        negative_zero
    );
    assert_eq!(negative_zero.add(&zero, || query.check())?, zero);
    for (text, expected) in [
        ("-0", "0"),
        (".5", "1"),
        ("-.5", "-1"),
        ("2.5", "3"),
        ("0.", "0"),
        ("0.400000000000000000001", "1"),
        ("0.400000000000000000000", "0"),
    ] {
        assert_eq!(
            BignumValue::parse(text, || query.check())?.to_string(),
            expected
        );
    }
    for bad in ["", "+", "-", ".", "1e3", " 1", "1 ", "1_0", "12🦆", "1.2.3"] {
        assert!(BignumValue::parse(bad, || query.check()).is_err(), "{bad}");
    }
    for bytes in [
        &[][..],
        &[0x80, 0, 1],
        &[0x80, 0, 2, 1],
        &[0x80, 0, 2, 0, 1],
        &[0x7f, 0xff, 0xfd, 0xff, 0xfe],
    ] {
        assert!(BignumValue::from_native(bytes, || query.check()).is_err());
    }
    assert!(BignumValue::from_parts(false, vec![]).is_err());
    assert!(BignumValue::from_parts(false, vec![1, 0]).is_err());
    assert!(matches!(
        BignumValue::parse("123", || Err(Error::Interrupted)),
        Err(Error::Interrupted)
    ));
    let bound = CastRegistry::builtins().bind(
        &DataType::Varchar,
        &DataType::Bignum,
        CastMode::Explicit,
        &builtin_types(),
    )?;
    let flat = Vector::flat(
        DataType::Varchar,
        vec![
            Value::Varchar("1".into()),
            Value::Varchar("-1".into()),
            Value::Null,
            Value::Varchar("340282366920938463463374607431768211456".into()),
        ],
    )?;
    for vector in [
        flat.clone(),
        Arc::new(flat).select(vec![3, 1, 1, 0, 2])?,
        Vector::constant(DataType::Varchar, Value::Varchar("123".into()), 32)?,
    ] {
        assert_eq!(
            bound
                .apply_batch(&vector, &query)?
                .values()
                .cloned()
                .collect::<Vec<_>>(),
            vector
                .values()
                .map(|v| bound.apply(v, &query))
                .collect::<Result<Vec<_>>>()?
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn bignum_casts_parameters_comparisons_keys_and_native_mutations_reopen() -> Result<()> {
    for expressions in [
        Arc::new(duckdb_rust::execution::expression_executor::ScalarEvaluator)
            as Arc<dyn duckdb_rust::execution::expression_executor::ExpressionEvaluator>,
        Arc::new(duckdb_rust::execution::expression_executor::BatchedEvaluator),
    ] {
        let mut c = DatabaseBuilder::new()
            .expressions(expressions)
            .batch_size(2)
            .build()?
            .connect();
        assert_eq!(c.query("SELECT '1.5'::BIGNUM,(-0.5::DOUBLE)::BIGNUM,typeof(1::VARINT),TRY_CAST('1e3' AS BIGNUM),TRY_CAST(1::DECIMAL(3,0) AS BIGNUM),'-1'::BIGNUM::UTINYINT,'340282366920938463463374607431768211456'::BIGNUM::UTINYINT")?.rows,
            vec![vec![number("2"),BignumValue::from_f64(-0.5)?.value(),Value::Varchar("BIGNUM".into()),Value::Null,Value::Null,Value::Unsigned(255),Value::Unsigned(0)]]);
        assert!(matches!(
            c.query("SELECT 1::BIGNUM::HUGEINT"),
            Err(Error::OutOfRange(_))
        ));
        assert!(matches!(
            c.query("SELECT TRY_CAST(1::BIGNUM AS HUGEINT)"),
            Err(Error::Internal(_))
        ));
        c.execute("CREATE TABLE t(k BIGNUM PRIMARY KEY, v BIGNUM DEFAULT '1.5'); INSERT INTO t(k) VALUES ('340282366920938463463374607431768211456'),(0),((-0.5::DOUBLE)::BIGNUM),(-1)")?;
        assert_eq!(
            c.query("SELECT k FROM t ORDER BY k")?.rows,
            vec![
                vec![number("-1")],
                vec![BignumValue::from_f64(-0.5)?.value()],
                vec![number("0")],
                vec![number("340282366920938463463374607431768211456")]
            ]
        );
        assert_eq!(
            c.execute_params(
                "SELECT k FROM t WHERE k=$1",
                &[number("340282366920938463463374607431768211456")]
            )?[0]
                .rows,
            vec![vec![number("340282366920938463463374607431768211456")]]
        );
        assert_eq!(
            c.query("SELECT count(*) FROM t a JOIN t b ON a.k=b.k")?
                .rows,
            vec![vec![Value::Integer(4)]]
        );
        assert_eq!(
            c.query("SELECT min(k),max(k),count(DISTINCT k) FROM t")?
                .rows,
            vec![vec![
                number("-1"),
                number("340282366920938463463374607431768211456"),
                Value::Integer(4)
            ]]
        );
    }
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("bignum.duckdb");
    {
        let mut c = Database::open_logged(&path)?.connect();
        c.execute("CREATE TABLE t(k BIGNUM PRIMARY KEY,v BIGNUM DEFAULT '1.5'); INSERT INTO t(k) VALUES ((-0.5::DOUBLE)::BIGNUM),(0),(1)")?;
        c.execute_params("UPDATE t SET v=$1 WHERE k=1", &[number(&"9".repeat(10000))])?;
        c.execute("BEGIN; DELETE FROM t; ROLLBACK; BEGIN")?;
        assert!(c.execute("UPDATE t SET k=0").is_err());
        c.execute("ROLLBACK")?;
    }
    for checkpoint in [false, true] {
        let mut c = Database::open_logged(&path)?.connect();
        assert_eq!(
            c.query("SELECT k,v FROM t ORDER BY k")?.rows,
            vec![
                vec![BignumValue::from_f64(-0.5)?.value(), number("2")],
                vec![number("0"), number("2")],
                vec![number("1"), number(&"9".repeat(10000))]
            ]
        );
        if checkpoint {
            c.execute("CHECKPOINT")?;
        }
    }
    assert_eq!(
        Database::open_read_only(&path)?
            .connect()
            .query("SELECT count(*) FROM t")?
            .rows,
        vec![vec![Value::Integer(3)]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn exact_bignum_operators_functions_grouping_and_windows_retain_bound_types() -> Result<()> {
    use duckdb_rust::common::vector::DataChunk;
    use duckdb_rust::function::operator::{Operator, OperatorRegistry};
    let query = QueryContext::background();
    let types = builtin_types();
    let operators = OperatorRegistry::builtins();
    let large = "340282366920938463463374607431768211456";
    for operator in [Operator::Add, Operator::Subtract, Operator::Negate] {
        let operation =
            operators.bind(operator, &vec![DataType::Bignum; operator.arity()], &types)?;
        let flat = Vector::flat(
            DataType::Bignum,
            vec![number(large), number("-1"), Value::Null, number("0")],
        )?;
        for vector in [
            flat.clone(),
            Arc::new(flat).select(vec![3, 0, 1, 0, 2])?,
            Vector::constant(DataType::Bignum, number(large), 33)?,
        ] {
            let chunk = DataChunk::new(vec![vector.clone(); operator.arity()], vector.len())?;
            assert_eq!(
                operation
                    .apply_batch(&chunk, &query)?
                    .values()
                    .cloned()
                    .collect::<Vec<_>>(),
                vector
                    .values()
                    .map(|value| operation.apply(&vec![value.clone(); operator.arity()], &query))
                    .collect::<Result<Vec<_>>>()?
            );
        }
    }
    for expressions in [
        Arc::new(duckdb_rust::execution::expression_executor::ScalarEvaluator)
            as Arc<dyn duckdb_rust::execution::expression_executor::ExpressionEvaluator>,
        Arc::new(duckdb_rust::execution::expression_executor::BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(duckdb_rust::optimizer::IdentityOptimizer)
                as Arc<dyn duckdb_rust::optimizer::Optimizer>,
            Arc::new(duckdb_rust::optimizer::PipelineOptimizer::default()),
        ] {
            let mut c = DatabaseBuilder::new()
                .expressions(expressions.clone())
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            assert_eq!(c.query("SELECT '340282366920938463463374607431768211455'::BIGNUM+1,1::BIGNUM+1.5::FLOAT,1::BIGNUM+1.5::DOUBLE,1::BIGNUM+1.5::DECIMAL(3,1),typeof(+(1::BIGNUM)),typeof(-(1::BIGNUM)),typeof(1::BIGNUM*2::BIGNUM),typeof(abs(1::BIGNUM)),typeof(round(1::BIGNUM)),typeof(trunc(1::BIGNUM)),sqrt(4::BIGNUM)")?.rows,
                vec![vec![number(large),number("2"),Value::Double(2.5),Value::Double(2.5),Value::Varchar("DOUBLE".into()),Value::Varchar("BIGNUM".into()),Value::Varchar("DOUBLE".into()),Value::Varchar("DOUBLE".into()),Value::Varchar("DOUBLE".into()),Value::Varchar("DOUBLE".into()),Value::Double(2.0)]]);
            assert_eq!(c.query("SELECT hex(1::BIGNUM),to_hex(-256::BIGNUM),bin(-1::BIGNUM),to_binary(1::BIGNUM),bin('a'),bin(-1::TINYINT),bin('340282366920938463463374607431768211455'::UHUGEINT)")?.rows,
                vec![vec![Value::Varchar("80000101".into()),Value::Varchar("7FFFFDFEFF".into()),Value::Varchar("01111111111111111111111011111110".into()),Value::Varchar("10000000000000000000000100000001".into()),Value::Varchar("01100001".into()),Value::Varchar("1".repeat(64)),Value::Varchar("1".repeat(128))]]);
            assert!(matches!(
                c.query("SELECT coalesce(1::BIGNUM,1.5::DECIMAL(3,1))"),
                Err(Error::Bind(_))
            ));
            assert!(matches!(
                c.query("SELECT 1::BIGNUM=1.5::DECIMAL(3,1)"),
                Err(Error::Conversion(_))
            ));
            c.execute("CREATE TABLE b(k INTEGER PRIMARY KEY,g INTEGER,v BIGNUM); INSERT INTO b VALUES (1,0,'340282366920938463463374607431768211456'),(2,0,1),(3,0,-1),(4,1,(-0.5::DOUBLE)::BIGNUM),(5,1,NULL)")?;
            assert_eq!(
                c.query(
                    "SELECT g,sum(v),typeof(sum(v)),count(DISTINCT v) FROM b GROUP BY g ORDER BY g"
                )?
                .rows,
                vec![
                    vec![
                        Value::Integer(0),
                        number(large),
                        Value::Varchar("BIGNUM".into()),
                        Value::Integer(3)
                    ],
                    vec![
                        Value::Integer(1),
                        number("0"),
                        Value::Varchar("BIGNUM".into()),
                        Value::Integer(1)
                    ]
                ]
            );
            assert_eq!(c.query("SELECT sum(v) OVER(PARTITION BY g ORDER BY k ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) FROM b ORDER BY k")?.rows,
                vec![vec![number(large)],vec![number("340282366920938463463374607431768211457")],vec![number("0")],vec![number("0")],vec![number("0")]]);
            assert_eq!(c.query("SELECT sum(DISTINCT v) OVER(PARTITION BY g ORDER BY k ROWS UNBOUNDED PRECEDING) FROM b ORDER BY k")?.rows,
                vec![vec![number(large)],vec![number("340282366920938463463374607431768211457")],vec![number(large)],vec![number("0")],vec![number("0")]]);
            assert_eq!(
                c.query("SELECT sum(v),sum(DISTINCT v),avg(v) FROM b WHERE false")?
                    .rows,
                vec![vec![Value::Null; 3]]
            );
            assert_eq!(
                c.execute_params("SELECT $1+$2", &[number(large), number("1")])?[0].rows,
                vec![vec![number("340282366920938463463374607431768211457")]]
            );
            c.execute("BEGIN; UPDATE b SET v=v+1; ROLLBACK; UPDATE b SET v=v-1 WHERE k=1")?;
            assert_eq!(
                c.query("SELECT v FROM b WHERE k=1")?.rows,
                vec![vec![number("340282366920938463463374607431768211455")]]
            );
        }
    }
    Ok(())
}
