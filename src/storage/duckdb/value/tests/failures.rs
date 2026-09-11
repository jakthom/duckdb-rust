use super::*;
use crate::common::type_registry::{KeyWriter, PrimitiveTypes, TypeAdapter};
use std::{
    cmp::Ordering,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn raw(
    ty: &DataType,
    version: u64,
    body: impl FnOnce(&mut Encoder, &mut State<'_>) -> Result<()>,
) -> Result<Vec<u8>> {
    let query = QueryContext::background();
    let mut state = State::new(version, query.types(), &query)?;
    let mut output = Encoder::default();
    output.field(100);
    metadata::write(&mut output, ty, 0, &mut state)?;
    output.field(101);
    output.boolean(false);
    output.field(102);
    body(&mut output, &mut state)?;
    output.end();
    Ok(output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn typed_value_rejects_malformed_widths_child_types_shapes_and_versions() -> Result<()> {
    let query = QueryContext::background();
    for (ty, coefficient) in [
        (DataType::TinyInt, 128),
        (
            DataType::Decimal { width: 4, scale: 1 },
            i128::from(i16::MIN),
        ),
        (
            DataType::Decimal { width: 9, scale: 1 },
            i128::from(i32::MIN),
        ),
        (
            DataType::Decimal {
                width: 18,
                scale: 1,
            },
            i128::from(i64::MIN),
        ),
        (
            DataType::Decimal {
                width: 38,
                scale: 1,
            },
            i128::MIN,
        ),
    ] {
        let bytes = raw(&ty, 69, |output, _| {
            if super::super::super::primitive::width(&ty)? == 16 {
                output.signed((coefficient >> 64) as i64);
                output.unsigned(coefficient as u64);
            } else {
                output.signed(coefficient as i64);
            }
            Ok(())
        })?;
        assert!(
            matches!(decode(&bytes, 69, &query), Err(Error::Corrupt(_))),
            "{ty}"
        );
    }
    let ty = NestedType::List(DataType::Integer).data_type();
    let mismatched = raw(&ty, 64, |output, state| {
        output.property(100, 1);
        write::value(output, &DataType::Varchar, &Value::Null, true, 1, state)?;
        output.end();
        Ok(())
    })?;
    assert!(matches!(
        decode(&mismatched, 64, &query),
        Err(Error::Corrupt(_))
    ));
    for ty in [
        NestedType::Array {
            element: DataType::Integer,
            length: 2,
        }
        .data_type(),
        NestedType::Struct(vec![("a".into(), DataType::Integer)]).data_type(),
    ] {
        let bytes = raw(&ty, 69, |output, _| {
            output.property(100, 0);
            output.end();
            Ok(())
        })?;
        assert!(matches!(decode(&bytes, 69, &query), Err(Error::Corrupt(_))));
    }
    let bytes = encode(&DataType::Integer, &Value::Integer(42), 69, &query)?;
    for length in 0..bytes.len() {
        assert!(decode(&bytes[..length], 69, &query).is_err());
    }
    assert!(decode(&[101, 0, 1, 255, 255], 69, &query).is_err());
    for version in [0, 63, 70, u64::MAX] {
        assert!(matches!(
            decode(&bytes, version, &query),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            encode(&DataType::Integer, &Value::Integer(42), version, &query),
            Err(Error::Unsupported(_))
        ));
    }
    for (ty, minimum) in [
        (NestedType::Variant.data_type(), 68),
        (NestedType::Tuple(vec![]).data_type(), 69),
    ] {
        let bytes = encode(&ty, &Value::Null, minimum, &query)?;
        assert!(matches!(
            decode(&bytes, minimum - 1, &query),
            Err(Error::Unsupported(_))
        ));
        assert!(matches!(
            encode(&ty, &Value::Null, minimum - 1, &query),
            Err(Error::Unsupported(_))
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn typed_union_rejects_invalid_tags_and_inactive_non_null_members() -> Result<()> {
    let query = QueryContext::background();
    let ty = NestedType::Union(vec![
        ("i".into(), DataType::Integer),
        ("s".into(), DataType::Varchar),
    ])
    .data_type();
    for (tag, i, s) in [
        (Value::Null, Value::Null, Value::Null),
        (Value::Unsigned(2), Value::Null, Value::Null),
        (
            Value::Unsigned(0),
            Value::Integer(42),
            Value::Varchar("inactive".into()),
        ),
    ] {
        let bytes = raw(&ty, 69, |output, state| {
            output.property(100, 3);
            for (ty, value) in [
                (DataType::UTinyInt, tag),
                (DataType::Integer, i),
                (DataType::Varchar, s),
            ] {
                write::value(output, &ty, &value, false, 1, state)?;
            }
            output.end();
            Ok(())
        })?;
        assert!(matches!(decode(&bytes, 69, &query), Err(Error::Corrupt(_))));
    }
    Ok(())
}

#[derive(Debug)]
struct Selected(Arc<AtomicUsize>, u8);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for Selected {
    fn name(&self) -> &'static str {
        "typed-value-selected-integer"
    }
    fn validate_type(&self, ty: &DataType) -> Result<()> {
        if self.1 == 3 {
            return Err(Error::Internal("selected literal binding failure".into()));
        }
        PrimitiveTypes.validate_type(ty)
    }
    fn validate_value(&self, ty: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
        self.0.fetch_add(1, AtomicOrdering::Relaxed);
        if self.1 == 1 {
            return Err(Error::Resource("selected literal child failure".into()));
        }
        if self.1 == 2 {
            return Err(Error::Internal(
                "selected literal validation failure".into(),
            ));
        }
        PrimitiveTypes.validate_value(ty, value, query)
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(a, b)
    }
    fn compare(&self, _: &DataType, _: &Value, _: &Value, _: &QueryContext) -> Result<Ordering> {
        panic!("literal codec requested SQL comparison")
    }
    fn write_key(
        &self,
        _: &DataType,
        _: &Value,
        _: &mut KeyWriter<'_>,
        _: &QueryContext,
    ) -> Result<()> {
        panic!("literal codec requested SQL keys")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn typed_value_uses_explicit_selected_services_and_keeps_failures_atomic() -> Result<()> {
    let query = QueryContext::background().with_types(Arc::new(TypeRegistry::default()));
    let mut types = TypeRegistry::builtins();
    let calls = Arc::new(AtomicUsize::new(0));
    types.replace("builtin.integer", Arc::new(Selected(calls.clone(), 0)))?;
    let ty = NestedType::Variant.data_type();
    let value = NestedValue::value(
        ty.clone(),
        NestedPayload::Variant {
            data_type: DataType::Integer,
            value: Value::Integer(42),
        },
    )?;
    let mut output = Encoder::default();
    write_typed(&mut output, &ty, &value, 69, &types, &query)?;
    let (_, decoded) = read_typed(&mut Reader::new(output.0.clone()), 69, &types, &query)?;
    assert_eq!(decoded, value);
    assert!(calls.load(AtomicOrdering::Relaxed) > 1);
    types.replace("builtin.integer", Arc::new(Selected(calls.clone(), 1)))?;
    assert!(
        matches!(read_typed(&mut Reader::new(output.0.clone()),69,&types,&query),Err(Error::Resource(message)) if message=="selected literal child failure")
    );
    let before = output.0.clone();
    assert!(
        matches!(write_typed(&mut output,&ty,&value,69,&types,&query),Err(Error::Resource(message)) if message=="selected literal child failure")
    );
    assert_eq!(output.0, before);
    for mode in [2, 3] {
        types.replace("builtin.integer", Arc::new(Selected(calls.clone(), mode)))?;
        assert!(matches!(
            read_typed(&mut Reader::new(before.clone()), 69, &types, &query),
            Err(Error::Internal(_))
        ));
        assert!(matches!(
            write_typed(&mut output, &ty, &value, 69, &types, &query),
            Err(Error::Internal(_))
        ));
        assert_eq!(output.0, before);
    }
    assert!(
        read_typed(
            &mut Reader::new(before),
            69,
            &TypeRegistry::default(),
            &query
        )
        .is_err()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn typed_value_has_shared_nested_budgets_and_cancellation() -> Result<()> {
    let query = QueryContext::background();
    let ty = NestedType::List(DataType::Varchar).data_type();
    let value = NestedValue::value(
        ty.clone(),
        NestedPayload::Sequence(vec![
            Value::Varchar("abc".into()),
            Value::Varchar("def".into()),
        ]),
    )?;
    let bytes = encode(&ty, &value, 69, &query)?;
    let mut state = State::new(69, query.types(), &query)?;
    state.bytes = 5;
    assert!(matches!(
        read::value(&mut Reader::new(bytes.clone()), None, 0, &mut state),
        Err(Error::Resource(_))
    ));
    let mut state = State::new(69, query.types(), &query)?;
    state.nodes = 5;
    assert!(matches!(
        write::value(&mut Encoder::default(), &ty, &value, true, 0, &mut state),
        Err(Error::Resource(_))
    ));
    let mut state = State::new(69, query.types(), &query)?;
    assert!(matches!(
        read::value(&mut Reader::new(bytes.clone()), None, 65, &mut state),
        Err(Error::Resource(_))
    ));
    let handle = crate::parallel::InterruptHandle::default();
    let interrupted = QueryContext::new(handle.clone(), None, 2, MAX_NODES)?;
    handle.interrupt();
    assert!(matches!(
        decode(&bytes, 69, &interrupted),
        Err(Error::Interrupted)
    ));
    assert!(matches!(
        encode(&ty, &value, 69, &interrupted),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn legacy_unnamed_record_identity_is_read_without_downgrading_new_writes() -> Result<()> {
    let query = QueryContext::background();
    let ty = NestedType::Tuple(vec![DataType::Integer]).data_type();
    let value = NestedValue::value(ty.clone(), NestedPayload::Struct(vec![Value::Null]))?;
    let mut bytes = encode(&ty, &value, 69, &query)?;
    assert_eq!(bytes[4], 110);
    bytes[4] = 100; // C++ legacy unnamed STRUCT spelling, not logical TUPLE 110.
    assert_eq!(decode(&bytes, 64, &query)?, (ty.clone(), value.clone()));
    assert!(matches!(
        encode(&ty, &value, 64, &query),
        Err(Error::Unsupported(_))
    ));
    let ty = NestedType::List(DataType::Integer).data_type();
    let value = NestedValue::value(
        ty.clone(),
        NestedPayload::Sequence(vec![Value::Null, Value::Integer(42)]),
    )?;
    let legacy = encode(&ty, &value, 64, &query)?;
    assert_eq!(decode(&legacy, 69, &query)?, (ty, value));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn typed_variant_bridge_shares_bytes_and_rejects_branch_invalid_payloads() -> Result<()> {
    use super::super::super::nested::variant::{value as bridge, wal};
    let query = QueryContext::background();
    let ty = NestedType::Variant.data_type();
    let selected = query.types().bind(&ty)?;
    let value = NestedValue::value(
        ty.clone(),
        NestedPayload::Variant {
            data_type: DataType::Varchar,
            value: Value::Varchar("abc".into()),
        },
    )?;
    let mut nodes = MAX_NODES;
    let mut bytes = 7;
    let physical = bridge::encode(&value, &selected, 0, &mut nodes, &mut bytes, &query)?;
    assert!(bytes < 4);
    assert!(matches!(
        bridge::encode(&value, &selected, 0, &mut nodes, &mut bytes, &query),
        Err(Error::Resource(_))
    ));
    let mut nodes = MAX_NODES;
    let mut bytes = 5;
    bridge::decode(&physical, &selected, 0, &mut nodes, &mut bytes, &query)?;
    assert_eq!(bytes, 2);
    assert!(matches!(
        bridge::decode(&physical, &selected, 0, &mut nodes, &mut bytes, &query),
        Err(Error::Resource(_))
    ));
    for (tag, offset) in [(99, 0), (0, 0), (25, 999)] {
        let Value::Nested(mut physical) = physical.clone() else {
            unreachable!()
        };
        let NestedPayload::Struct(fields) = &mut Arc::make_mut(&mut physical).payload else {
            unreachable!()
        };
        let Value::Nested(descriptors) = &mut fields[2] else {
            unreachable!()
        };
        let NestedPayload::Sequence(descriptors) = &mut Arc::make_mut(descriptors).payload else {
            unreachable!()
        };
        let Value::Nested(descriptor) = &mut descriptors[0] else {
            unreachable!()
        };
        let NestedPayload::Struct(descriptor) = &mut Arc::make_mut(descriptor).payload else {
            unreachable!()
        };
        descriptor[0] = Value::Unsigned(tag);
        descriptor[1] = Value::Unsigned(offset);
        let physical = Value::Nested(physical);
        let bytes = raw(&ty, 69, |output, state| {
            let DataType::Nested(metadata) = wal::data_type() else {
                unreachable!()
            };
            let NestedType::Struct(types) = metadata.as_ref() else {
                unreachable!()
            };
            let Value::Nested(value) = &physical else {
                unreachable!()
            };
            let NestedPayload::Struct(fields) = &value.payload else {
                unreachable!()
            };
            output.property(100, 4);
            for ((_, ty), value) in types.iter().zip(fields) {
                write::value(output, ty, value, false, 1, state)?;
            }
            output.end();
            Ok(())
        })?;
        assert!(matches!(decode(&bytes, 69, &query), Err(Error::Corrupt(_))));
    }
    Ok(())
}
