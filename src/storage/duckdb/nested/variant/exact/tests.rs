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
fn wrap(ty: DataType, value: Value) -> Result<Value> {
    NestedValue::value(
        NestedType::Variant.data_type(),
        NestedPayload::Variant {
            data_type: ty,
            value,
        },
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn exact_variant_equivalence_matches_canonical_encoding_not_sql_equality() -> Result<()> {
    let types = TypeRegistry::builtins();
    let bound = types.bind(&NestedType::Variant.data_type())?;
    let query = QueryContext::background();
    let mut c = crate::Database::memory()?.connect();
    let mut values = Vec::new();
    for sql in [
        "NULL",
        "true",
        "false",
        "1::TINYINT",
        "1::SMALLINT",
        "1::INTEGER",
        "1::BIGINT",
        "1::HUGEINT",
        "1::UTINYINT",
        "1::USMALLINT",
        "1::UINTEGER",
        "1::UBIGINT",
        "1::UHUGEINT",
        "1::FLOAT",
        "1::DOUBLE",
        "1::DECIMAL(4,0)",
        "1::DECIMAL(9,0)",
        "1::DECIMAL(18,0)",
        "1::DECIMAL(38,0)",
        "1::DECIMAL(4,1)",
        "1::BIGNUM",
        "'-0'::BIGNUM",
        "(-0.5::DOUBLE)::BIGNUM",
        "'0'::BIGNUM",
        "'340282366920938463463374607431768211456'::BIGNUM",
        "'1'",
        "'1'::BLOB",
        "'01'::BIT",
        "'010'::BIT",
        "'ffffffff-ffff-ffff-ffff-ffffffffffff'::UUID",
        "DATE '2000-01-01'",
        "TIMESTAMP_S '2000-01-01'",
        "TIMESTAMP_MS '2000-01-01'",
        "TIMESTAMP '2000-01-01'",
        "TIMESTAMP_NS '2000-01-01'",
        "TIMESTAMPTZ '2000-01-01'",
        "'2000-01-01'::TIMESTAMPTZ_NS",
        "TIME '00:00:00'",
        "'00:00:00'::TIME_NS",
        "'01:00:00+01'::TIMETZ",
        "'00:00:00+00'::TIMETZ",
        "INTERVAL '1 day'",
        "INTERVAL '24 hours'",
        "[]",
        "[NULL::INTEGER]",
        "[NULL::VARCHAR]",
        "[1,NULL]",
        "[1,NULL]::INTEGER[2]",
        "row(1,NULL)",
        "map([1],[NULL])",
        "[{'key':1,'value':NULL}]",
        "{'z':1,'a':NULL::INTEGER}",
        "{'z':1,'a':NULL::VARCHAR}",
        "{'a':NULL,'z':1}",
        "struct_pack()",
        "row()",
        "union_value(i:=1)",
        "{'a':NULL}",
        "{'A':NULL}",
    ] {
        values.push(c.query(&format!("SELECT ({sql})::VARIANT"))?.rows[0][0].clone());
    }
    for bits in [0_u32, 1 << 31, 0x7fc0_0001, 0x7fc0_0002] {
        values.push(wrap(DataType::Float, Value::Float(f32::from_bits(bits)))?);
    }
    for bits in [0_u64, 1 << 63, 0x7ff8_0000_0000_0001, 0x7ff8_0000_0000_0002] {
        let value = Value::Double(f64::from_bits(bits));
        values.push(wrap(DataType::Double, value.clone())?);
        let ty = NestedType::Struct(vec![("f".into(), DataType::Double)]).data_type();
        values.push(wrap(
            ty.clone(),
            NestedValue::value(ty, NestedPayload::Struct(vec![value]))?,
        )?);
    }
    let enumeration = DataType::enumeration(vec!["1".into(), "2".into()])?;
    values.push(wrap(
        enumeration.clone(),
        Value::enumeration(&enumeration, 0)?,
    )?);
    let object = NestedType::Object(vec![
        ("z".into(), DataType::Integer),
        ("a".into(), DataType::Varchar),
    ])
    .data_type();
    values.push(wrap(
        object.clone(),
        NestedValue::value(
            object,
            NestedPayload::Struct(vec![Value::Integer(1), Value::Null]),
        )?,
    )?);
    let encoded = super::super::encoding::encode_rows(&values, &bound, &query)?;
    for (i, left) in values.iter().enumerate() {
        for (j, right) in values.iter().enumerate() {
            assert_eq!(
                equivalent(left, right, &bound, &query)?,
                encoded[i] == encoded[j],
                "exact canonical pair {i}/{j}"
            );
        }
    }
    // A false positive here would permit layout recovery to erase type, bits,
    // interval fields, or BIGNUM sign even though SQL considers the pair equal.
    for (a, b) in [
        ("1::TINYINT", "1::INTEGER"),
        ("1::DECIMAL(4,0)", "1::DECIMAL(9,0)"),
        ("(-0.5::DOUBLE)::BIGNUM", "'0'::BIGNUM"),
        ("'-0'::DOUBLE", "'0'::DOUBLE"),
        ("INTERVAL '1 day'", "INTERVAL '24 hours'"),
    ] {
        let result = c.query(&format!("SELECT ({a})::VARIANT,({b})::VARIANT"))?;
        let pair = &result.rows[0];
        assert_eq!(
            bound.compare(&pair[0], &pair[1], &query)?,
            Ordering::Equal,
            "{a}/{b}"
        );
        assert!(!equivalent(&pair[0], &pair[1], &bound, &query)?, "{a}/{b}");
    }
    Ok(())
}

#[derive(Debug)]
struct SelectedValidation(Arc<AtomicUsize>, bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for SelectedValidation {
    fn name(&self) -> &'static str {
        "selected-exact-variant-child"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(ty)
    }
    fn validate_value(&self, ty: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        self.0.fetch_add(1, AtomicOrdering::Relaxed);
        if self.1 {
            return Err(Error::Resource("exact selected child failure".into()));
        }
        PrimitiveTypes.validate_value(ty, value, query)
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(a, b)
    }
    fn compare(&self, _: &DataType, _: &Value, _: &Value, _: &QueryContext) -> Result<Ordering> {
        panic!("exact content must not use selected SQL comparison")
    }
    fn write_key(
        &self,
        _: &DataType,
        _: &Value,
        _: &mut KeyWriter<'_>,
        _: &QueryContext,
    ) -> Result<()> {
        panic!("exact content must not use SQL keys")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn exact_variant_equivalence_retains_validation_and_bounds_without_mutation() -> Result<()> {
    let mut types = TypeRegistry::builtins();
    let calls = Arc::new(AtomicUsize::new(0));
    types.replace(
        "builtin.integer",
        Arc::new(SelectedValidation(calls.clone(), false)),
    )?;
    let bound = types.bind(&NestedType::Variant.data_type())?;
    types.replace(
        "builtin.integer",
        Arc::new(SelectedValidation(Arc::new(AtomicUsize::new(0)), true)),
    )?;
    let failing = types.bind(&NestedType::Variant.data_type())?;
    let query = QueryContext::background().with_types(Arc::new(TypeRegistry::default()));
    let value = wrap(DataType::Integer, Value::Integer(1))?;
    let original = value.clone();
    assert!(equivalent(&value, &value, &bound, &query)?);
    assert!(calls.load(AtomicOrdering::Relaxed) >= 2);
    assert!(
        matches!(equivalent(&value,&value,&failing,&query),Err(Error::Resource(message)) if message=="exact selected child failure")
    );
    assert_eq!(value, original);
    assert!(equivalent(&value, &value, &types.bind(&DataType::Integer)?, &query).is_err());
    let bad = Value::Nested(Arc::new(NestedValue {
        data_type: NestedType::Variant.data_type(),
        payload: NestedPayload::Variant {
            data_type: DataType::Integer,
            value: Value::Varchar("bad".into()),
        },
    }));
    assert!(equivalent(&value, &bad, &bound, &query).is_err());
    let null = wrap(DataType::Integer, Value::Null)?;
    assert!(equivalent(&null, &Value::Null, &bound, &query).is_err());
    for (left, right) in [(&value, &value), (&Value::Null, &Value::Null)] {
        assert!(matches!(
            equivalent_with_budget(
                left,
                right,
                &bound,
                &query,
                &mut Budget {
                    nodes: 0,
                    bytes: 100
                }
            ),
            Err(Error::Resource(_))
        ));
    }
    let text = wrap(DataType::Varchar, Value::Varchar("x".repeat(8193)))?;
    assert!(matches!(
        equivalent_with_budget(
            &text,
            &text,
            &bound,
            &query,
            &mut Budget {
                nodes: 100,
                bytes: 16385
            }
        ),
        Err(Error::Resource(_))
    ));
    let list_type = NestedType::List(bound.data_type().clone()).data_type();
    let repeated = wrap(
        list_type.clone(),
        NestedValue::value(list_type, NestedPayload::Sequence(vec![value.clone(); 4]))?,
    )?;
    assert!(equivalent(&repeated, &repeated, &bound, &query)?);
    // Repeated Arc children are logical visits, not permission to skip work
    // based on pointer identity or to treat the DAG as a single scalar.
    assert!(matches!(
        equivalent_with_budget(
            &repeated,
            &repeated,
            &bound,
            &query,
            &mut Budget {
                nodes: 10,
                bytes: 100
            }
        ),
        Err(Error::Resource(_))
    ));
    let interrupt = crate::parallel::InterruptHandle::default();
    interrupt.interrupt();
    let cancelled = QueryContext::new(interrupt, None, 1, 1)?;
    assert!(matches!(
        equivalent(&value, &value, &bound, &cancelled),
        Err(Error::Interrupted)
    ));
    let mut budget = Budget {
        nodes: 100,
        bytes: 100,
    };
    assert!(matches!(
        resolve(
            Node::Typed(&DataType::Integer, &Value::Integer(1)),
            65,
            &mut budget,
            &query
        ),
        Err(Error::Resource(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn exact_variant_object_names_order_and_present_nulls_remain_significant() -> Result<()> {
    let bound = TypeRegistry::builtins().bind(&NestedType::Variant.data_type())?;
    let query = QueryContext::background();
    let make = |names: &[&str]| -> Result<Value> {
        let ty = NestedType::Object(
            names
                .iter()
                .map(|name| (String::from(*name), DataType::Integer))
                .collect(),
        )
        .data_type();
        wrap(
            ty.clone(),
            NestedValue::value(ty, NestedPayload::Struct(vec![Value::Null; names.len()]))?,
        )
    };
    let original = make(&["", "A", "a", "nul\0key"])?;
    assert!(equivalent(&original, &original, &bound, &query)?);
    for other in [
        make(&["", "a", "A", "nul\0key"])?,
        make(&["", "A", "a"])?,
        make(&["", "A", "a", "nulkey"])?,
    ] {
        assert!(!equivalent(&original, &other, &bound, &query)?);
    }
    let empty = make(&[])?;
    assert!(!equivalent(&empty, &make(&[""])?, &bound, &query)?);
    assert!(!equivalent(&empty, &Value::Null, &bound, &query)?);
    let ty = NestedType::Struct(vec![("n".into(), DataType::Integer)]).data_type();
    let bad = Value::Nested(Arc::new(NestedValue {
        data_type: ty.clone(),
        payload: NestedPayload::Struct(vec![]),
    }));
    let bad = Value::Nested(Arc::new(NestedValue {
        data_type: bound.data_type().clone(),
        payload: NestedPayload::Variant {
            data_type: ty,
            value: bad,
        },
    }));
    assert!(equivalent(&bad, &empty, &bound, &query).is_err());
    Ok(())
}
