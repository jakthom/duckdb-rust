use std::{io::Read, sync::Arc};

use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    common::{
        BitString,
        cast::{CastMode, CastRegistry},
        type_registry::builtin_types,
        vector::{DataChunk, Vector},
    },
    execution::{
        expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
        index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
        operator::join::{HashJoin, JoinAlgorithm, NestedLoopJoin},
        physical_plan::NativePhysicalPlanner,
    },
    function::operator::{Operator, OperatorRegistry},
    parallel::QueryContext,
    storage::{
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bit(text: &str) -> Value {
    BitString::parse(text, || Ok(())).unwrap().value()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn bit_type_modifiers_keep_development_grammar_and_persist_only_logical_metadata() -> Result<()> {
    use sqlparser::{dialect::PostgreSqlDialect, parser::Parser};

    // The dependency's other dialects retain their numeric precision grammar.
    assert!(Parser::parse_sql(&PostgreSqlDialect {}, "SELECT '1'::BIT(-1)").is_err());
    assert!(Parser::parse_sql(&PostgreSqlDialect {}, "SELECT '1'::BIT(1.0)").is_err());
    assert!(Parser::parse_sql(&PostgreSqlDialect {}, "SELECT '1'::BIT(19)").is_ok());
    let directory = tempfile::tempdir()?;
    for (number, format) in [
        Arc::new(JsonSnapshotFormat) as Arc<dyn SnapshotFormat>,
        Arc::new(DuckDbFormat::default()),
    ]
    .into_iter()
    .enumerate()
    {
        let path = directory.path().join(format!("bit-modifiers-{number}.db"));
        let open = || {
            DatabaseBuilder::new()
                .durability(Arc::new(FileCheckpoint::open(
                    &path,
                    OpenMode::ReadWrite,
                    format.clone(),
                )?))
                .build()
        };
        {
            let mut c = open()?.connect();
            for modifier in [
                "-1",
                "+1",
                "-9223372036854775808",
                "9223372036854775808",
                "1.0",
                "1+2",
                "missing_name",
                "missing_function()",
                "1/0",
                "1,2",
            ] {
                for kind in ["BIT", "BIT VARYING"] {
                    let result = c.query(&format!("SELECT '001'::{kind}({modifier}) AS b"))?;
                    assert_eq!(result.columns[0].data_type, DataType::Bit);
                    assert_eq!(result.rows, vec![vec![bit("001")]]);
                }
            }
            for modifier in ["-1", "+1", "-9223372036854775808", "9223372036854775807"] {
                assert_eq!(
                    c.query(&format!("SELECT '001'::BITSTRING({modifier})"))?
                        .rows,
                    vec![vec![bit("001")]]
                );
            }
            for modifier in ["9223372036854775808", "-9223372036854775809", "1.0", "1,2"] {
                let error = c
                    .query(&format!("SELECT '1'::BITSTRING({modifier})"))
                    .unwrap_err();
                assert!(matches!(error, Error::Bind(_)), "{modifier}: {error}");
            }
            assert!(matches!(c.query("SELECT '1'::BIT()"), Err(Error::Parse(_))));
            c.execute("CREATE TABLE b(k BIT(missing_name) PRIMARY KEY, v BIT VARYING(1/0), s BITSTRING(-1)); INSERT INTO b VALUES ('00','001','101')")?;
            c.execute_params("UPDATE b SET v=$1 WHERE k='00'::BIT", &[bit("111")])?;
            c.execute("BEGIN; UPDATE b SET s='0'; ROLLBACK")?;
        }
        let result = open()?
            .connect()
            .query("SELECT k,v,s FROM b WHERE k='00'::BIT")?;
        assert!(
            result
                .columns
                .iter()
                .all(|column| column.data_type == DataType::Bit)
        );
        assert_eq!(result.rows, vec![vec![bit("00"), bit("111"), bit("101")]]);
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn empty_blob_bit_try_failures_remain_fatal_through_nested_parameters_and_mutations() -> Result<()>
{
    let interrupt = duckdb_rust::parallel::InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 2, 1024)?;
    let cast = CastRegistry::builtins().bind(
        &DataType::Blob,
        &DataType::Bit,
        CastMode::Explicit,
        &builtin_types(),
    )?;
    assert!(matches!(
        cast.apply(&Value::Blob(vec![]), &query),
        Err(Error::Conversion(_))
    ));
    assert!(matches!(
        cast.apply_try(&Value::Blob(vec![]), &query),
        Err(Error::Internal(_))
    ));
    assert_eq!(cast.apply_try(&Value::Null, &query)?, Value::Null);
    assert_eq!(
        cast.apply_try(&Value::Blob(vec![1]), &query)?,
        bit("00000001")
    );
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
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
            for sql in [
                "SELECT TRY_CAST(''::BLOB AS BIT)",
                "SELECT TRY_CAST([''::BLOB] AS BIT[])",
                "SELECT TRY_CAST({'b':''::BLOB} AS STRUCT(b BIT))",
                "SELECT TRY_CAST(b AS BIT) FROM (VALUES (NULL::BLOB),('x'::BLOB),(''::BLOB)) t(b)",
            ] {
                let error = c.query(sql).unwrap_err();
                assert!(matches!(error, Error::Internal(_)), "{sql}: {error}");
                assert!(error.to_string().contains("Cannot cast empty BLOB to BIT"));
            }
            assert_eq!(c.query("SELECT TRY_CAST('x' AS BIT),TRY_CAST('000000000'::BIT AS TINYINT),TRY_CAST(NULL::BLOB AS BIT)")?.rows,vec![vec![Value::Null;3]]);
            assert!(matches!(
                c.execute_params("SELECT TRY_CAST($1 AS BIT)", &[Value::Blob(vec![])]),
                Err(Error::Internal(_))
            ));
            c.execute("CREATE TABLE b(k INTEGER PRIMARY KEY,v BIT); INSERT INTO b VALUES (1,'001'); BEGIN")?;
            assert!(matches!(
                c.execute_params("UPDATE b SET v=TRY_CAST($1 AS BIT)", &[Value::Blob(vec![])]),
                Err(Error::Internal(_))
            ));
            c.execute("ROLLBACK")?;
            assert_eq!(
                c.query("SELECT v FROM b WHERE k=1")?.rows,
                vec![vec![bit("001")]]
            );
        }
    }
    interrupt.interrupt();
    assert!(matches!(
        cast.apply_try(&Value::Blob(vec![]), &query),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn packed_bit_lengths_native_padding_vectors_and_numeric_patterns_are_exact() -> Result<()> {
    assert!(std::mem::size_of::<Value>() <= 32);
    assert!(std::mem::size_of::<DataType>() <= 16);
    let query = QueryContext::background();
    let types = builtin_types();
    let casts = CastRegistry::builtins();
    for length in 1..=257 {
        let text: String = (0..length)
            .map(|i| if i % 3 == 0 { '1' } else { '0' })
            .collect();
        let value = BitString::parse(&text, || query.check())?;
        assert_eq!(value.length(), length);
        assert_eq!(value.to_string(), text);
        let native = value.to_native(|| query.check())?;
        assert_eq!(native.len(), length.div_ceil(8) + 1);
        assert_eq!(native[0] as usize, (8 - length % 8) % 8);
        assert_eq!(BitString::from_native(&native, || query.check())?, value);
        let padded = BitString::from_blob(value.to_blob(|| query.check())?)?;
        assert_eq!(
            padded.to_string(),
            format!("{}{text}", "0".repeat((8 - length % 8) % 8))
        );
        for count in [0, 1, 7, 8, 9, length - 1, length, length + 1] {
            let clipped = count.min(length);
            assert_eq!(
                value.shift(count, true, || query.check())?.to_string(),
                format!("{}{}", &text[clipped..], "0".repeat(clipped))
            );
            assert_eq!(
                value.shift(count, false, || query.check())?.to_string(),
                format!("{}{}", "0".repeat(clipped), &text[..length - clipped])
            );
            assert_eq!(
                value.extend(length + count, || query.check())?.to_string(),
                format!("{}{text}", "0".repeat(count))
            );
        }
        assert_eq!(
            value.count(|| query.check())?,
            text.bytes().filter(|b| *b == b'1').count()
        );
        assert_eq!(
            value.invert(|| query.check())?.invert(|| query.check())?,
            value
        );
        assert_eq!(
            value
                .bitwise(&value, |a, b| a ^ b, || query.check())?
                .to_string(),
            "0".repeat(length)
        );
    }
    assert_eq!(bit(""), bit("0"));
    assert_eq!(bit("xAbC"), bit("101010111100"));
    for bad in ["x", "X12", "0🦆1", " 1", "2"] {
        assert!(BitString::parse(bad, || Ok(())).is_err());
    }
    for bad in [&[][..], &[8, 255], &[7, 1], &[1][..]] {
        assert!(BitString::from_native(bad, || Ok(())).is_err());
    }
    assert!(BitString::from_parts(vec![1], 1).is_err());
    assert!(BitString::from_parts(vec![0], 0).is_err());
    assert_eq!(BitString::from_native(&[0], || Ok(()))?.length(), 0);
    assert!(matches!(
        BitString::parse("1", || Err(Error::Interrupted)),
        Err(Error::Interrupted)
    ));
    for (source, value) in [
        (DataType::TinyInt, Value::Integer(-1)),
        (DataType::HugeInt, Value::Integer(i128::MIN)),
        (DataType::UHugeInt, Value::Unsigned(u128::MAX)),
        (DataType::Float, Value::Float(-1.5)),
        (DataType::Double, Value::Double(f64::INFINITY)),
        (DataType::Boolean, Value::Boolean(true)),
    ] {
        let encoded = casts
            .bind(&source, &DataType::Bit, CastMode::Explicit, &types)?
            .apply(&value, &query)?;
        assert_eq!(
            casts
                .bind(&DataType::Bit, &source, CastMode::Explicit, &types)?
                .apply(&encoded, &query)?,
            value
        );
    }
    assert_eq!(
        bit("11111111").cast(&DataType::TinyInt)?,
        Value::Integer(-1)
    );
    assert_eq!(
        bit("1111111").cast(&DataType::TinyInt)?,
        Value::Integer(127)
    );
    assert!(bit("000000000").cast(&DataType::TinyInt).is_err());
    assert!(matches!(
        Value::Blob(Vec::new()).cast(&DataType::Bit),
        Err(Error::Conversion(_))
    ));
    let values = vec![
        bit("0"),
        bit("00"),
        bit("01"),
        bit("1"),
        bit("10"),
        Value::Null,
    ];
    let bound = types.bind(&DataType::Bit)?;
    let flat = Vector::flat(DataType::Bit, values.clone())?;
    let selected = Arc::new(flat.clone()).select(vec![4, 0, 5, 1, 3, 2, 4])?;
    for vector in [
        flat,
        selected,
        Vector::constant(DataType::Bit, bit("001"), 33)?,
    ] {
        let cast = casts.bind(
            &DataType::Bit,
            &DataType::Varchar,
            CastMode::Explicit,
            &types,
        )?;
        let output = cast.apply_batch(&vector, &query)?;
        assert_eq!(
            output.values().cloned().collect::<Vec<_>>(),
            vector
                .values()
                .map(|v| cast.apply(v, &query))
                .collect::<Result<Vec<_>>>()?
        );
    }
    for (i, a) in values.iter().enumerate() {
        for (j, b) in values.iter().enumerate() {
            let (mut ak, mut bk) = (Vec::new(), Vec::new());
            bound.append_key(a, &mut ak, &query)?;
            bound.append_key(b, &mut bk, &query)?;
            assert_eq!(ak == bk, i == j);
            if !a.is_null() && !b.is_null() {
                assert_eq!(bound.compare(a, b, &query)?, i.cmp(&j));
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn bits_cross_parameters_nested_children_keys_relational_mutation_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut composition = 0;
    for format in [
        Arc::new(JsonSnapshotFormat) as Arc<dyn SnapshotFormat>,
        Arc::new(DuckDbFormat::default()),
    ] {
        for index in [
            Arc::new(HashIndexFactory) as Arc<dyn IndexFactory>,
            Arc::new(BTreeIndexFactory),
        ] {
            for expressions in [
                Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
                Arc::new(BatchedEvaluator),
            ] {
                for join in [
                    Arc::new(HashJoin) as Arc<dyn JoinAlgorithm>,
                    Arc::new(NestedLoopJoin),
                ] {
                    composition += 1;
                    let path = directory.path().join(format!("bits-{composition}.db"));
                    let open = || {
                        DatabaseBuilder::new()
                            .indexes(index.clone())
                            .expressions(expressions.clone())
                            .batch_size(3)
                            .physical_planner(Arc::new(NativePhysicalPlanner::with_joins(vec![
                                join.clone(),
                            ])))
                            .durability(Arc::new(FileCheckpoint::open(
                                &path,
                                OpenMode::ReadWrite,
                                format.clone(),
                            )?))
                            .build()
                    };
                    {
                        let mut c = open()?.connect();
                        c.execute("CREATE TABLE t(k BIT PRIMARY KEY DEFAULT '0', v BIT, d DECIMAL(6,2) DEFAULT 1.25, u UUID DEFAULT '00000000000000000000000000000001'); INSERT INTO t DEFAULT VALUES")?;
                        let insert = c.prepare("INSERT INTO t(k,v) VALUES ($1,$2)")?;
                        c.execute_prepared(&insert, &[bit("00"), bit("111")])?;
                        c.execute_prepared(&insert, &[bit("1"), bit("01")])?;
                        assert!(matches!(
                            c.execute_prepared(&insert, &[bit("0"), Value::Null]),
                            Err(Error::Constraint(_))
                        ));
                        assert_eq!(
                            c.query("SELECT k FROM t ORDER BY k")?.rows,
                            vec![vec![bit("0")], vec![bit("00")], vec![bit("1")]]
                        );
                        assert_eq!(
                            c.query("SELECT count(DISTINCT k),min(k),max(k) FROM t")?
                                .rows,
                            vec![vec![Value::Integer(3), bit("0"), bit("1")]]
                        );
                        c.execute("CREATE TABLE s AS SELECT k FROM t UNION ALL SELECT k FROM t UNION ALL SELECT NULL::BIT")?;
                        assert_eq!(
                            c.query("SELECT count(*) FROM t JOIN s ON t.k=s.k")?.rows,
                            vec![vec![Value::Integer(6)]]
                        );
                        assert_eq!(
                            c.query("SELECT count(*) FROM s GROUP BY k ORDER BY k")?
                                .rows,
                            vec![
                                vec![Value::Integer(2)],
                                vec![Value::Integer(2)],
                                vec![Value::Integer(2)],
                                vec![Value::Integer(1)]
                            ]
                        );
                        assert_eq!(c.query("SELECT lag(k) OVER(ORDER BY k),first_value(k) OVER(ORDER BY k) FROM t ORDER BY k")?.rows,vec![vec![Value::Null,bit("0")],vec![bit("0"),bit("0")],vec![bit("00"),bit("0")]]);
                        c.execute("CREATE TABLE nested_bits(s STRUCT(k BIT,d DECIMAL(6,2)), a BIT[]); INSERT INTO nested_bits VALUES ({'k':'01'::BIT,'d':1.25},['1'::BIT,NULL,'00'::BIT])")?;
                        c.execute("BEGIN; DELETE FROM t; ROLLBACK; BEGIN; UPDATE t SET v='1111'; ROLLBACK")?;
                        assert!(matches!(
                            c.execute("UPDATE t SET v='invalid'"),
                            Err(Error::Conversion(_))
                        ));
                        c.execute(
                            "UPDATE t SET v='10' WHERE k='0'::BIT; DELETE FROM t WHERE k='00'::BIT",
                        )?;
                        assert_eq!(
                            c.execute_params("SELECT v FROM t WHERE k=$1", &[bit("0")])?[0].rows,
                            vec![vec![bit("10")]]
                        );
                    }
                    let mut c = open()?.connect();
                    assert_eq!(
                        c.query("SELECT k,v,d,u FROM t ORDER BY k")?.rows,
                        vec![
                            vec![
                                bit("0"),
                                bit("10"),
                                Value::Decimal {
                                    value: 125,
                                    width: 6,
                                    scale: 2
                                },
                                Value::Uuid(1)
                            ],
                            vec![
                                bit("1"),
                                bit("01"),
                                Value::Decimal {
                                    value: 125,
                                    width: 6,
                                    scale: 2
                                },
                                Value::Uuid(1)
                            ]
                        ]
                    );
                    assert_eq!(
                        c.query("SELECT struct_extract(s,'k'),a[1],a[2],a[3] FROM nested_bits")?
                            .rows,
                        vec![vec![bit("01"), bit("1"), Value::Null, bit("00")]]
                    );
                    assert!(c.execute("INSERT INTO t DEFAULT VALUES").is_err());
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn bit_native_wal_and_overflow_preserve_commits_nulls_and_bit_length() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("bits-wal.duckdb");
    let long = bit(&"10010".repeat(14001));
    {
        let mut c = Database::open_logged(&path)?.connect();
        c.execute("CREATE TABLE t(k BIT PRIMARY KEY DEFAULT '1',v BIT DEFAULT '001'); INSERT INTO t DEFAULT VALUES")?;
        c.execute_params(
            "INSERT INTO t VALUES ('00'::BIT,$1),('0'::BIT,NULL)",
            std::slice::from_ref(&long),
        )?;
        c.execute("BEGIN; DELETE FROM t; ROLLBACK; BEGIN; UPDATE t SET v='0'; ROLLBACK")?;
    }
    for checkpoint in [false, true] {
        let mut c = Database::open_logged(&path)?.connect();
        assert_eq!(
            c.query("SELECT k,v FROM t ORDER BY k")?.rows,
            vec![
                vec![bit("0"), Value::Null],
                vec![bit("00"), long.clone()],
                vec![bit("1"), bit("001")]
            ]
        );
        if checkpoint {
            c.execute("CHECKPOINT")?;
        }
    }
    let mut c = Database::open_read_only(&path)?.connect();
    assert_eq!(
        c.query("SELECT v FROM t WHERE k='00'::BIT")?.rows,
        vec![vec![long]]
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn numeric_bitwise_widths_shifts_and_selected_batches_preserve_full_domains() -> Result<()> {
    let query = QueryContext::background();
    let types = builtin_types();
    let registry = OperatorRegistry::builtins();
    for data_type in [
        DataType::TinyInt,
        DataType::SmallInt,
        DataType::Integer,
        DataType::BigInt,
        DataType::HugeInt,
        DataType::UTinyInt,
        DataType::USmallInt,
        DataType::UInteger,
        DataType::UBigInt,
        DataType::UHugeInt,
    ] {
        let width = data_type
            .integer_bits()
            .or_else(|| data_type.unsigned_bits())
            .unwrap();
        let signed = data_type.is_signed_integer();
        let mask = u128::MAX >> (128 - width);
        let value = |word: u128| {
            if signed {
                Value::Integer(((word as i128) << (128 - width)) >> (128 - width))
            } else {
                Value::Unsigned(word)
            }
        };
        let values = vec![
            value(0),
            value(1),
            value(mask >> 1),
            value(mask),
            Value::Null,
        ];
        for (operation, expected) in [
            (Operator::BitAnd, value(1)),
            (Operator::BitOr, value(mask)),
            (Operator::BitXor, value(mask ^ 1)),
        ] {
            let bound =
                registry.bind(operation, &[data_type.clone(), data_type.clone()], &types)?;
            assert_eq!(bound.apply(&[value(mask), value(1)], &query)?, expected);
            for vector in [
                Vector::flat(data_type.clone(), values.clone())?,
                Arc::new(Vector::flat(data_type.clone(), values.clone())?)
                    .select(vec![3, 0, 4, 1, 2, 3])?,
                Vector::constant(data_type.clone(), value(mask), 17)?,
            ] {
                let right = Vector::constant(data_type.clone(), value(1), vector.len())?;
                let count = vector.len();
                let input = DataChunk::new(vec![vector, right], count)?;
                let output = bound.apply_batch(&input, &query)?;
                for (row, values) in input.rows().enumerate() {
                    assert_eq!(output.get(row).unwrap(), &bound.apply(&values, &query)?);
                }
            }
        }
        let not = registry.bind(Operator::BitNot, std::slice::from_ref(&data_type), &types)?;
        assert_eq!(not.apply(&[value(0)], &query)?, value(mask));
        assert_eq!(not.apply(&[value(mask)], &query)?, value(0));
        let left = registry.bind(
            Operator::ShiftLeft,
            &[data_type.clone(), data_type.clone()],
            &types,
        )?;
        let right = registry.bind(
            Operator::ShiftRight,
            &[data_type.clone(), data_type],
            &types,
        )?;
        let safe_shift = width - if signed { 2 } else { 1 };
        assert_eq!(
            left.apply(&[value(1), value(u128::from(safe_shift))], &query)?,
            value(1_u128 << safe_shift)
        );
        assert!(matches!(
            left.apply(&[value(1), value(u128::from(safe_shift + 1))], &query),
            Err(Error::OutOfRange(_))
        ));
        assert_eq!(
            left.apply(&[value(0), value(u128::from(width))], &query)?,
            value(0)
        );
        assert_eq!(
            right.apply(&[value(mask), value(u128::from(width))], &query)?,
            value(0)
        );
        assert_eq!(
            right.apply(&[value(mask), value(1)], &query)?,
            value(if signed { mask } else { mask >> 1 })
        );
        if signed {
            assert!(matches!(
                left.apply(&[value(mask), value(0)], &query),
                Err(Error::OutOfRange(_))
            ));
            assert!(matches!(
                left.apply(&[value(0), value(mask)], &query),
                Err(Error::OutOfRange(_))
            ));
            assert_eq!(right.apply(&[value(mask), value(mask)], &query)?, value(0));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn bit_functions_aggregates_windows_and_mutations_use_logical_positions() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut c = DatabaseBuilder::new()
            .expressions(expressions)
            .batch_size(2)
            .build()?
            .connect();
        assert_eq!(c.query("SELECT bitstring('101',5),bitstring('101'::BIT,5),bit_length('é'),bit_length('001'::BIT),bit_count('001'::BIT),get_bit('101'::BIT,1),set_bit('101'::BIT,1,1)")?.rows,
            vec![vec![bit("00101"),bit("00101"),Value::Integer(16),Value::Integer(3),Value::Integer(1),Value::Integer(0),bit("111")]]);
        assert_eq!(c.query("SELECT length('001'::BIT),len('001'::BIT),char_length('001'::BIT),character_length('001'::BIT),octet_length('001'::BIT),hex(bitstring_byte_comparable('001'::BIT)),bit_position('001'::BIT,'0001'::BIT),bit_position('11'::BIT,'111'::BIT)")?.rows,
            vec![vec![Value::Integer(3),Value::Integer(3),Value::Integer(3),Value::Integer(3),Value::Integer(1),Value::Varchar("020203".into()),Value::Integer(0),Value::Integer(1)]]);
        assert_eq!(c.query("SELECT '101'::BIT & '011'::BIT,'101'::BIT | '011'::BIT,xor('101'::BIT,'011'::BIT),~'101'::BIT,'101'::BIT << 1,'101'::BIT >> 1,'101'::BIT >> -1,'101'::BIT << 3,bit_count(-1::HUGEINT),bit_count(255::UTINYINT)")?.rows,
            vec![vec![bit("001"),bit("111"),bit("110"),bit("010"),bit("010"),bit("010"),bit("000"),bit("000"),Value::Integer(-128),Value::Integer(8)]]);
        assert_eq!(c.query("SELECT typeof(xor(1::UTINYINT,1)),typeof(xor(1::UTINYINT,1::INTEGER)),typeof(xor(1::UTINYINT,CASE WHEN true THEN 1 ELSE 2 END)),typeof(xor(1::UTINYINT,256)),xor('101'::BIT,'011')")?.rows,
            vec![vec![Value::Varchar("UTINYINT".into()),Value::Varchar("INTEGER".into()),Value::Varchar("INTEGER".into()),Value::Varchar("INTEGER".into()),bit("110")]]);
        assert_eq!(
            c.execute_params("SELECT typeof(xor(1::UTINYINT,$1))", &[Value::Integer(1)])?[0].rows,
            vec![vec![Value::Varchar("INTEGER".into())]]
        );
        c.execute("CREATE TABLE b(k INTEGER PRIMARY KEY,v BIT); INSERT INTO b VALUES (1,'001'),(2,'010'),(3,'111'),(4,NULL)")?;
        assert_eq!(
            c.query("SELECT bit_and(v),bit_or(v),bit_xor(v),bit_xor(DISTINCT v) FROM b")?
                .rows,
            vec![vec![bit("000"), bit("111"), bit("100"), bit("100")]]
        );
        assert_eq!(
            c.query(
                "SELECT bit_xor(v) OVER(ORDER BY k ROWS UNBOUNDED PRECEDING) FROM b ORDER BY k"
            )?
            .rows,
            vec![
                vec![bit("001")],
                vec![bit("011")],
                vec![bit("100")],
                vec![bit("100")]
            ]
        );
        assert_eq!(
            c.query("SELECT bit_xor(v),bit_and(v),bit_or(v) FROM b WHERE false")?
                .rows,
            vec![vec![Value::Null; 3]]
        );
        assert_eq!(
            c.query("SELECT bit_xor('101'::BIT) FROM b")?.rows,
            vec![vec![bit("000")]]
        );
        let update = c.prepare("UPDATE b SET v=set_bit(v,$1,$2) WHERE k=$3")?;
        c.execute_prepared(
            &update,
            &[Value::Integer(1), Value::Integer(1), Value::Integer(1)],
        )?;
        c.execute("BEGIN; UPDATE b SET v=~v; ROLLBACK")?;
        assert_eq!(
            c.query("SELECT v FROM b WHERE k=1")?.rows,
            vec![vec![bit("011")]]
        );
        for sql in [
            "SELECT get_bit('1'::BIT,-1)",
            "SELECT get_bit('1'::BIT,1)",
            "SELECT '1'::BIT << -1",
        ] {
            assert!(matches!(c.query(sql), Err(Error::OutOfRange(_))), "{sql}");
        }
        for sql in [
            "SELECT bitstring('1',0)",
            "SELECT bitstring('1',-1)",
            "SELECT set_bit('1'::BIT,0,2)",
            "SELECT '1'::BIT & '01'::BIT",
            "SELECT bit_and(v) FROM (VALUES ('1'::BIT),('01'::BIT)) t(v)",
        ] {
            assert!(matches!(c.query(sql), Err(Error::InvalidInput(_))), "{sql}");
        }
        for sql in [
            "SELECT bit_count(1::UHUGEINT)",
            "SELECT bit_count(1.0)",
            "SELECT '1'::BIT + '1'::BIT",
        ] {
            assert!(matches!(c.query(sql), Err(Error::Bind(_))), "{sql}");
        }
        for sql in [
            "SELECT bitstring('',1)",
            "SELECT bitstring('xF',9)",
            "SELECT bitstring('2',1)",
        ] {
            assert!(matches!(c.query(sql), Err(Error::Conversion(_))), "{sql}");
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independently_written_bit_columns_children_and_compression_survive_mutation_and_reopen()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    for target in ["release", "development"] {
        for name in ["scalar", "dictionary", "fsst", "dict_fsst"] {
            if target == "release" && name == "dict_fsst" {
                continue; // Development-only codec; its positive fixture follows.
            }
            let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "test/data/duckdb/bit-{target}/bit_{name}.duckdb.gz"
            ));
            let mut bytes = Vec::new();
            flate2::read::GzDecoder::new(std::fs::File::open(source)?).read_to_end(&mut bytes)?;
            let path = directory.path().join(format!("bit-{target}-{name}.duckdb"));
            std::fs::write(&path, &bytes)?;
            let expected = if name == "scalar" {
                [
                    Some("0".to_owned()),
                    Some("1111111".to_owned()),
                    Some("11111111".to_owned()),
                    Some("100000000".to_owned()),
                    None,
                    Some("10010".repeat(14001)),
                    Some("001".to_owned()),
                ]
                .into_iter()
                .enumerate()
                .map(|(i, text)| {
                    vec![
                        Value::Integer(i as i128),
                        text.map_or(Value::Null, |s| bit(&s)),
                    ]
                })
                .collect::<Vec<_>>()
            } else {
                (0..10013)
                    .map(|i| {
                        vec![
                            Value::Integer(i),
                            if i % 11 == 0 {
                                Value::Null
                            } else {
                                bit(&format!("{}1", "01".repeat((i % 37) as usize + 1)))
                            },
                        ]
                    })
                    .collect::<Vec<_>>()
            };
            {
                let mut c = Database::open_read_only(&path)
                    .unwrap_or_else(|error| panic!("{target}/{name}: {error}"))
                    .connect();
                let result = c.query("SELECT id,b FROM t ORDER BY id")?;
                assert_eq!(result.columns[1].data_type, DataType::Bit);
                assert_eq!(result.rows, expected, "{target}/{name}");
                if name == "scalar" {
                    assert_eq!(c.query("SELECT xs[1],xs[2],xs[3],struct_extract(s,'b'),struct_extract(s,'d') FROM t WHERE id=0")?.rows,
                        vec![vec![bit("1"),Value::Null,bit("001"),bit("01"),Value::Decimal{value:125,width:8,scale:2}]]);
                    assert_eq!(
                        c.query(
                            "SELECT xs IS NULL,s IS NULL FROM t WHERE id IN (1,2) ORDER BY id"
                        )?
                        .rows,
                        vec![
                            vec![Value::Boolean(false), Value::Boolean(false)],
                            vec![Value::Boolean(true), Value::Boolean(true)]
                        ]
                    );
                }
            }
            assert_eq!(
                std::fs::read(&path)?,
                bytes,
                "read-only open changed C++ fixture"
            );
            {
                let mut c = Database::open(&path)?.connect();
                c.execute("BEGIN; UPDATE t SET b='0'; DELETE FROM t; ROLLBACK")?;
                assert_eq!(c.query("SELECT id,b FROM t ORDER BY id")?.rows, expected);
                c.execute("UPDATE t SET b='101' WHERE id=1; DELETE FROM t WHERE id=2")?;
            }
            let mut after = expected;
            after[1][1] = bit("101");
            after.remove(2);
            let mut c = Database::open_read_only(&path)?.connect();
            assert_eq!(
                c.query("SELECT id,b FROM t ORDER BY id")?.rows,
                after,
                "{target}/{name} mutation checkpoint"
            );
        }
    }
    Ok(())
}
