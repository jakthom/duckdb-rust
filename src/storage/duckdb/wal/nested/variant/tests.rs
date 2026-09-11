use super::*;
use crate::common::type_registry::{KeyWriter, PrimitiveTypes, TypeAdapter, TypeRegistry};
use std::{
    cmp::Ordering,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn encode(
    values: &[Value],
    ty: &DataType,
    depth: usize,
    budget: usize,
    query: &QueryContext,
) -> Result<Vec<u8>> {
    let mut output = Encoder::default();
    super::super::super::writer::vector(
        &mut output,
        ty,
        values.iter(),
        depth,
        &mut { budget },
        query,
    )?;
    output.end();
    Ok(output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decode(
    bytes: &[u8],
    count: usize,
    depth: usize,
    budget: usize,
    query: &QueryContext,
) -> Result<Vec<Value>> {
    let mut reader = Reader::new(bytes.to_vec());
    let values = super::super::super::chunk::vector(
        &mut reader,
        &NestedType::Variant.data_type(),
        count,
        depth,
        &mut { budget },
        query,
    )?;
    reader.end()?;
    if !reader.finished() {
        return Err(corrupt("test VARIANT vector trailing bytes"));
    }
    Ok(values)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn variant_wal_vectors_preserve_exact_tags_bits_and_nested_nulls() -> Result<()> {
    let query = QueryContext::background();
    let ty = NestedType::Variant.data_type();
    let selected = query.types().bind(&ty)?;
    let mut connection = crate::Database::memory()?.connect();
    let mut values = vec![Value::Null];
    for expression in [
        "true",
        "false",
        "'-128'::TINYINT",
        "'-32768'::SMALLINT",
        "'-2147483648'::INTEGER",
        "'-9223372036854775808'::BIGINT",
        "'-170141183460469231731687303715884105728'::HUGEINT",
        "255::UTINYINT",
        "65535::USMALLINT",
        "4294967295::UINTEGER",
        "18446744073709551615::UBIGINT",
        "'340282366920938463463374607431768211455'::UHUGEINT",
        "'-0.0'::FLOAT",
        "'nan'::DOUBLE",
        "1.2::DECIMAL(4,1)",
        "1.2::DECIMAL(9,1)",
        "1.2::DECIMAL(18,1)",
        "1.2::DECIMAL(38,1)",
        "'🦆'",
        "from_hex('610062')",
        "'ffffffff-ffff-ffff-ffff-ffffffffffff'::UUID",
        "DATE '-infinity'",
        "TIME '24:00:00'",
        "'23:59:59.123456789'::TIME_NS",
        "'2000-01-01'::TIMESTAMP_S",
        "'2000-01-01'::TIMESTAMP_MS",
        "'2000-01-01'::TIMESTAMP",
        "'2000-01-01 00:00:00.123456789'::TIMESTAMP_NS",
        "'24:00:00+05:30'::TIMETZ",
        "'2000-01-01 00:00:00+00'::TIMESTAMPTZ",
        "INTERVAL '1 month -2 days 3 microseconds'",
        "(-0.5::DOUBLE)::BIGNUM",
        "'340282366920938463463374607431768211456'::BIGNUM",
        "'101010101'::BIT",
        "'2000-01-01 00:00:00.123456789+00'::TIMESTAMPTZ_NS",
        "'red'::ENUM('red','blue')",
        "[1,NULL,2]",
        "[1,NULL]::INTEGER[2]",
        "{'d':1.25::DECIMAL(12,2),'l':[TIMESTAMP_NS '2000-01-01 00:00:00.123456789',NULL]}",
        "map(['x','y'],[[1,NULL],[]])",
        "(1,'a',NULL)",
        "union_value(i:=1)",
        "union_value(i:=NULL)",
    ] {
        values.push(
            connection
                .query(&format!("SELECT ({expression})::VARIANT"))?
                .rows[0][0]
                .clone(),
        );
    }
    // Noncanonical NaN payloads and exact-case/empty/NUL object names must not
    // disappear behind SQL numeric comparison, map keys or diagnostic text.
    for value in [
        Value::Float(f32::from_bits(0x7fc01234)),
        Value::Double(f64::from_bits(0x7ff8000000001234)),
    ] {
        let data_type = if matches!(value, Value::Float(_)) {
            DataType::Float
        } else {
            DataType::Double
        };
        values.push(NestedValue::value(
            ty.clone(),
            NestedPayload::Variant { data_type, value },
        )?);
    }
    let object = NestedType::Object(vec![
        ("".into(), DataType::Integer),
        ("A".into(), DataType::Integer),
        ("a".into(), DataType::Integer),
        ("\0".into(), DataType::Integer),
    ])
    .data_type();
    values.push(NestedValue::value(
        ty.clone(),
        NestedPayload::Variant {
            data_type: object.clone(),
            value: NestedValue::value(
                object,
                NestedPayload::Struct(vec![
                    Value::Integer(1),
                    Value::Null,
                    Value::Integer(2),
                    Value::Integer(3),
                ]),
            )?,
        },
    )?);
    let bytes = encode(&values, &ty, 0, MAX_CELLS, &query)?;
    let recovered = decode(&bytes, values.len(), 0, MAX_CELLS, &query)?;
    let before = canonical::encode(&values, &selected, 0, &mut { MAX_CELLS }, &query)?;
    let after = canonical::encode(&recovered, &selected, 0, &mut { MAX_CELLS }, &query)?;
    assert_eq!(before, after); // Deterministic wire payload includes raw float bits.
    assert!(super::super::super::super::write_support::wal_type(&ty).is_err());
    assert!(super::super::super::super::write_support::successor_type(&ty).is_err());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn physical(
    keys: &[&str],
    children: &[(Option<u32>, u32)],
    descriptors: &[(u8, u32)],
    bytes: &[u8],
) -> Result<Value> {
    let DataType::Nested(metadata) = canonical::data_type() else {
        unreachable!()
    };
    let NestedType::Struct(fields) = metadata.as_ref() else {
        unreachable!()
    };
    let lists = [
        keys.iter()
            .map(|key| Value::Varchar((*key).into()))
            .collect(),
        children
            .iter()
            .map(|(key, child)| {
                vec![
                    key.map_or(Value::Null, |key| Value::Unsigned(key as u128)),
                    Value::Unsigned(*child as u128),
                ]
            })
            .map(|row| {
                let DataType::Nested(ty) = &fields[1].1 else {
                    unreachable!()
                };
                let NestedType::List(ty) = ty.as_ref() else {
                    unreachable!()
                };
                NestedValue::value(ty.clone(), NestedPayload::Struct(row))
            })
            .collect::<Result<_>>()?,
        descriptors
            .iter()
            .map(|(tag, offset)| {
                vec![
                    Value::Unsigned(*tag as u128),
                    Value::Unsigned(*offset as u128),
                ]
            })
            .map(|row| {
                let DataType::Nested(ty) = &fields[2].1 else {
                    unreachable!()
                };
                let NestedType::List(ty) = ty.as_ref() else {
                    unreachable!()
                };
                NestedValue::value(ty.clone(), NestedPayload::Struct(row))
            })
            .collect::<Result<_>>()?,
    ];
    let mut row = lists
        .into_iter()
        .zip(fields)
        .map(|(values, (_, ty))| NestedValue::value(ty.clone(), NestedPayload::Sequence(values)))
        .collect::<Result<Vec<_>>>()?;
    row.push(Value::Blob(bytes.to_vec()));
    NestedValue::value(canonical::data_type(), NestedPayload::Struct(row))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn variant_wal_vectors_reject_malformed_payloads_truncation_and_amplification() -> Result<()> {
    let query = QueryContext::background();
    let row = physical(&[], &[], &[(5, 0)], &42i32.to_le_bytes())?;
    let bytes = encode(
        std::slice::from_ref(&row),
        &canonical::data_type(),
        0,
        MAX_CELLS,
        &query,
    )?;
    assert_eq!(decode(&bytes, 1, 0, MAX_CELLS, &query)?.len(), 1);
    for end in 0..bytes.len() {
        assert!(
            decode(&bytes[..end], 1, 0, MAX_CELLS, &query).is_err(),
            "prefix {end}"
        );
    }
    for row in [
        physical(&[], &[], &[(0, 0)], &[])?,
        physical(&[], &[], &[(35, 0)], &[])?,
        physical(&[], &[], &[(5, 4)], &[0; 4])?,
        physical(&[], &[(None, 0)], &[(30, 0)], &[1, 0])?,
        physical(&["x"], &[(Some(0), 1)], &[(30, 0), (1, 0)], &[1, 0])?,
        physical(
            &["x"],
            &[(Some(0), 1), (Some(0), 1)],
            &[(29, 0), (1, 0)],
            &[2, 0],
        )?,
    ] {
        let bytes = encode(&[row], &canonical::data_type(), 0, MAX_CELLS, &query)?;
        assert!(matches!(
            decode(&bytes, 1, 0, MAX_CELLS, &query),
            Err(Error::Corrupt(_))
        ));
    }
    assert!(matches!(
        decode(&bytes, 1, 0, 2, &query),
        Err(Error::Resource(_))
    ));
    assert!(matches!(
        decode(&bytes, 1, 65, MAX_CELLS, &query),
        Err(Error::Resource(_))
    ));
    let handle = crate::parallel::InterruptHandle::default();
    let interrupted = QueryContext::new(handle.clone(), None, 2, MAX_CELLS)?;
    handle.interrupt();
    assert!(matches!(
        decode(&bytes, 1, 0, MAX_CELLS, &interrupted),
        Err(Error::Interrupted)
    ));
    let selected = query.types().bind(&NestedType::Variant.data_type())?;
    assert!(matches!(
        canonical::decode(&[row], &selected, 0, &mut { MAX_CELLS }, &interrupted),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[derive(Debug)]
struct Selected(Arc<AtomicUsize>, bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for Selected {
    fn name(&self) -> &'static str {
        "wal-selected-integer"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(ty)
    }
    fn validate_value(&self, ty: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        self.0.fetch_add(1, AtomicOrdering::Relaxed);
        if self.1 {
            return Err(Error::Resource("selected WAL child failure".into()));
        }
        PrimitiveTypes.validate_value(ty, value, query)
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(a, b)
    }
    fn compare(&self, _: &DataType, _: &Value, _: &Value, _: &QueryContext) -> Result<Ordering> {
        panic!("wire codec called SQL compare")
    }
    fn write_key(
        &self,
        _: &DataType,
        _: &Value,
        _: &mut KeyWriter<'_>,
        _: &QueryContext,
    ) -> Result<()> {
        panic!("wire codec called SQL keys")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn variant_wal_canonical_bridge_retains_selected_children_and_propagates_failures() -> Result<()> {
    let mut types = TypeRegistry::builtins();
    let calls = Arc::new(AtomicUsize::new(0));
    types.replace("builtin.integer", Arc::new(Selected(calls.clone(), false)))?;
    let selected = types.bind(&NestedType::Variant.data_type())?;
    types.replace("builtin.integer", Arc::new(Selected(calls.clone(), true)))?;
    let failing = types.bind(&NestedType::Variant.data_type())?;
    let query = QueryContext::background().with_types(Arc::new(TypeRegistry::default()));
    let value = NestedValue::value(
        NestedType::Variant.data_type(),
        NestedPayload::Variant {
            data_type: DataType::Integer,
            value: Value::Integer(42),
        },
    )?;
    let physical = canonical::encode(
        std::slice::from_ref(&value),
        &selected,
        0,
        &mut { MAX_CELLS },
        &query,
    )?;
    assert_eq!(
        canonical::decode(&physical, &selected, 0, &mut { MAX_CELLS }, &query)?,
        vec![value.clone()]
    );
    assert!(calls.load(AtomicOrdering::Relaxed) >= 2);
    assert!(
        matches!(canonical::decode(&physical,&failing,0,&mut { MAX_CELLS },&query),Err(Error::Resource(message)) if message=="selected WAL child failure")
    );
    assert!(
        matches!(canonical::encode(&[value],&failing,0,&mut { MAX_CELLS },&query),Err(Error::Resource(message)) if message=="selected WAL child failure")
    );
    assert!(
        canonical::decode(
            &physical,
            &types.bind(&DataType::Integer)?,
            0,
            &mut { MAX_CELLS },
            &query
        )
        .is_err()
    );
    Ok(())
}
