use super::*;
use duckdb_rust::{
    common::type_registry::{KeyRepresentation, KeyWriter, PrimitiveTypes, ValueValidation},
    parallel::InterruptHandle,
};

struct PartialKey {
    mode: usize,
    interrupt: InterruptHandle,
}

#[derive(Debug)]
struct DecimalIntegerKeys {
    representation: KeyRepresentation,
    writes: Arc<std::sync::atomic::AtomicUsize>,
}

impl TypeAdapter for DecimalIntegerKeys {
    fn name(&self) -> &'static str {
        "decimal-integer-keys"
    }
    fn key_representation(&self, _: &DataType) -> KeyRepresentation {
        self.representation
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(data_type)
    }
    fn validate_value(&self, t: &DataType, v: &Value, q: &QueryContext) -> Result<()> {
        PrimitiveTypes.validate_value(t, v, q)
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(a, b)
    }
    fn compare(&self, t: &DataType, a: &Value, b: &Value, q: &QueryContext) -> Result<Ordering> {
        PrimitiveTypes.compare(t, a, b, q)
    }
    fn write_key(
        &self,
        _: &DataType,
        v: &Value,
        out: &mut KeyWriter<'_>,
        q: &QueryContext,
    ) -> Result<()> {
        q.check()?;
        self.writes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        out.extend_from_slice(v.as_i128()?.to_string().as_bytes())
    }
}

#[test]
fn integer_grouping_respects_the_registered_key_representation() -> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    for representation in [
        KeyRepresentation::CanonicalBytes,
        KeyRepresentation::Integer,
    ] {
        let writes = Arc::new(AtomicUsize::new(0));
        let mut types = TypeRegistry::builtins();
        types.replace(
            DataType::BigInt.family(),
            Arc::new(DecimalIntegerKeys {
                representation,
                writes: writes.clone(),
            }),
        )?;
        let db = DatabaseBuilder::new()
            .types(Arc::new(types))
            .batch_size(3)
            .build()?;
        let mut c = db.connect();
        c.execute("CREATE TABLE t(i BIGINT); INSERT INTO t VALUES(1),(1),(2),(NULL)")?;
        writes.store(0, AtomicOrdering::Relaxed);
        assert_eq!(c.query("SELECT i,count(*),grouping(i) FROM t GROUP BY ROLLUP(i) ORDER BY grouping(i),i NULLS FIRST")?.rows, vec![
            vec![Value::Null,Value::Integer(1),Value::Integer(0)],
            vec![Value::Integer(1),Value::Integer(2),Value::Integer(0)],
            vec![Value::Integer(2),Value::Integer(1),Value::Integer(0)],
            vec![Value::Null,Value::Integer(4),Value::Integer(1)],
        ]);
        assert_eq!(
            writes.load(AtomicOrdering::Relaxed) == 0,
            representation == KeyRepresentation::Integer
        );
    }
    Ok(())
}

