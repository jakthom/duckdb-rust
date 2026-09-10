use super::*;
use crate::common::type_registry::{KeyWriter, PrimitiveTypes, TypeAdapter, TypeRegistry};
use std::{
    cmp::Ordering,
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decode(value: &Value) -> Result<Value> {
    let mut budget = payload::Budget::new();
    match payload::Unshredded::new(value, &mut budget)? {
        None => Ok(Value::Null),
        Some(value) => payload::envelope(value.decode(0, &mut budget)?),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn canonical_variant_encoder_preserves_exact_scalar_tags_widths_and_bytes() -> Result<()> {
    let types = TypeRegistry::builtins();
    let variant = types.bind(&NestedType::Variant.data_type())?;
    let query = QueryContext::background();
    let mut connection = crate::Database::memory()?.connect();
    let cases = [
        ("true", 1),
        ("false", 2),
        ("'-128'::TINYINT", 3),
        ("'-32768'::SMALLINT", 4),
        ("'-2147483648'::INTEGER", 5),
        ("'-9223372036854775808'::BIGINT", 6),
        ("'-170141183460469231731687303715884105728'::HUGEINT", 7),
        ("255::UTINYINT", 8),
        ("65535::USMALLINT", 9),
        ("4294967295::UINTEGER", 10),
        ("18446744073709551615::UBIGINT", 11),
        ("'340282366920938463463374607431768211455'::UHUGEINT", 12),
        ("'-0.0'::FLOAT", 13),
        ("'nan'::DOUBLE", 14),
        ("1.2::DECIMAL(4,1)", 15),
        ("1.2::DECIMAL(9,1)", 15),
        ("1.2::DECIMAL(18,1)", 15),
        ("1.2::DECIMAL(38,1)", 15),
        ("'🦆'", 16),
        ("from_hex('610062')", 17),
        ("'ffffffff-ffff-ffff-ffff-ffffffffffff'::UUID", 18),
        ("DATE '-infinity'", 19),
        ("TIME '24:00:00'", 20),
        ("'23:59:59.123456789'::TIME_NS", 21),
        ("'2000-01-01'::TIMESTAMP_S", 22),
        ("'2000-01-01'::TIMESTAMP_MS", 23),
        ("'2000-01-01'::TIMESTAMP", 24),
        ("'2000-01-01 00:00:00.123456789'::TIMESTAMP_NS", 25),
        ("'24:00:00+05:30'::TIMETZ", 26),
        ("'2000-01-01 00:00:00+00'::TIMESTAMPTZ", 27),
        ("INTERVAL '1 month -2 days 3 microseconds'", 28),
        ("'-0'::BIGNUM", 31),
        ("'340282366920938463463374607431768211456'::BIGNUM", 31),
        ("'101010101'::BIT", 32),
        ("'2000-01-01 00:00:00.123456789+00'::TIMESTAMPTZ_NS", 34),
        ("'red'::ENUM('red','blue')", 16),
    ];
    for (sql, tag) in cases {
        let original = connection.query(&format!("SELECT ({sql})::VARIANT"))?.rows[0][0].clone();
        let rows = encode_rows(std::slice::from_ref(&original), &variant, &query)?;
        let fields = payload::record(&rows[0])?;
        let descriptors = payload::sequence(&fields[2])?;
        assert_eq!(
            payload::record(&descriptors[0])?,
            [Value::Unsigned(tag), Value::Unsigned(0)],
            "{sql}"
        );
        let decoded = decode(&rows[0])?;
        assert_eq!(
            variant.compare(&original, &decoded, &query)?,
            Ordering::Equal,
            "{sql}"
        );
        let original = Node::Typed(variant.data_type(), &original).materialized(0, &|| Ok(()))?;
        let decoded = Node::Typed(variant.data_type(), &decoded).materialized(0, &|| Ok(()))?;
        assert_eq!(original.0, decoded.0, "{sql}");
        match (&original.1, &decoded.1) {
            (Value::Float(a), Value::Float(b)) => assert_eq!(a.to_bits(), b.to_bits()),
            (Value::Double(a), Value::Double(b)) => assert_eq!(a.to_bits(), b.to_bits()),
            _ => assert_eq!(original.1, decoded.1, "{sql}"),
        }
        if tag == 15 {
            let Value::Blob(bytes) = &fields[3] else {
                unreachable!()
            };
            let DataType::Decimal { width, scale } = original.0 else {
                unreachable!()
            };
            assert_eq!(&bytes[..2], &[width, scale]);
            assert_eq!(
                &bytes[2..],
                &12i128.to_le_bytes()[..super::super::super::super::primitive::width(
                    &DataType::Decimal { width, scale }
                )?]
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn canonical_variant_encoder_reserves_contiguous_children_and_preserves_logical_containers()
-> Result<()> {
    let types = TypeRegistry::builtins();
    let variant = types.bind(&NestedType::Variant.data_type())?;
    let query = QueryContext::background();
    let mut connection = crate::Database::memory()?.connect();
    for sql in [
        "NULL::VARIANT",
        "[]::VARIANT",
        "struct_pack()::VARIANT",
        "[1,NULL,3]::VARIANT",
        "[1,NULL]::INTEGER[2]::VARIANT",
        "row(1,'a',NULL)::VARIANT",
        "map(['','A','a'],[1,2,NULL])::VARIANT",
        "{'z':[{'a':1},{'b':2}],'a':NULL}::VARIANT",
        "union_value(i:=[1,NULL])::VARIANT",
        "union_value(i:=NULL)::VARIANT",
    ] {
        let original = connection.query(&format!("SELECT {sql}"))?.rows[0][0].clone();
        let rows = encode_rows(std::slice::from_ref(&original), &variant, &query)?;
        let decoded = decode(&rows[0])?;
        assert_eq!(original.is_null(), decoded.is_null(), "{sql}");
        if !original.is_null() {
            assert_eq!(
                variant.compare(&original, &decoded, &query)?,
                Ordering::Equal,
                "{sql}"
            );
        }
        let text = connection.prepare("SELECT ($1)::VARCHAR,variant_typeof($1)")?;
        assert_eq!(
            connection.execute_prepared(&text, &[original])?.rows,
            connection.execute_prepared(&text, &[decoded])?.rows,
            "{sql}"
        );
    }
    let object_type = NestedType::Object(vec![
        ("".into(), DataType::Integer),
        ("A".into(), DataType::Integer),
        ("a".into(), NestedType::List(DataType::Integer).data_type()),
        ("nul\0key".into(), DataType::Integer),
    ])
    .data_type();
    let sequence = NestedValue::value(
        NestedType::List(DataType::Integer).data_type(),
        NestedPayload::Sequence(vec![Value::Integer(3), Value::Null]),
    )?;
    let object = NestedValue::value(
        object_type.clone(),
        NestedPayload::Struct(vec![
            Value::Integer(1),
            Value::Integer(2),
            sequence,
            Value::Null,
        ]),
    )?;
    let original = Node::Typed(&object_type, &object).owned()?;
    let encoded = encode_rows(std::slice::from_ref(&original), &variant, &query)?;
    let fields = payload::record(&encoded[0])?;
    assert_eq!(
        payload::sequence(&fields[0])?,
        ["", "A", "a", "nul\0key"].map(|name| Value::Varchar(name.into()))
    );
    assert_eq!(payload::sequence(&fields[1])?.len(), 6);
    let decoded = decode(&encoded[0])?;
    assert_eq!(
        variant.compare(&original, &decoded, &query)?,
        Ordering::Equal
    );
    // VARIANT_NULL has no declared child type in the native layout. Compare
    // canonical dynamic materializations, not the original typed NULL hints.
    assert_eq!(
        Node::Typed(variant.data_type(), &decoded).materialized(0, &|| Ok(()))?,
        Node::Typed(variant.data_type(), &original).materialized(0, &|| Ok(()))?
    );
    Ok(())
}

#[derive(Debug)]
struct SelectedInteger {
    calls: Arc<AtomicUsize>,
    fail: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for SelectedInteger {
    fn name(&self) -> &'static str {
        "selected-variant-encoding-child"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(ty)
    }
    fn validate_value(&self, ty: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        self.calls.fetch_add(1, AtomicOrdering::Relaxed);
        if self.fail {
            return Err(Error::Resource("selected encoding child failure".into()));
        }
        PrimitiveTypes.validate_value(ty, value, query)
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(a, b)
    }
    fn compare(
        &self,
        ty: &DataType,
        a: &Value,
        b: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        PrimitiveTypes.compare(ty, a, b, query)
    }
    fn write_key(
        &self,
        ty: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.write_key(ty, value, output, query)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn canonical_variant_encoder_retains_selection_and_rejects_errors_without_partial_output()
-> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let replacement = Arc::new(AtomicUsize::new(0));
    let mut types = TypeRegistry::builtins();
    types.replace(
        "builtin.integer",
        Arc::new(SelectedInteger {
            calls: calls.clone(),
            fail: false,
        }),
    )?;
    let variant = types.bind(&NestedType::Variant.data_type())?;
    types.replace(
        "builtin.integer",
        Arc::new(SelectedInteger {
            calls: replacement.clone(),
            fail: true,
        }),
    )?;
    let failing = types.bind(&NestedType::Variant.data_type())?;
    let query = QueryContext::background().with_types(Arc::new(TypeRegistry::default()));
    let value = Node::Typed(&DataType::Integer, &Value::Integer(7)).owned()?;
    let values = vec![Value::Null, value];
    let original = values.clone();
    assert_eq!(encode_rows(&values, &variant, &query)?.len(), 2);
    assert!(calls.load(AtomicOrdering::Relaxed) > 0);
    assert_eq!(replacement.load(AtomicOrdering::Relaxed), 0);
    assert!(
        matches!(encode_rows(&values, &failing, &query), Err(Error::Resource(message)) if message == "selected encoding child failure")
    );
    assert_eq!(values, original);
    assert!(encode_rows(&values, &types.bind(&DataType::Integer)?, &query).is_err());
    for (nodes, bytes) in [(0, 100), (100, 0)] {
        assert!(matches!(
            encode_with_limits(&values, &variant, &query, &mut Limits { nodes, bytes }),
            Err(Error::Resource(_))
        ));
    }
    let bad = Value::Nested(Arc::new(NestedValue {
        data_type: NestedType::Variant.data_type(),
        payload: NestedPayload::Variant {
            data_type: DataType::Integer,
            value: Value::Varchar("bad".into()),
        },
    }));
    assert!(encode_rows(&[bad], &variant, &query).is_err());
    let interrupt = crate::parallel::InterruptHandle::default();
    interrupt.interrupt();
    let query = QueryContext::new(interrupt, None, 1, 1)?;
    assert!(matches!(
        encode_rows(&values, &variant, &query),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn canonical_variant_encoder_roundtrips_independent_native_children_without_publication()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let types = TypeRegistry::builtins();
    let bound = types.bind(&NestedType::Variant.data_type())?;
    let query = QueryContext::background();
    for (target, name) in [
        ("development", "nested_variant_unshredded"),
        ("development", "nested_variant_shredded"),
        ("release", "nested_variant_shredded"),
    ] {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "test/data/duckdb/nested-variant-{target}/{name}.duckdb.gz"
        ));
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(std::fs::File::open(source)?).read_to_end(&mut bytes)?;
        let path = directory.path().join(format!("{target}-{name}.duckdb"));
        std::fs::write(&path, &bytes)?;
        let mut connection = crate::Database::open_read_only(&path)?.connect();
        let rows = connection
            .query("SELECT v,xs[1],s::VARIANT FROM t ORDER BY id")?
            .rows;
        let values = rows.into_iter().flatten().collect::<Vec<_>>();
        let encoded = encode_rows(&values, &bound, &query)?;
        let text = connection.prepare("SELECT ($1)::VARCHAR,variant_typeof($1)")?;
        for (original, encoded) in values.iter().zip(encoded) {
            let decoded = decode(&encoded)?;
            assert_eq!(original.is_null(), decoded.is_null());
            if !original.is_null() {
                assert_eq!(bound.compare(original, &decoded, &query)?, Ordering::Equal);
            }
            assert_eq!(
                connection
                    .execute_prepared(&text, std::slice::from_ref(original))?
                    .rows,
                connection.execute_prepared(&text, &[decoded])?.rows
            );
        }
        assert_eq!(std::fs::read(&path)?, bytes);
        assert!(!path.with_extension("duckdb.wal").exists());
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn canonical_variant_encoder_checks_varints_depth_and_physical_temporal_boundaries() -> Result<()> {
    let types = TypeRegistry::builtins();
    let bound = types.bind(&NestedType::Variant.data_type())?;
    let query = QueryContext::background();
    for temporal in [
        crate::common::TemporalValue::Time(86_400_500_000),
        crate::common::TemporalValue::TimeNs(86_400_500_000_000),
        crate::common::TemporalValue::TimeTz {
            micros: 86_400_500_000,
            offset: 57599,
        },
        crate::common::TemporalValue::from_ticks(&DataType::TimestampNs, i64::MIN + 2)?,
    ] {
        let ty = temporal.data_type();
        let value = Node::Typed(&ty, &Value::Temporal(temporal)).owned()?;
        let encoded = encode_rows(std::slice::from_ref(&value), &bound, &query)?;
        assert_eq!(decode(&encoded[0])?, value);
    }
    let string = Node::Typed(&DataType::Varchar, &Value::Varchar("\0".repeat(128))).owned()?;
    let list_type = NestedType::List(DataType::Integer).data_type();
    let list = NestedValue::value(
        list_type.clone(),
        NestedPayload::Sequence(vec![Value::Integer(0); 129]),
    )?;
    let list = Node::Typed(&list_type, &list).owned()?;
    for (value, prefix) in [(string, &[128, 1][..]), (list, &[129, 1, 0][..])] {
        let encoded = encode_rows(std::slice::from_ref(&value), &bound, &query)?;
        let fields = payload::record(&encoded[0])?;
        let Value::Blob(bytes) = &fields[3] else {
            unreachable!()
        };
        assert!(bytes.starts_with(prefix));
        assert_eq!(decode(&encoded[0])?, value);
    }
    let mut limits = Limits {
        nodes: 100,
        bytes: 100,
    };
    let mut row = Builder {
        keys: Vec::new(),
        children: Vec::new(),
        values: Vec::new(),
        data: Vec::new(),
        limits: &mut limits,
        query: &query,
    };
    assert!(matches!(
        row.emit(Node::Typed(&DataType::Integer, &Value::Integer(1)), 65),
        Err(Error::Resource(_))
    ));
    if usize::BITS > 32 {
        assert!(index(u32::MAX as usize + 1).is_err());
    }
    let root_null = NestedValue::value(
        NestedType::Variant.data_type(),
        NestedPayload::Variant {
            data_type: DataType::Integer,
            value: Value::Null,
        },
    )?;
    assert!(encode_rows(&[root_null], &bound, &query).is_err());
    Ok(())
}
