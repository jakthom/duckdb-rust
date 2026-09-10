use std::sync::Arc;

use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    common::{
        BitString,
        cast::{CastMode, CastRegistry},
        type_registry::builtin_types,
        vector::Vector,
    },
    execution::{
        expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
        index::{BTreeIndexFactory, HashIndexFactory, IndexFactory},
        operator::join::{HashJoin, JoinAlgorithm, NestedLoopJoin},
        physical_plan::NativePhysicalPlanner,
    },
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