#[test]
fn integer_membership_uses_the_selected_key_capability_and_keeps_byte_key_failures() -> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    for representation in [
        KeyRepresentation::CanonicalBytes,
        KeyRepresentation::Integer,
    ] {
        let writes = Arc::new(AtomicUsize::new(0));
        let adapter = Arc::new(DecimalIntegerKeys {
            representation,
            writes: writes.clone(),
        });
        let mut types = TypeRegistry::builtins();
        let retained = types.bind(&DataType::BigInt)?;
        types.replace(DataType::BigInt.family(), adapter.clone())?;
        assert_eq!(retained.key_representation(), KeyRepresentation::Integer);
        assert_eq!(
            types.bind(&DataType::BigInt)?.key_representation(),
            representation
        );
        let db = DatabaseBuilder::new()
            .types(Arc::new(types))
            .batch_size(3)
            .build()?;
        let mut c = db.connect();
        c.execute("CREATE TABLE l(i BIGINT); CREATE TABLE r(i BIGINT); INSERT INTO l VALUES (-1),(0),(1),(1),(2),(NULL); INSERT INTO r VALUES (1),(1),(2),(NULL)")?;
        writes.store(0, AtomicOrdering::Relaxed);
        assert_eq!(
            c.query("SELECT i FROM l WHERE EXISTS(SELECT 1 FROM r WHERE r.i=l.i)")?
                .rows,
            vec![
                vec![Value::Integer(1)],
                vec![Value::Integer(1)],
                vec![Value::Integer(2)]
            ]
        );
        assert_eq!(
            c.query("SELECT i FROM l WHERE NOT EXISTS(SELECT 1 FROM r WHERE r.i=l.i)")?
                .rows,
            vec![
                vec![Value::Integer(-1)],
                vec![Value::Integer(0)],
                vec![Value::Null]
            ]
        );
        assert_eq!(
            writes.load(AtomicOrdering::Relaxed) > 0,
            representation == KeyRepresentation::CanonicalBytes
        );

        if representation == KeyRepresentation::Integer {
            let mut invalid = TypeRegistry::builtins();
            invalid.replace(DataType::Varchar.family(), adapter)?;
            assert!(matches!(
                invalid.bind(&DataType::Varchar),
                Err(Error::Bind(_))
            ));
        }
    }
    // The default representation must keep an adapter's errors; a physically
    // integer column alone is not permission to bypass its key implementation.
    let mut types = TypeRegistry::builtins();
    types.replace(
        DataType::BigInt.family(),
        Arc::new(PartialKey {
            mode: 0,
            interrupt: InterruptHandle::default(),
        }),
    )?;
    let db = DatabaseBuilder::new().types(Arc::new(types)).build()?;
    let mut c = db.connect();
    c.execute("CREATE TABLE l(i BIGINT); CREATE TABLE r(i BIGINT); INSERT INTO l VALUES (1),(2); INSERT INTO r VALUES (1)")?;
    let result = c.query("SELECT i FROM l WHERE EXISTS(SELECT 1 FROM r WHERE r.i=l.i)");
    assert!(
        matches!(
            &result, Err(Error::Execution(message)) if message == "failure after writing"
        ),
        "{result:?}"
    );
    Ok(())
}
impl std::fmt::Debug for PartialKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("partial-key")
    }
}
impl TypeAdapter for PartialKey {
    fn name(&self) -> &'static str {
        "partial-key"
    }
    fn value_validation(&self) -> ValueValidation {
        ValueValidation::Physical
    }
    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(data_type)
    }
    fn validate_value(
        &self,
        data_type: &DataType,
        value: &Value,
        query: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.validate_value(data_type, value, query)
    }
    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(left, right)
    }
    fn compare(
        &self,
        data_type: &DataType,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        PrimitiveTypes.compare(data_type, left, right, query)
    }
    fn write_key(
        &self,
        _: &DataType,
        _: &Value,
        output: &mut KeyWriter<'_>,
        _: &QueryContext,
    ) -> Result<()> {
        output.extend_from_slice(&[1, 2, 3])?;
        match self.mode {
            0 => Err(Error::Execution("failure after writing".into())),
            1 => {
                self.interrupt.interrupt();
                Ok(())
            }
            _ => {
                // Even an adapter that swallows the size failure cannot publish
                // its partial component as a successful, truncated key.
                assert!(matches!(
                    output.extend_from_slice(&vec![0; 16 * 1024 * 1024]),
                    Err(Error::Resource(_))
                ));
                assert!(matches!(output.push(4), Err(Error::Resource(_))));
                Ok(())
            }
        }
    }
}

