use super::*;
use duckdb_rust::common::type_registry::{
    KeyRepresentation, KeyWriter, OrderingRepresentation, PrimitiveTypes, TypeAdapter, TypeRegistry,
};
use std::cmp::Ordering as Cmp;

#[derive(Debug)]
struct OrderedIntegers {
    representation: OrderingRepresentation,
    fail_comparison: bool,
    reject_negative: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for OrderedIntegers {
    fn name(&self) -> &'static str {
        "test-ordered-integers"
    }
    fn ordering_representation(&self, _: &DataType) -> OrderingRepresentation {
        self.representation
    }
    fn key_representation(&self, _: &DataType) -> KeyRepresentation {
        KeyRepresentation::Integer
    }
    fn validate_type(&self, t: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(t)
    }
    fn validate_value(&self, t: &DataType, v: &Value, q: &QueryContext) -> Result<()> {
        PrimitiveTypes.validate_value(t, v, q)?;
        if self.reject_negative && v.as_i128()? < 0 {
            return Err(Error::Conversion("negative logical integer".into()));
        }
        Ok(())
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(a, b)
    }
    fn compare(&self, t: &DataType, a: &Value, b: &Value, q: &QueryContext) -> Result<Cmp> {
        if self.fail_comparison {
            return Err(Error::Execution("comparison failure".into()));
        }
        let order = PrimitiveTypes.compare(t, a, b, q)?;
        Ok(
            if self.representation == OrderingRepresentation::Comparison {
                order.reverse()
            } else {
                order
            },
        )
    }
    fn write_key(
        &self,
        t: &DataType,
        v: &Value,
        out: &mut KeyWriter<'_>,
        q: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.write_key(t, v, out, q)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn ordering_capability_is_independent_of_integer_equality_and_preserves_failures() -> Result<()> {
    for fail_comparison in [false, true] {
        let mut types = TypeRegistry::builtins();
        let retained = types.bind(&DataType::BigInt)?;
        types.replace(
            DataType::BigInt.family(),
            Arc::new(OrderedIntegers {
                representation: OrderingRepresentation::Comparison,
                fail_comparison,
                reject_negative: false,
            }),
        )?;
        assert_eq!(
            retained.ordering_representation(),
            OrderingRepresentation::SignedInteger
        );
        let bound = types.bind(&DataType::BigInt)?;
        assert_eq!(bound.key_representation(), KeyRepresentation::Integer);
        assert_eq!(
            bound.ordering_representation(),
            OrderingRepresentation::Comparison
        );
        let types = Arc::new(types);
        for algorithm in algorithms() {
            let db = DatabaseBuilder::new()
                .types(types.clone())
                .physical_planner(Arc::new(
                    NativePhysicalPlanner::default().with_sorting(algorithm),
                ))
                .build()?;
            let mut c = db.connect();
            c.execute("CREATE TABLE t(i BIGINT); INSERT INTO t VALUES(1),(3),(2),(NULL)")?;
            let result = c.query("SELECT i FROM t ORDER BY i NULLS FIRST");
            if fail_comparison {
                assert!(
                    matches!(result,Err(Error::Execution(message)) if message == "comparison failure")
                );
            } else {
                assert_eq!(
                    result?.rows,
                    vec![vec![Value::Null], ints(&[3]), ints(&[2]), ints(&[1])]
                );
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn integer_ordering_does_not_bypass_logical_validation() -> Result<()> {
    let mut types = TypeRegistry::builtins();
    types.replace(
        DataType::BigInt.family(),
        Arc::new(OrderedIntegers {
            representation: OrderingRepresentation::SignedInteger,
            fail_comparison: false,
            reject_negative: true,
        }),
    )?;
    let query = QueryContext::background().with_types(Arc::new(types));
    let input = DataChunk::from_rows(&[DataType::BigInt], &[ints(&[1])])?;
    for algorithm in algorithms() {
        assert!(
            matches!(
                sort(
                    algorithm.as_ref(),
                    &query,
                    &ReturnedKey(Value::Integer(-1)),
                    &input,
                    &[key(0, &DataType::BigInt, false, false)]
                ),
                Err(Error::Conversion(_))
            ),
            "{} accepted an invalid logical sort key",
            algorithm.name()
        );
    }
    Ok(())
}

#[derive(Debug)]
struct InvalidCapability;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for InvalidCapability {
    fn name(&self) -> &'static str {
        "invalid-ordering-capability"
    }
    fn ordering_representation(&self, _: &DataType) -> OrderingRepresentation {
        OrderingRepresentation::SignedInteger
    }
    fn validate_type(&self, t: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(t)
    }
    fn validate_value(&self, t: &DataType, v: &Value, q: &QueryContext) -> Result<()> {
        PrimitiveTypes.validate_value(t, v, q)
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(a, b)
    }
    fn compare(&self, t: &DataType, a: &Value, b: &Value, q: &QueryContext) -> Result<Cmp> {
        PrimitiveTypes.compare(t, a, b, q)
    }
    fn write_key(
        &self,
        t: &DataType,
        v: &Value,
        out: &mut KeyWriter<'_>,
        q: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.write_key(t, v, out, q)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn integer_ordering_capability_rejects_other_physical_types() -> Result<()> {
    let mut types = TypeRegistry::builtins();
    types.replace(DataType::Varchar.family(), Arc::new(InvalidCapability))?;
    assert!(matches!(
        types.bind(&DataType::Varchar),
        Err(Error::Bind(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sorting_fallback_preserves_string_boolean_float_order_and_payload_bits() -> Result<()> {
    let float_values = [
        f64::NAN,
        -0.0,
        0.0,
        f64::NEG_INFINITY,
        f64::INFINITY,
        0.0,
        1.0,
        f64::from_bits(0x7ff8_0000_0000_0042),
    ];
    let cases = [
        (
            DataType::Varchar,
            [Some("z"), Some("a"), Some("ä"), Some("A"), None, Some("a")]
                .map(|s| s.map_or(Value::Null, |s| Value::Varchar(s.into())))
                .to_vec(),
            vec![3, 1, 5, 0, 2, 4],
        ),
        (
            DataType::Boolean,
            vec![
                Value::Boolean(true),
                Value::Null,
                Value::Boolean(false),
                Value::Boolean(true),
                Value::Boolean(false),
            ],
            vec![2, 4, 0, 3, 1],
        ),
        (
            DataType::Double,
            float_values
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    if i == 5 {
                        Value::Null
                    } else {
                        Value::Double(*v)
                    }
                })
                .collect(),
            vec![3, 1, 2, 6, 4, 0, 7, 5],
        ),
        (
            DataType::Float,
            float_values
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    if i == 5 {
                        Value::Null
                    } else {
                        Value::Float(*v as f32)
                    }
                })
                .collect(),
            vec![3, 1, 2, 6, 4, 0, 7, 5],
        ),
    ];
    for (data_type, values, ids) in cases {
        let rows: Vec<_> = values
            .iter()
            .enumerate()
            .map(|(i, v)| vec![v.clone(), Value::Integer(i as i128)])
            .collect();
        let input = DataChunk::from_rows(&[data_type.clone(), DataType::BigInt], &rows)?;
        let expected: Vec<_> = ids.iter().map(|&i| rows[i].clone()).collect();
        for algorithm in algorithms() {
            let result = sort(
                algorithm.as_ref(),
                &QueryContext::background(),
                &BatchedEvaluator,
                &input,
                &[key(0, &data_type, false, false)],
            )?;
            // The value serializer records IEEE bits, so NaN payloads and
            // signed zero are checked without NaN's non-reflexive equality.
            assert_eq!(
                serde_json::to_value(&result).unwrap(),
                serde_json::to_value(&expected).unwrap(),
                "{} {data_type}",
                algorithm.name()
            );
        }
    }
    Ok(())
}
