use super::*;
use crate::common::{
    NestedValue,
    cast::{CastFunction, CastMode, CastRegistry, CastSpec},
    vector::Vector,
};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

#[derive(Debug)]
struct CountInteger(Arc<AtomicUsize>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for CountInteger {
    fn name(&self) -> &'static str {
        "count-selected-integer"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        super::super::PrimitiveTypes.validate_type(ty)
    }
    fn validate_value(&self, _: &DataType, _: &Value, query: &QueryContext) -> Result<()> {
        query.check()
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        super::super::PrimitiveTypes.common_type(a, b)
    }
    fn compare(
        &self,
        ty: &DataType,
        a: &Value,
        b: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        self.0.fetch_add(1, AtomicOrdering::Relaxed);
        super::super::PrimitiveTypes.compare(ty, a, b, query)
    }
    fn write_key(
        &self,
        ty: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        super::super::PrimitiveTypes.write_key(ty, value, output, query)
    }
}

#[derive(Debug)]
struct CountCast(Arc<AtomicUsize>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for CountCast {
    fn name(&self) -> &'static str {
        "count-selected-dynamic-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Integer && spec.target == DataType::BigInt
    }
    fn cast(&self, value: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        self.0.fetch_add(1, AtomicOrdering::Relaxed);
        Ok(value.clone())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn variant_retains_dynamic_child_selection_across_replacement_and_batch_execution() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let new_calls = Arc::new(AtomicUsize::new(0));
    let mut types = TypeRegistry::builtins();
    types.replace("builtin.integer", Arc::new(CountInteger(calls.clone())))?;
    let variant = NestedType::Variant.data_type();
    let bound = types.bind(&variant)?;
    let mut casts = CastRegistry::builtins();
    let cast_calls = Arc::new(AtomicUsize::new(0));
    let spec = CastSpec {
        source: DataType::Integer,
        target: DataType::BigInt,
        mode: CastMode::Explicit,
    };
    casts.replace(spec.clone(), Arc::new(CountCast(cast_calls.clone())))?;
    let cast = casts.bind(&variant, &DataType::BigInt, CastMode::Explicit, &types)?;
    types.replace("builtin.integer", Arc::new(CountInteger(new_calls.clone())))?;
    casts.replace(spec, Arc::new(CountCast(new_calls.clone())))?;
    let query = QueryContext::background().with_types(Arc::new(TypeRegistry::default()));
    let a = Node::Typed(&DataType::Integer, &Value::Integer(1)).owned()?;
    let b = Node::Typed(&DataType::Integer, &Value::Integer(2)).owned()?;
    assert_eq!(bound.compare(&a, &b, &query)?, Ordering::Less);
    assert_eq!(calls.load(AtomicOrdering::Relaxed), 1);
    let batch = Vector::flat(variant.clone(), vec![a.clone(), Value::Null, b.clone()])?;
    assert_eq!(
        cast.apply_batch(&batch, &query)?
            .values()
            .cloned()
            .collect::<Vec<_>>(),
        vec![Value::Integer(1), Value::Null, Value::Integer(2)]
    );
    assert_eq!(cast_calls.load(AtomicOrdering::Relaxed), 2);
    assert_eq!(new_calls.load(AtomicOrdering::Relaxed), 0);
    let mut akey = Vec::new();
    let mut bkey = Vec::new();
    let decimal = Node::Typed(
        &DataType::Decimal { width: 3, scale: 2 },
        &Value::Decimal {
            value: 100,
            width: 3,
            scale: 2,
        },
    )
    .owned()?;
    bound.append_key(&a, &mut akey, &query)?;
    bound.append_key(&decimal, &mut bkey, &query)?;
    assert_eq!(akey, bkey);
    let invalid = NestedValue::value(
        variant,
        NestedPayload::Variant {
            data_type: DataType::Integer,
            value: Value::Null,
        },
    )?;
    assert!(bound.validate(&invalid, &query).is_err());
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn variant_exact_number_keys_cover_full_unsigned_and_signed_boundaries() -> Result<()> {
    let low = NumberKey::new(&Value::Integer(i128::MIN))?;
    let high = NumberKey::new(&Value::Unsigned(u128::MAX))?;
    assert_eq!(low.compare(&high), Ordering::Less);
    let zero = NumberKey::new(&Value::Decimal {
        value: 0,
        width: 38,
        scale: 38,
    })?;
    assert_eq!(zero, NumberKey::new(&Value::Unsigned(0))?);
    for (a, b) in [(1, 10), (12, 120), (1000, 10000), (-1, -10), (-12, -120)] {
        assert_eq!(
            NumberKey::new(&Value::Integer(a))?,
            NumberKey::new(&Value::Decimal {
                value: b,
                width: 8,
                scale: 1
            })?
        );
    }
    assert_eq!(
        NumberKey::new(&Value::Decimal {
            value: -123,
            width: 3,
            scale: 2
        })?
        .compare(&NumberKey::new(&Value::Decimal {
            value: -12,
            width: 2,
            scale: 1
        })?),
        Ordering::Less
    );
    Ok(())
}
