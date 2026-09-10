use duckdb_rust::{
    DataType, Database, DatabaseBuilder, Error, Result, Value,
    common::{
        cast::{CastMode, CastRegistry, CastSpec, numeric::ExactNumericCast},
        type_registry::builtin_types,
        vector::Vector,
    },
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::QueryContext,
    storage::{
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
    },
};
use std::sync::Arc;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn text(value: &str) -> Value {
    Value::Varchar(value.into())
}

#[derive(serde::Deserialize)]
struct RawTextOracle {
    bits: String,
    cpp: String,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_pinned_raw_bit_oracles_include_all_exponent_boundaries_and_formatter_quirks()
-> Result<()> {
    let types = builtin_types();
    let casts = CastRegistry::builtins();
    let query = QueryContext::background();
    for reference in ["release", "development"] {
        for (source, name, count) in [
            (DataType::Float, "float", 6536),
            (DataType::Double, "double", 17288),
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "test/data/floating-text-production/{reference}-{name}.json.gz"
            ));
            let records: Vec<RawTextOracle> =
                serde_json::from_reader(flate2::read::GzDecoder::new(std::fs::File::open(path)?))
                    .map_err(|error| Error::Internal(format!("floating oracle fixture: {error}")))?;
            assert_eq!(records.len(), count);
            let bound = casts.bind(&source, &DataType::Varchar, CastMode::Explicit, &types)?;
            for record in records {
                let bits = u64::from_str_radix(&record.bits, 16).unwrap();
                let value = if source == DataType::Float {
                    Value::Float(f32::from_bits(u32::try_from(bits).unwrap()))
                } else {
                    Value::Double(f64::from_bits(bits))
                };
                // Expected text comes only from the independent C++ producer;
                // the report's diagnostic Rust column is not deserialized.
                assert_eq!(
                    bound.apply(&value, &query)?,
                    text(&record.cpp),
                    "{reference} {source} 0x{}",
                    record.bits
                );
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct SelectedDoubleText;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::common::cast::CastFunction for SelectedDoubleText {
    fn name(&self) -> &'static str {
        "selected-double-text-regression"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Double
            && spec.target == DataType::Varchar
            && spec.mode != CastMode::Implicit
    }
    fn cast(&self, value: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        match value {
            Value::Double(value) if *value == 9.0 => {
                Err(Error::Resource("selected child resource failure".into()))
            }
            Value::Double(value) if *value == 8.0 => Ok(Value::Null),
            Value::Double(_) => Ok(text("selected")),
            _ => Err(Error::Internal("selected floating text input".into())),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn nested_text_retains_child_cast_replacement_validation_and_failure_provenance() -> Result<()> {
    let types = builtin_types();
    let query = QueryContext::background();
    let mut casts = CastRegistry::builtins();
    let mut normal = Database::memory()?.connect();
    let input = normal.query("SELECT [1.0::DOUBLE,NULL,-0.0::DOUBLE]")?.rows[0][0].clone();
    let before = casts.bind(
        &input.data_type(),
        &DataType::Varchar,
        CastMode::Explicit,
        &types,
    )?;
    casts.replace(
        CastSpec {
            source: DataType::Double,
            target: DataType::Varchar,
            mode: CastMode::Explicit,
        },
        Arc::new(SelectedDoubleText),
    )?;
    let after = casts.bind(
        &input.data_type(),
        &DataType::Varchar,
        CastMode::Explicit,
        &types,
    )?;
    assert_eq!(before.apply(&input, &query)?, text("[1.0, NULL, -0.0]"));
    assert_eq!(
        after.apply(&input, &query)?,
        text("[selected, NULL, selected]")
    );
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        let mut c = DatabaseBuilder::new()
            .casts(casts.clone())
            .expressions(expressions)
            .batch_size(2)
            .build()?
            .connect();
        assert_eq!(c.query("SELECT [1.0::DOUBLE,NULL]::VARCHAR,{'a':1.0::DOUBLE}::VARCHAR,row(1.0::DOUBLE)::VARCHAR,map([1.0::DOUBLE],[1.0::DOUBLE])::VARCHAR,union_value(a:=1.0::DOUBLE)::VARCHAR,(1.0::DOUBLE)::VARIANT::VARCHAR,concat({'a':[1.0::DOUBLE]}),[1.0::DOUBLE,NULL]::DOUBLE[2]::VARCHAR")?.rows,
            vec![vec![text("[selected, NULL]"),text("{'a': selected}"),text("(selected,)"),text("{selected=selected}"),text("selected"),text("selected"),text("{'a': [selected]}"),text("[selected, NULL]")]]);
        for sql in [
            "SELECT [9.0::DOUBLE]::VARCHAR",
            "SELECT TRY_CAST([9.0::DOUBLE] AS VARCHAR)",
            "SELECT TRY_CAST({'a':9.0::DOUBLE} AS VARCHAR)",
            "SELECT TRY_CAST(union_value(a:=9.0::DOUBLE) AS VARCHAR)",
            "SELECT TRY_CAST((9.0::DOUBLE)::VARIANT AS VARCHAR)",
        ] {
            assert!(matches!(c.query(sql), Err(Error::Resource(_))), "{sql}");
        }
        for sql in [
            "SELECT [8.0::DOUBLE]::VARCHAR",
            "SELECT TRY_CAST({'a':8.0::DOUBLE} AS VARCHAR)",
        ] {
            assert!(matches!(c.query(sql), Err(Error::Internal(_))), "{sql}");
        }
        assert_eq!(
            c.execute_params(
                "SELECT [$1]::VARCHAR,concat({'a':$1})",
                &[Value::Double(2.0)]
            )?[0]
                .rows,
            vec![vec![text("[selected]"), text("{'a': selected}")]]
        );
    }
    let interrupted = duckdb_rust::parallel::InterruptHandle::default();
    let context = QueryContext::new(interrupted.clone(), None, 2, 1024)?;
    interrupted.interrupt();
    assert!(matches!(
        after.apply_try(&input, &context),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn selected_floating_text_casts_keep_shortest_midpoint_fallback_and_vector_semantics() -> Result<()>
{
    let types = builtin_types();
    let query = QueryContext::background();
    for alternate in [false, true] {
        for (source, cases) in [
            (
                DataType::Float,
                vec![
                    (Value::Float(0.0), "0.0"),
                    (Value::Float(-0.0), "-0.0"),
                    (Value::Float(1.0), "1.0"),
                    (Value::Float(1e16), "1e+16"),
                    (Value::Float(1e-5), "1e-05"),
                    (Value::Float(f32::from_bits(0xca2454ff)), "-2692415.75"),
                    (Value::Float(f32::from_bits(0xc9e83812)), "-1902338.25"),
                    (Value::Float(f32::from_bits(0x4cd3aa38)), "110973376.0"),
                    (Value::Float(f32::from_bits(0xcd1889c2)), "-159947808.0"),
                    (Value::Float(f32::from_bits(0x44834f00)), "1050.46875"),
                    (Value::Float(f32::INFINITY), "inf"),
                    (Value::Float(f32::NEG_INFINITY), "-inf"),
                ],
            ),
            (
                DataType::Double,
                vec![
                    (Value::Double(0.0), "0.0"),
                    (Value::Double(-0.0), "-0.0"),
                    (Value::Double(1.0), "1.0"),
                    (Value::Double(1e15), "1000000000000000.0"),
                    (Value::Double(1e16), "1e+16"),
                    (Value::Double(1e-4), "0.0001"),
                    (Value::Double(1e-5), "1e-05"),
                    (Value::Double(1e-6), "1e-06"),
                    (
                        Value::Double(f64::from_bits(0x42eaa13992d343a4)),
                        "234238063843869.12",
                    ),
                    (Value::Double(f64::INFINITY), "inf"),
                    (Value::Double(f64::NEG_INFINITY), "-inf"),
                    // Independently confirmed native CLI/source quirks. These
                    // text casts are deliberately NOT numeric round trips.
                    (
                        Value::Double(f64::from_bits(0x4500000000000000)),
                        "4.835703278458517e+24",
                    ),
                    (
                        Value::Double(f64::from_bits(0x45a0000000000000)),
                        "4.951760157141521e+27",
                    ),
                    (
                        Value::Double(f64::from_bits(0x7260000000000000)),
                        "A.070116948172427e+242",
                    ),
                    (
                        Value::Double(f64::from_bits(0xf260000000000000)),
                        "-A.070116948172427e+242",
                    ),
                ],
            ),
        ] {
            for mode in [CastMode::Assignment, CastMode::Explicit] {
                let mut casts = CastRegistry::builtins();
                if alternate {
                    casts.replace(
                        CastSpec {
                            source: source.clone(),
                            target: DataType::Varchar,
                            mode,
                        },
                        Arc::new(ExactNumericCast),
                    )?;
                }
                let bound = casts.bind(&source, &DataType::Varchar, mode, &types)?;
                let mut values = vec![Value::Null];
                for (value, expected) in &cases {
                    assert_eq!(
                        bound.apply(value, &query)?,
                        text(expected),
                        "{value:?}, alternate={alternate}"
                    );
                    values.push(value.clone());
                }
                let flat = Vector::flat(source.clone(), values.clone())?;
                for vector in [
                    flat.clone(),
                    Arc::new(flat).select((0..values.len()).rev().collect())?,
                    Vector::constant(source.clone(), values[2].clone(), 33)?,
                ] {
                    assert_eq!(
                        bound
                            .apply_batch(&vector, &query)?
                            .values()
                            .cloned()
                            .collect::<Vec<_>>(),
                        vector
                            .values()
                            .map(|value| bound.apply(value, &query))
                            .collect::<Result<Vec<_>>>()?
                    );
                }
                // Deterministic raw IEEE coverage checks round-trip identity,
                // on this sampled stream. It is NOT a universal property:
                // pinned power-of-two failures above are explicit exceptions.
                // Independent pinned text equality is a separate comparison.
                let mut word = 0x304d_1029_e7a8_41d3_u64;
                for _ in 0..10000 {
                    word ^= word << 13;
                    word ^= word >> 7;
                    word ^= word << 17;
                    let value = if source == DataType::Float {
                        Value::Float(f32::from_bits(word as u32))
                    } else {
                        Value::Double(f64::from_bits(word))
                    };
                    let rendered = bound.apply(&value, &query)?;
                    let Value::Varchar(rendered) = rendered else {
                        panic!("VARCHAR cast")
                    };
                    match value {
                        Value::Float(value) if value.is_finite() => {
                            assert_eq!(rendered.parse::<f32>().unwrap().to_bits(), value.to_bits())
                        }
                        Value::Double(value) if value.is_finite() => {
                            assert_eq!(rendered.parse::<f64>().unwrap().to_bits(), value.to_bits())
                        }
                        _ => (),
                    }
                }
            }
        }
    }
    // Diagnostic Display is intentionally not the SQL VARCHAR cast contract.
    assert_eq!(Value::Double(1.0).to_string(), "1");
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn floating_text_casts_cross_nested_concat_parameters_keys_mutations_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
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
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            assert_eq!(c.query("SELECT concat(1.0::DOUBLE),concat(-0.0::FLOAT),[1.0::DOUBLE,-0.0::DOUBLE,NULL]::VARCHAR,{'a':1.0::DOUBLE,'b':[1e16::DOUBLE]}::VARCHAR,row(1.0::DOUBLE)::VARCHAR,map([1.0::DOUBLE],[-0.0::DOUBLE])::VARCHAR,union_value(a:=1.0::DOUBLE)::VARCHAR,union_value(a:=NULL::DOUBLE)::VARCHAR,(1.0::DOUBLE)::VARIANT::VARCHAR,[1.0::DOUBLE,-0.0::DOUBLE]::DOUBLE[2]::VARCHAR")?.rows,
                vec![vec![text("1.0"),text("-0.0"),text("[1.0, -0.0, NULL]"),text("{'a': 1.0, 'b': [1e+16]}"),text("(1.0,)"),text("{1.0=-0.0}"),text("1.0"),text("NULL"),text("1.0"),text("[1.0, -0.0]")]]);
            assert_eq!(c.query("SELECT concat([1.0::DOUBLE],[-0.0::DOUBLE])::VARCHAR,concat({'a':1.0::DOUBLE}),concat([1.0::DOUBLE]::VARIANT)::VARCHAR")?.rows,
                vec![vec![text("[1.0, -0.0]"),text("{'a': 1.0}"),text("[1.0]")]]);
            assert_eq!(
                c.execute_params(
                    "SELECT concat($1),[$1]::VARCHAR,{'a':$1}::VARCHAR",
                    &[Value::Double(-0.0)]
                )?[0]
                    .rows,
                vec![vec![text("-0.0"), text("[-0.0]"), text("{'a': -0.0}")]]
            );
            for sql in [
                "SELECT [make_timestamp(-9223372036854775806)]::VARCHAR",
                "SELECT TRY_CAST({'a':make_timestamp(-9223372036854775806)} AS VARCHAR)",
            ] {
                assert!(matches!(c.query(sql), Err(Error::Internal(_))), "{sql}");
            }
        }
    }
    for (index, format) in [
        Arc::new(JsonSnapshotFormat) as Arc<dyn SnapshotFormat>,
        Arc::new(DuckDbFormat::default()),
    ]
    .into_iter()
    .enumerate()
    {
        let path = directory.path().join(format!("floating-text-{index}.db"));
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
            c.execute("CREATE TABLE t(k VARCHAR PRIMARY KEY DEFAULT (1.0::DOUBLE),d DOUBLE DEFAULT 1.0,n DOUBLE[]); INSERT INTO t DEFAULT VALUES; INSERT INTO t VALUES (concat(-0.0::DOUBLE),-0.0::DOUBLE,[-0.0::DOUBLE,1.0::DOUBLE]); BEGIN; UPDATE t SET k=concat(d+2); ROLLBACK")?;
            assert!(c.execute("UPDATE t SET k=concat(1.0::DOUBLE)").is_err());
        }
        {
            let mut c = open()?.connect();
            assert_eq!(
                c.query("SELECT k,d::VARCHAR,n::VARCHAR FROM t ORDER BY k")?
                    .rows,
                vec![
                    vec![text("-0.0"), text("-0.0"), text("[-0.0, 1.0]")],
                    vec![text("1.0"), text("1.0"), Value::Null]
                ]
            );
            assert_eq!(
                c.execute_params("SELECT k FROM t WHERE k=concat($1)", &[Value::Double(-0.0)])?[0]
                    .rows,
                vec![vec![text("-0.0")]]
            );
            c.execute("UPDATE t SET n=[d,1e16] WHERE k='1.0'; CHECKPOINT")?;
        }
        assert_eq!(
            open()?
                .connect()
                .query("SELECT n::VARCHAR FROM t WHERE k='1.0'")?
                .rows,
            vec![vec![text("[1.0, 1e+16]")]]
        );
    }
    // Native WAL is an independent persistence selection from checkpoints.
    let path = directory.path().join("floating-text-wal.duckdb");
    {
        let mut c = Database::open(&path)?.connect();
        c.execute("CREATE TABLE t(k VARCHAR PRIMARY KEY,d DOUBLE); INSERT INTO t VALUES (concat(-0.0::DOUBLE),-0.0::DOUBLE); BEGIN; DELETE FROM t; ROLLBACK")?;
    }
    assert_eq!(
        Database::open(&path)?
            .connect()
            .query("SELECT k,concat(d),[d]::VARCHAR FROM t")?
            .rows,
        vec![vec![text("-0.0"), text("-0.0"), text("[-0.0]")]]
    );
    Ok(())
}