#[test]
fn key_writer_preserves_prefixes_on_partial_failure_cancellation_and_size_errors() -> Result<()> {
    for mode in 0..3 {
        let interrupt = InterruptHandle::default();
        let query = QueryContext::new(interrupt.clone(), None, 16, 100)?;
        let mut registry = TypeRegistry::builtins();
        registry.replace(
            DataType::BigInt.family(),
            Arc::new(PartialKey {
                mode,
                interrupt: interrupt.clone(),
            }),
        )?;
        let mut output = vec![9, 8, 7];
        let result =
            registry
                .bind(&DataType::BigInt)?
                .append_key(&Value::Integer(1), &mut output, &query);
        assert!(matches!(
            (mode, result),
            (0, Err(Error::Execution(_)))
                | (1, Err(Error::Interrupted))
                | (2, Err(Error::Resource(_)))
        ));
        assert_eq!(output, [9, 8, 7]);
        interrupt.reset();
        let bound = TypeRegistry::builtins().bind(&DataType::BigInt)?;
        bound.append_key(&Value::Integer(1), &mut output, &query)?;
        let retained = output.clone();
        bound.append_key(&Value::Null, &mut output, &query)?;
        assert_eq!(&output[..retained.len()], retained.as_slice());
        assert_eq!(output.last(), Some(&0));
    }
    Ok(())
}

#[test]
fn column_keys_match_scalar_keys_and_validate_before_visiting() -> Result<()> {
    use duckdb_rust::common::vector::Vector;
    let query = QueryContext::background();
    for data_type in [DataType::BigInt, DataType::Double, DataType::Varchar] {
        let values = match data_type {
            DataType::BigInt => vec![
                Value::Integer(i64::MIN.into()),
                Value::Null,
                Value::Integer(0),
                Value::Integer(i64::MAX.into()),
            ],
            DataType::Double => vec![
                Value::Double(f64::NAN),
                Value::Null,
                Value::Double(-0.0),
                Value::Double(0.0),
            ],
            _ => vec![
                Value::Varchar("A\0b".into()),
                Value::Null,
                Value::Varchar("".into()),
                Value::Varchar("ß".into()),
            ],
        };
        let bound = query.types().bind(&data_type)?;
        let flat = Vector::flat(data_type, values)?;
        for column in [
            flat.clone(),
            Arc::new(flat.clone()).select(vec![3, 0, 1, 0])?,
            flat.slice(1, 2)?,
        ] {
            let mut actual = Vec::new();
            bound.for_each_key(&column, &query, |index, key| {
                assert_eq!(index, actual.len());
                actual.push(key.map(<[u8]>::to_vec));
                Ok(())
            })?;
            let expected = column
                .values()
                .map(|value| {
                    if value.is_null() {
                        return Ok(None);
                    }
                    let mut key = Vec::new();
                    bound.append_key(value, &mut key, &query)?;
                    Ok(Some(key))
                })
                .collect::<Result<Vec<_>>>()?;
            assert_eq!(actual, expected);
        }
    }
    let mut types = TypeRegistry::builtins();
    types.register(ascii::FAMILY, Arc::new(StreamingAscii))?;
    let data_type = ascii::data_type(64)?;
    let bound = types.bind(&data_type)?;
    let invalid = Vector::flat(
        data_type.clone(),
        vec![
            Value::extension(data_type.clone(), b"valid".to_vec()),
            Value::extension(data_type, vec![255]),
        ],
    )?;
    let mut calls = 0;
    assert!(
        bound
            .for_each_key(&invalid, &query, |_, _| {
                calls += 1;
                Ok(())
            })
            .is_err()
    );
    assert_eq!(calls, 0);
    assert!(
        bound
            .for_each_key(
                &Vector::constant(DataType::BigInt, Value::Integer(1), 2)?,
                &query,
                |_, _| {
                    calls += 1;
                    Ok(())
                }
            )
            .is_err()
    );
    assert_eq!(calls, 0);
    let bound = query.types().bind(&DataType::BigInt)?;
    let input = Vector::constant(DataType::BigInt, Value::Integer(1), 3)?;
    assert!(matches!(
        bound.for_each_key(&input, &query, |_, _| {
            calls += 1;
            Err(Error::Execution("consumer stopped".into()))
        }),
        Err(Error::Execution(_))
    ));
    assert_eq!(calls, 1);
    Ok(())
}
