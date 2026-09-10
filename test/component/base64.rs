use super::*;
use duckdb_rust::{
    common::cast::{CastFunction, CastSpec},
    function::FunctionRegistry,
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn text(value: &str) -> Value {
    Value::Varchar(value.into())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn base64_preserves_bytes_padding_errors_and_selected_vector_shapes() -> Result<()> {
    let registry = FunctionRegistry::builtins();
    let types = builtin_types();
    let query = QueryContext::background();
    let decode = registry.scalar("from_base64")?;
    for name in ["base64", "to_base64"] {
        let encode = registry.scalar(name)?;
        assert_eq!(
            encode.return_type(&[DataType::Blob], &types)?,
            DataType::Varchar
        );
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("üäabcdef", "w7zDpGFiY2RlZg=="),
        ] {
            let blob = Value::Blob(input.as_bytes().to_vec());
            assert_eq!(
                encode.evaluate(std::slice::from_ref(&blob), &query)?,
                text(expected)
            );
            assert_eq!(decode.evaluate(&[text(expected)], &query)?, blob);
        }
        let bytes = (0..=255).cycle().take(4097).collect::<Vec<u8>>();
        for size in (0..=259).chain([1023, 1024, 1025, 4095, 4096, 4097]) {
            // Independent bit-stream oracle: collect one bit at a time and
            // split sextets, rather than repeating the implementation's words.
            let mut bits = Vec::new();
            for byte in &bytes[..size] {
                bits.extend((0..8).rev().map(|bit| byte >> bit & 1));
            }
            let mut expected = String::new();
            for sextet in bits.chunks(6) {
                let index = sextet
                    .iter()
                    .fold(0, |value, bit| value * 2 + usize::from(*bit))
                    << (6 - sextet.len());
                expected.push(char::from(
                    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"[index],
                ));
            }
            while !expected.len().is_multiple_of(4) {
                expected.push('=');
            }
            let input = Value::Blob(bytes[..size].to_vec());
            assert_eq!(
                encode.evaluate(std::slice::from_ref(&input), &query)?,
                text(&expected)
            );
            assert_eq!(decode.evaluate(&[text(&expected)], &query)?, input);
        }
        let flat = Vector::flat(
            DataType::Blob,
            vec![
                Value::Blob(vec![0, 255]),
                Value::Null,
                Value::Blob(vec![]),
                Value::Blob(vec![255; 2049]),
            ],
        )?;
        for vector in [
            flat.clone(),
            flat.slice(1, 3)?,
            Arc::new(flat).select(vec![3, 1, 0, 2, 3])?,
            Vector::constant(DataType::Blob, Value::Blob(vec![0, 1]), 9)?,
        ] {
            for value in vector.values() {
                let encoded = encode.evaluate(std::slice::from_ref(value), &query)?;
                assert_eq!(decode.evaluate(&[encoded], &query)?, *value);
            }
        }
        assert!(matches!(
            encode.evaluate(&[text("bad")], &query),
            Err(Error::Internal(_))
        ));
    }
    for (input, expected) in [
        ("AR==", vec![1]),
        ("AAB=", vec![0, 0]),
        ("AA=B", vec![0]),
        ("AB=C", vec![0]),
        ("//==", vec![255]),
        ("///=", vec![255, 255]),
        ("AAAAAA=A", vec![0, 0, 0, 0]),
    ] {
        assert_eq!(
            decode.evaluate(&[text(input)], &query)?,
            Value::Blob(expected),
            "{input}"
        );
    }
    for (input, position) in [
        ("=AAA", 0),
        ("A=AA", 1),
        ("AA==AAAA", 2),
        ("AAA=AAAA", 3),
        ("AA=!", 3),
        ("AA-_", 2),
        ("AAA\n", 3),
        ("AAA\0", 3),
        ("üab", 0),
    ] {
        let Err(Error::Conversion(message)) = decode.evaluate(&[text(input)], &query) else {
            panic!("{input:?}");
        };
        assert!(
            message.contains(&format!("position {position}")),
            "{message}"
        );
    }
    for input in ["a", "ab", "abc", "é", "AAAA\n", "AAAA====="] {
        let Err(Error::Conversion(message)) = decode.evaluate(&[text(input)], &query) else {
            panic!("{input:?}");
        };
        assert!(
            message.contains("length must be a multiple of 4"),
            "{message}"
        );
    }
    let interrupted = duckdb_rust::parallel::InterruptHandle::default();
    let context = QueryContext::new(interrupted.clone(), None, 2, 1024)?;
    interrupted.interrupt();
    for (name, value) in [
        ("base64", Value::Null),
        ("to_base64", Value::Blob(vec![0; 4096])),
        ("from_base64", text("bad")),
    ] {
        assert!(matches!(
            registry.scalar(name)?.evaluate(&[value], &context),
            Err(Error::Interrupted)
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct SelectedBlobInput;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for SelectedBlobInput {
    fn name(&self) -> &'static str {
        "base64-selected-blob-input"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar
            && spec.target == DataType::Blob
            && spec.mode == CastMode::Explicit
    }
    fn cast(&self, value: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        match value {
            Value::Varchar(value) if value == "fatal" => {
                Err(Error::Resource("selected base64 input failure".into()))
            }
            Value::Varchar(_) => Ok(Value::Blob(vec![b'X'])),
            _ => Err(Error::Internal("selected base64 input".into())),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn base64_binding_keeps_literals_selected_casts_and_typed_parameters() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut c = DatabaseBuilder::new()
                .expressions(expressions.clone())
                .optimizer(optimizer.clone())
                .batch_size(2)
                .build()?
                .connect();
            assert_eq!(c.query("SELECT base64('a'),to_base64(''),typeof(base64(NULL)),typeof(from_base64(NULL)),from_base64(NULL),hex(from_base64('QQ=='::ENUM('QQ==')))")?.rows,vec![vec![text("YQ=="),text(""),text("VARCHAR"),text("BLOB"),Value::Null,text("41")]]);
            for sql in [
                "SELECT base64('a'::VARCHAR)",
                "SELECT base64(1)",
                "SELECT base64('a'::ENUM('a'))",
                "SELECT from_base64('QQ=='::BLOB)",
                "SELECT base64()",
                "SELECT from_base64('QQ==','QQ==')",
                "SELECT base64(s) FROM (VALUES ('a')) t(s)",
            ] {
                assert!(matches!(c.query(sql), Err(Error::Bind(_))), "{sql}");
            }
            assert_eq!(
                c.execute_params(
                    "SELECT base64($1),from_base64($2)",
                    &[Value::Blob(vec![0, 255]), text("AP8=")]
                )?[0]
                    .rows,
                vec![vec![text("AP8="), Value::Blob(vec![0, 255])]]
            );
            assert!(matches!(
                c.execute_params("SELECT base64($1)", &[text("a")]),
                Err(Error::Bind(_))
            ));
            assert_eq!(
                c.query(
                    "SELECT CASE WHEN false THEN from_base64('bad') ELSE from_base64('QQ==') END"
                )?
                .rows,
                vec![vec![Value::Blob(vec![65])]]
            );
            assert!(matches!(
                c.query("SELECT TRY_CAST(from_base64('bad') AS VARCHAR)"),
                Err(Error::Conversion(_))
            ));
            let mut casts = CastRegistry::builtins();
            casts.replace(
                CastSpec {
                    source: DataType::Varchar,
                    target: DataType::Blob,
                    mode: CastMode::Explicit,
                },
                Arc::new(SelectedBlobInput),
            )?;
            let mut selected = DatabaseBuilder::new()
                .casts(casts)
                .expressions(expressions.clone())
                .optimizer(optimizer)
                .build()?
                .connect();
            assert_eq!(
                selected
                    .query("SELECT base64('a'),to_base64('b'),base64(encode('a'))")?
                    .rows,
                vec![vec![text("WA=="), text("WA=="), text("YQ==")]]
            );
            assert!(matches!(
                selected.query("SELECT TRY_CAST(base64('fatal') AS VARCHAR)"),
                Err(Error::Resource(_))
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn base64_values_cross_keys_joins_groups_windows_mutations_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for (index, format) in [
        Arc::new(JsonSnapshotFormat) as Arc<dyn SnapshotFormat>,
        Arc::new(DuckDbFormat::default()),
    ]
    .into_iter()
    .enumerate()
    {
        for expressions in [
            Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
            Arc::new(BatchedEvaluator),
        ] {
            let path = directory
                .path()
                .join(format!("base64-{index}-{}.db", expressions.name()));
            let open = || {
                DatabaseBuilder::new()
                    .expressions(expressions.clone())
                    .batch_size(2)
                    .durability(Arc::new(FileCheckpoint::open(
                        &path,
                        OpenMode::ReadWrite,
                        format.clone(),
                    )?))
                    .build()
            };
            {
                let mut c = open()?.connect();
                c.execute("CREATE TABLE t(k BLOB PRIMARY KEY DEFAULT from_base64('AA=='),s VARCHAR DEFAULT to_base64('A'::BLOB),n BLOB[]); INSERT INTO t DEFAULT VALUES; INSERT INTO t VALUES (from_base64('AP8='),'AP8=',[from_base64('AA=='),NULL,from_base64('AP8=')]),(from_base64('AAE='),'invalid',[from_base64('AAE=')]); BEGIN; DELETE FROM t; ROLLBACK")?;
                assert!(matches!(
                    c.execute("UPDATE t SET k=from_base64(s)"),
                    Err(Error::Conversion(_))
                ));
                assert!(c.execute("UPDATE t SET k=from_base64('AA==')").is_err());
                c.execute("BEGIN; UPDATE t SET s=base64(k); ROLLBACK")?;
                assert_eq!(
                    c.query("SELECT s FROM t ORDER BY k")?.rows,
                    vec![
                        vec![text("QQ==")],
                        vec![text("invalid")],
                        vec![text("AP8=")]
                    ]
                );
                assert_eq!(c.query("SELECT hex(a.k),count(*),min(base64(a.k)) FROM t a JOIN t b ON a.k=from_base64(base64(b.k)) GROUP BY a.k ORDER BY a.k")?.rows,vec![vec![text("00"),Value::Integer(1),text("AA==")],vec![text("0001"),Value::Integer(1),text("AAE=")],vec![text("00FF"),Value::Integer(1),text("AP8=")]]);
                assert_eq!(c.query("SELECT first_value(base64(k)) OVER(ORDER BY k),base64(lag(k) OVER(ORDER BY k)) FROM t ORDER BY k")?.rows,vec![vec![text("AA=="),Value::Null],vec![text("AA=="),text("AA==")],vec![text("AA=="),text("AAE=")]]);
                c.execute("UPDATE t SET s=base64(k); CHECKPOINT")?;
            }
            let mut c = open()?.connect();
            let p = c.prepare("SELECT hex(k),s FROM t WHERE k=from_base64($1)")?;
            assert_eq!(
                c.execute_prepared(&p, &[text("AP8=")])?.rows,
                vec![vec![text("00FF"), text("AP8=")]]
            );
            assert_eq!(c.query("SELECT base64(n[1]),base64(n[2]),base64(n[3]) FROM t WHERE k=from_base64('AP8=')")?.rows,vec![vec![text("AA=="),Value::Null,text("AP8=")]]);
        }
    }
    let path = directory.path().join("base64-wal.duckdb");
    let bytes = (0..=255).cycle().take(300007).collect::<Vec<_>>();
    {
        let mut c = Database::open(&path)?.connect();
        c.execute("CREATE TABLE t(k INTEGER PRIMARY KEY,b BLOB,s VARCHAR); INSERT INTO t VALUES (1,from_base64('AA=='),base64('A'::BLOB))")?;
        c.execute_params(
            "UPDATE t SET b=from_base64(base64($1)),s=to_base64($1)",
            &[Value::Blob(bytes.clone())],
        )?;
        c.execute("BEGIN; DELETE FROM t; ROLLBACK")?;
    }
    let mut c = Database::open(&path)?.connect();
    assert_eq!(
        c.query("SELECT b,from_base64(s) FROM t")?.rows,
        vec![vec![Value::Blob(bytes.clone()), Value::Blob(bytes)]]
    );
    c.execute("CHECKPOINT")?;
    drop(c);
    assert_eq!(
        Database::open(&path)?
            .connect()
            .query("SELECT base64(b)=s FROM t")?
            .rows,
        vec![vec![Value::Boolean(true)]]
    );
    Ok(())
}
