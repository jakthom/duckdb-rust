use super::*;
use duckdb_rust::{
    DatabaseBuilder,
    common::{
        cast::{CastFunction, CastSpec},
        vector::Vector,
    },
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::{FunctionRegistry, ScalarBindArguments, ScalarFunction},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    storage::{
        checkpoint::FileCheckpoint,
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::{JsonSnapshotFormat, SnapshotFormat},
    },
};

struct Arguments(DataType, i32);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarBindArguments for Arguments {
    fn len(&self) -> usize {
        2
    }
    fn data_type(&self, index: usize) -> Result<DataType> {
        match index {
            0 => Ok(self.0.clone()),
            1 => Ok(DataType::Integer),
            _ => Err(Error::Bind("argument index".into())),
        }
    }
    fn constant(&self, index: usize) -> Result<Value> {
        self.data_type(index)?;
        if index == 1 {
            Ok(Value::Integer(i128::from(self.1)))
        } else {
            Err(Error::Bind("column".into()))
        }
    }
    fn constant_if_closed(&self, index: usize) -> Result<Option<Value>> {
        self.data_type(index)?;
        if index == 1 {
            self.constant(index).map(Some)
        } else {
            Ok(None)
        }
    }
    fn constant_as(&self, index: usize, target: &DataType, mode: CastMode) -> Result<Value> {
        assert_eq!(*target, DataType::Integer);
        assert_eq!(mode, CastMode::Implicit);
        self.constant(index)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bind(
    function: &Arc<dyn ScalarFunction>,
    ty: DataType,
    precision: i32,
) -> Result<Arc<dyn ScalarFunction>> {
    Ok(function
        .bind(&Arguments(ty, precision), &QueryContext::background())?
        .unwrap())
}

// Independent text-digit rounding oracle: split retained and discarded digits,
// inspect the first dropped digit and the tail, then restore requested zeros.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn magnitude(value: u128, drop: u32, zeros: u32, name: &str) -> Option<u128> {
    if drop > 39 {
        return Some(0);
    }
    let mut digits = value.to_string();
    if drop as usize >= digits.len() {
        digits = format!("{:0>width$}", digits, width = drop as usize + 1);
    }
    let split = digits.len() - drop as usize;
    let mut retained = digits[..split].parse::<u128>().unwrap();
    if drop != 0 && name != "trunc" {
        let removed = &digits.as_bytes()[split..];
        let greater = removed[0] > b'5'
            || (removed[0] == b'5' && removed[1..].iter().any(|digit| *digit != b'0'));
        let tie = removed[0] == b'5' && removed[1..].iter().all(|digit| *digit == b'0');
        if greater || (tie && (name == "round" || retained % 2 != 0)) {
            retained += 1;
        }
    }
    if retained == 0 {
        Some(0)
    } else {
        retained.checked_mul(10_u128.checked_pow(zeros)?)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn precision_rounding_checks_every_decimal_metadata_and_integral_boundary() -> Result<()> {
    let functions = FunctionRegistry::builtins();
    let types = builtin_types();
    let casts = CastRegistry::builtins();
    let query = QueryContext::background();
    for name in ["round", "trunc", "round_even", "roundbankers"] {
        let root = functions.scalar(name)?;
        for width in 1..=38 {
            let limit = 10_i128.pow(u32::from(width));
            for scale in 0..=width {
                let source = DataType::Decimal { width, scale };
                for precision in [
                    i32::MIN,
                    -40,
                    -i32::from(width),
                    -1,
                    0,
                    1,
                    i32::from(scale),
                    40,
                    i32::MAX,
                ] {
                    let bound = bind(&root, source.clone(), precision)?;
                    let arguments =
                        bound.argument_types(&[source.clone(), DataType::Integer], &types)?;
                    let target = bound.return_type(&arguments, &types)?;
                    let carry = name != "trunc"
                        && scale == 0
                        && precision < 0
                        && precision >= -i32::from(width);
                    let result_width = if carry { (width + 1).min(38) } else { width };
                    let result_scale = precision.clamp(0, i32::from(scale)) as u8;
                    assert_eq!(
                        target,
                        DataType::Decimal {
                            width: result_width,
                            scale: result_scale
                        }
                    );
                    let cast = casts.bind(&source, &arguments[0], CastMode::Implicit, &types)?;
                    let constant_zero = precision < 0
                        && if name == "trunc" {
                            precision <= -i32::from(width - scale)
                        } else {
                            precision < -i32::from(width - scale)
                        };
                    let mut values = vec![Value::Null];
                    for coefficient in [
                        -limit + 1,
                        -999,
                        -250,
                        -150,
                        -149,
                        -1,
                        0,
                        1,
                        149,
                        150,
                        250,
                        999,
                        limit - 1,
                    ] {
                        if coefficient.unsigned_abs() < limit as u128 {
                            values.push(decimal(coefficient, width, scale)?);
                        }
                    }
                    let flat = Vector::flat(source.clone(), values)?;
                    for vector in [
                        flat.clone(),
                        Arc::new(flat.clone()).select((0..flat.len()).rev().collect())?,
                        flat.slice(1, flat.len() - 1)?,
                        Vector::constant(source.clone(), decimal(1, width, scale)?, 2)?,
                    ] {
                        for input in vector.values() {
                            let result = bound.evaluate(
                                &[
                                    cast.apply(input, &query)?,
                                    Value::Integer(i128::from(precision)),
                                ],
                                &query,
                            );
                            let expected = if constant_zero {
                                Some(decimal(0, result_width, result_scale)?)
                            } else if input.is_null() {
                                Some(Value::Null)
                            } else {
                                let Value::Decimal { value, .. } = input else {
                                    unreachable!()
                                };
                                let drop = (i64::from(scale)
                                    - i64::from(precision.min(i32::from(scale))))
                                    as u32;
                                magnitude(
                                    value.unsigned_abs(),
                                    drop,
                                    if precision < 0 {
                                        precision.unsigned_abs()
                                    } else {
                                        0
                                    },
                                    name,
                                )
                                .filter(|n| *n < 10_u128.pow(u32::from(result_width)))
                                .map(|n| {
                                    decimal(n as i128 * value.signum(), result_width, result_scale)
                                })
                                .transpose()?
                            };
                            match expected {
                                Some(value) => assert_eq!(
                                    result.unwrap_or_else(|error| panic!(
                                        "{name}, {source}, {precision}, {input}: {error}"
                                    )),
                                    value,
                                    "{name}, {source}, {precision}, {input}"
                                ),
                                None => assert!(
                                    matches!(result, Err(Error::OutOfRange(_))),
                                    "{name}, {source}, {precision}, {input}: {result:?}"
                                ),
                            }
                        }
                    }
                }
            }
        }
        for ty in [
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
            if name != "trunc" && ty.is_unsigned_integer() {
                continue;
            }
            let bits = ty.integer_bits().or_else(|| ty.unsigned_bits()).unwrap();
            let max = if ty.is_unsigned_integer() {
                u128::MAX >> (128 - bits)
            } else {
                (i128::MAX >> (128 - bits)) as u128
            };
            let mut values = vec![Value::Null];
            for raw in [0, 1, 5, 15, 25, 50, max] {
                if raw <= max {
                    values.push(if ty.is_unsigned_integer() {
                        Value::Unsigned(raw)
                    } else {
                        Value::Integer(raw as i128)
                    });
                    if ty.is_signed_integer() {
                        values.push(Value::Integer(-(raw as i128)));
                    }
                }
            }
            if ty.is_signed_integer() {
                values.push(Value::Integer(-((max) as i128) - 1));
            }
            for precision in [i32::MIN, -39, -38, -19, -18, -3, -2, -1, 0, 1, i32::MAX] {
                let bound = bind(&root, ty.clone(), precision)?;
                for input in &values {
                    let actual = bound.evaluate(
                        &[input.clone(), Value::Integer(i128::from(precision))],
                        &query,
                    );
                    let expected = match input {
                        Value::Null => Some(Value::Null),
                        Value::Integer(value) => {
                            let cutoff = if name == "trunc" && bits < 128 {
                                19
                            } else {
                                39
                            };
                            let magnitude = if precision >= 0 {
                                Some(value.unsigned_abs())
                            } else if precision.unsigned_abs() >= cutoff {
                                Some(0)
                            } else {
                                magnitude(
                                    value.unsigned_abs(),
                                    precision.unsigned_abs(),
                                    precision.unsigned_abs(),
                                    name,
                                )
                            };
                            magnitude
                                .and_then(|n| {
                                    if *value < 0 {
                                        (n <= 1_u128 << 127).then_some(n.wrapping_neg() as i128)
                                    } else {
                                        i128::try_from(n).ok()
                                    }
                                })
                                .map(Value::Integer)
                                .filter(|v| v.fits_type(&ty))
                        }
                        Value::Unsigned(value) => {
                            let n = if precision >= 0 {
                                *value
                            } else if precision.unsigned_abs() >= if bits < 128 { 19 } else { 39 } {
                                0
                            } else {
                                magnitude(
                                    *value,
                                    precision.unsigned_abs(),
                                    precision.unsigned_abs(),
                                    name,
                                )
                                .unwrap()
                            };
                            Some(Value::Unsigned(n))
                        }
                        _ => unreachable!(),
                    };
                    match expected {
                        Some(value) => {
                            assert_eq!(
                                actual.unwrap_or_else(|error| panic!(
                                    "{name}, {ty}, {precision}, {input}: {error}"
                                )),
                                value,
                                "{name}, {ty}, {precision}, {input}"
                            )
                        }
                        None => assert!(
                            matches!(actual, Err(Error::OutOfRange(_))),
                            "{name}, {ty}, {precision}, {input}: {actual:?}"
                        ),
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct PrecisionCast;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for PrecisionCast {
    fn name(&self) -> &'static str {
        "selected-rounding-precision"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar
            && spec.target == DataType::Integer
            && spec.mode == CastMode::Explicit
    }
    fn cast(&self, value: &Value, _: &CastSpec, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if value == &Value::Varchar("fatal".into()) {
            return Err(Error::Resource("precision cast resource".into()));
        }
        Ok(Value::Integer(1))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn precision_rounding_selects_casts_contextual_nulls_parameters_and_floating_edges() -> Result<()> {
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
            assert_eq!(c.query("SELECT round(1.25::DECIMAL(4,2),'1'),trunc(-1.25::DECIMAL(4,2),1),round_even(1.25::DECIMAL(4,2),1),roundbankers(1.35::DECIMAL(4,2),1),round(9999::DECIMAL(4,0),-1),trunc(9999::DECIMAL(4,0),-1)")?.rows,vec![vec![decimal(13,4,1)?,decimal(-12,4,1)?,decimal(12,4,1)?,decimal(14,4,1)?,decimal(10000,5,0)?,decimal(9990,4,0)?]]);
            assert_eq!(c.query("SELECT typeof(round(NULL)),typeof(round(NULL::INTEGER,1)),typeof(round(NULL::DECIMAL(4,2),-3)),typeof(trunc(1.25::DECIMAL(4,2),NULL)),typeof(round_even(1.25::DECIMAL(4,2),NULL::INTEGER)),typeof(trunc(NULL::DECIMAL(4,2))),typeof(round(1::UTINYINT,-1)),typeof(round(1::UBIGINT,-1)),typeof(round(1::UHUGEINT,-1))")?.rows,vec![vec!["BIGINT","INTEGER","\"NULL\"","\"NULL\"","\"NULL\"","\"NULL\"","BIGINT","HUGEINT","DOUBLE"].into_iter().map(|s|Value::Varchar(s.into())).collect()]);
            assert_eq!(c.query("SELECT x,round(x,-3),trunc(x,-2),round_even(x,-3),round(NULL::DECIMAL(4,2),i::INTEGER) FROM (VALUES (NULL::DECIMAL(4,2),0),(1.25::DECIMAL(4,2),1)) t(x,i) ORDER BY i")?.rows, vec![vec![Value::Null,decimal(0,4,0)?,decimal(0,4,0)?,decimal(0,4,0)?,Value::Null],vec![decimal(125,4,2)?,decimal(0,4,0)?,decimal(0,4,0)?,decimal(0,4,0)?,Value::Null]]);
            assert_eq!(c.query("SELECT round(125,-1),round_even(125,-1),trunc(-125,-1),trunc(18446744073709551615::UBIGINT,-19),trunc('340282366920938463463374607431768211455'::UHUGEINT,-38)")?.rows,vec![vec![Value::Integer(130),Value::Integer(120),Value::Integer(-120),Value::Unsigned(0),Value::Unsigned(300000000000000000000000000000000000000)]]);
            for name in ["round", "round_even", "roundbankers", "trunc"] {
                for bad in [
                    "1.25::DECIMAL(4,2),1.5",
                    "1.25::DECIMAL(4,2),'1'::VARCHAR",
                    "1.25::DECIMAL(4,2),i::INTEGER",
                    "1.25::DOUBLE,i::BIGINT",
                    "TRUE,1",
                    "'1.25',1",
                    "1,1,1",
                    "",
                ] {
                    assert!(
                        matches!(
                            c.query(&format!("SELECT {name}({bad}) FROM range(2) t(i)")),
                            Err(Error::Bind(_))
                        ),
                        "{name}({bad})"
                    );
                }
            }
            assert!(matches!(
                c.query("SELECT round_even(1)"),
                Err(Error::Bind(_))
            ));
            assert!(matches!(
                c.query("SELECT TRY_CAST(round(127::TINYINT,-1) AS VARCHAR)"),
                Err(Error::OutOfRange(_))
            ));
            assert!(matches!(
                c.query(
                    "SELECT round('99999999999999999999999999999999999999'::DECIMAL(38,0),-38)"
                ),
                Err(Error::OutOfRange(_))
            ));
            assert_eq!(c.query("SELECT round('-0.0'::FLOAT,1)::VARCHAR,trunc('-0.0'::DOUBLE,-1)::VARCHAR,round('inf'::DOUBLE,-1),round_even('nan'::FLOAT,-1),trunc('inf'::DOUBLE,-1)::VARCHAR,round('inf'::DOUBLE,1)::VARCHAR,round(1.25::DOUBLE,-2147483648),trunc(1.25::DOUBLE,-2147483648),round(1.25::DOUBLE,2147483647),trunc(1.25::DOUBLE,2147483647),round_even(2.5::FLOAT,0),round_even(3.5::FLOAT,0)")?.rows,vec![vec![Value::Varchar("-0.0".into()),Value::Varchar("-0.0".into()),Value::Double(0.0),Value::Float(0.0),Value::Varchar("inf".into()),Value::Varchar("inf".into()),Value::Double(0.0),Value::Double(1.25),Value::Double(1.25),Value::Double(1.25),Value::Float(2.0),Value::Float(4.0)]]);
            let prepared = c.prepare("SELECT round($1,$2),round_even($1,$2),trunc($1,$2)")?;
            for (precision, round, even, trunc, width, scale) in [
                (1, 13, 12, 12, 4, 1),
                (0, 1, 1, 1, 4, 0),
                (-1, 0, 0, 0, 4, 0),
            ] {
                assert_eq!(
                    c.execute_prepared(
                        &prepared,
                        &[decimal(125, 4, 2)?, Value::Integer(precision)]
                    )?
                    .rows,
                    vec![vec![
                        decimal(round, width, scale)?,
                        decimal(even, width, scale)?,
                        decimal(trunc, width, scale)?
                    ]]
                );
            }
            assert!(matches!(
                c.execute_prepared(
                    &prepared,
                    &[decimal(125, 4, 2)?, Value::Varchar("1".into())]
                ),
                Err(Error::Bind(_))
            ));
            let mut casts = CastRegistry::builtins();
            casts.replace(
                CastSpec {
                    source: DataType::Varchar,
                    target: DataType::Integer,
                    mode: CastMode::Explicit,
                },
                Arc::new(PrecisionCast),
            )?;
            let mut selected = DatabaseBuilder::new()
                .expressions(expressions.clone())
                .optimizer(optimizer)
                .casts(casts)
                .build()?
                .connect();
            assert_eq!(selected.query("SELECT round(1.25::DECIMAL(4,2),'0'),round_even(1.25::DECIMAL(4,2),'0'),trunc(1.25::DECIMAL(4,2),'0')")?.rows,vec![vec![decimal(13,4,1)?,decimal(12,4,1)?,decimal(12,4,1)?]]);
            assert!(matches!(
                selected.query("SELECT TRY_CAST(round(1.25::DECIMAL(4,2),'fatal') AS VARCHAR)"),
                Err(Error::Resource(_))
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn precision_rounding_crosses_relational_keys_mutations_rollback_wal_and_reopen() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for (format_index, format) in [
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
                .join(format!("rounding-{format_index}-{}.db", expressions.name()));
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
                let db = open()?;
                let mut c = db.connect();
                c.execute("CREATE TABLE t(k DECIMAL(5,1) PRIMARY KEY,d DECIMAL(5,2),i TINYINT,f FLOAT); INSERT INTO t VALUES (round(1.25::DECIMAL(5,2),1),1.25,25,1.25),(round(2.25::DECIMAL(5,2),1),2.25,125,2.25),(3.3,NULL,NULL,NULL)")?;
                let before = c.query("SELECT * FROM t ORDER BY k")?.rows;
                assert!(matches!(
                    c.execute("UPDATE t SET i=round(i,-1)"),
                    Err(Error::OutOfRange(_))
                ));
                assert_eq!(c.query("SELECT * FROM t ORDER BY k")?.rows, before);
                c.execute("BEGIN; UPDATE t SET k=k+10,d=trunc(d,1); ROLLBACK")?;
                assert_eq!(c.query("SELECT * FROM t ORDER BY k")?.rows, before);
                let lookup = c.prepare("SELECT d FROM t WHERE k=round($1,1)")?;
                assert_eq!(
                    c.execute_prepared(&lookup, &[decimal(125, 5, 2)?])?.rows,
                    vec![vec![decimal(125, 5, 2)?]]
                );
                assert_eq!(
                    c.query("SELECT count(*) FROM t a JOIN t b ON round(a.d,1)=round(b.d,1)")?
                        .rows,
                    vec![vec![Value::Integer(2)]]
                );
                assert_eq!(
                    c.query("SELECT trunc(d,-4),count(*) FROM t GROUP BY trunc(d,-4)")?
                        .rows,
                    vec![vec![decimal(0, 5, 0)?, Value::Integer(3)]]
                );
                assert_eq!(c.query("SELECT sum(round(d,1)) OVER (ORDER BY k ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM t ORDER BY k")?.rows,vec![vec![decimal(13,38,1)?],vec![decimal(36,38,1)?],vec![decimal(36,38,1)?]]);
                assert_eq!(c.query("SELECT {'d':round(d,1),'f':[round_even(f,1),NULL]}::VARCHAR FROM t WHERE k=1.3")?.rows,vec![vec![Value::Varchar("{'d': 1.3, 'f': [1.2, NULL]}".into())]]);
                c.execute("UPDATE t SET d=round_even(d,1); DELETE FROM t WHERE k=3.3")?;
                c.execute("CHECKPOINT")?;
            }
            {
                let db = open()?;
                let mut c = db.connect();
                assert_eq!(
                    c.query("SELECT k,d,trunc(i,-1),round(f,1) FROM t ORDER BY k")?
                        .rows,
                    vec![
                        vec![
                            decimal(13, 5, 1)?,
                            decimal(120, 5, 2)?,
                            Value::Integer(20),
                            Value::Float(1.3)
                        ],
                        vec![
                            decimal(23, 5, 1)?,
                            decimal(220, 5, 2)?,
                            Value::Integer(120),
                            Value::Float(2.3)
                        ]
                    ]
                );
            }
        }
    }
    let path = directory.path().join("rounding-wal.db");
    {
        let db = Database::open(&path)?;
        db.connect().execute("CREATE TABLE t(k DECIMAL(5,1) PRIMARY KEY,d DECIMAL(5,2)); INSERT INTO t VALUES (round(1.25::DECIMAL(5,2),1),1.25)")?;
        let mut c = db.connect();
        c.execute("CHECKPOINT")?;
        c.execute("UPDATE t SET d=round_even(d,1); BEGIN; DELETE FROM t; ROLLBACK")?;
    }
    let db = Database::open(&path)?;
    assert_eq!(
        db.connect().query("SELECT k,d FROM t")?.rows,
        vec![vec![decimal(13, 5, 1)?, decimal(120, 5, 2)?]]
    );
    Ok(())
}
