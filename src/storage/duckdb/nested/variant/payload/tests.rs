use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn list(ty: &DataType, values: Vec<Value>) -> Result<Value> {
    NestedValue::value(ty.clone(), NestedPayload::Sequence(values))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn records(ty: &DataType, values: Vec<Vec<Value>>) -> Result<Value> {
    let DataType::Nested(metadata) = ty else {
        unreachable!()
    };
    let NestedType::List(child) = metadata.as_ref() else {
        unreachable!()
    };
    list(
        ty,
        values
            .into_iter()
            .map(|values| NestedValue::value(child.clone(), NestedPayload::Struct(values)))
            .collect::<Result<_>>()?,
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn row(
    keys: &[&str],
    children: &[(Option<u32>, u32)],
    values: &[(u8, u32)],
    data: Vec<u8>,
) -> Result<Value> {
    let fields = super::super::fields();
    let values = vec![
        list(
            &fields[0].1,
            keys.iter()
                .map(|key| Value::Varchar((*key).into()))
                .collect(),
        )?,
        records(
            &fields[1].1,
            children
                .iter()
                .map(|(key, value)| {
                    vec![
                        key.map_or(Value::Null, |key| Value::Unsigned(u128::from(key))),
                        Value::Unsigned(u128::from(*value)),
                    ]
                })
                .collect(),
        )?,
        records(
            &fields[2].1,
            values
                .iter()
                .map(|(tag, offset)| {
                    vec![
                        Value::Unsigned(u128::from(*tag)),
                        Value::Unsigned(u128::from(*offset)),
                    ]
                })
                .collect(),
        )?,
        Value::Blob(data),
    ];
    NestedValue::value(
        super::super::unshredded_type(),
        NestedPayload::Struct(values),
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decoded(value: &Value, budget: &mut Budget) -> Result<Typed> {
    Unshredded::new(value, budget)?
        .ok_or_else(|| corrupt("test row unexpectedly NULL"))?
        .decode(0, budget)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_variant_payload_retains_unsigned_decimal_width_and_child_nulls() -> Result<()> {
    for (tag, ty, bytes, value) in [
        (3, DataType::TinyInt, vec![128], Value::Integer(-128)),
        (
            7,
            DataType::HugeInt,
            i128::MIN.to_le_bytes().to_vec(),
            Value::Integer(i128::MIN),
        ),
        (
            12,
            DataType::UHugeInt,
            u128::MAX.to_le_bytes().to_vec(),
            Value::Unsigned(u128::MAX),
        ),
        (
            13,
            DataType::Float,
            1.5f32.to_le_bytes().to_vec(),
            Value::Float(1.5),
        ),
        (
            14,
            DataType::Double,
            (-2.5f64).to_le_bytes().to_vec(),
            Value::Double(-2.5),
        ),
    ] {
        assert_eq!(
            decoded(&row(&[], &[], &[(tag, 0)], bytes)?, &mut Budget::new())?,
            (ty, value)
        );
    }
    for (width, size) in [(4, 2), (9, 4), (18, 8), (38, 16)] {
        let mut data = vec![width, 1];
        data.extend_from_slice(&123i128.to_le_bytes()[..size]);
        assert_eq!(
            decoded(&row(&[], &[], &[(15, 0)], data)?, &mut Budget::new())?,
            (
                DataType::Decimal { width, scale: 1 },
                Value::Decimal {
                    value: 123,
                    width,
                    scale: 1
                }
            )
        );
        let mut data = vec![width, 1];
        data.extend(vec![0; size - 1]);
        data.push(128);
        assert!(matches!(
            decoded(&row(&[], &[], &[(15, 0)], data)?, &mut Budget::new()),
            Err(Error::Corrupt(_))
        ));
    }
    let mut data = vec![2, 0];
    data.extend_from_slice(&42i32.to_le_bytes());
    let source = row(
        &[],
        &[(None, 1), (None, 2)],
        &[(30, 0), (5, 2), (0, 6)],
        data,
    )?;
    let (ty, value) = decoded(&source, &mut Budget::new())?;
    assert_eq!(
        ty,
        NestedType::List(NestedType::Variant.data_type()).data_type()
    );
    let children = sequence(&value)?;
    assert!(children[1].is_null());
    assert_eq!(
        crate::common::variant::Node::Typed(&NestedType::Variant.data_type(), &children[0])
            .materialized(0, &|| Ok(()))?,
        (DataType::Integer, Value::Integer(42))
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_variant_payload_rejects_bad_references_cycles_truncation_and_budget_exhaustion()
-> Result<()> {
    let cases = [
        row(&[], &[(None, 1)], &[(30, 0)], vec![1, 0])?,
        row(&[], &[], &[(35, 0)], vec![])?,
        row(&[], &[], &[(1, 1)], vec![])?,
        row(&[], &[], &[(16, 0)], vec![255, 255, 255, 255, 16])?,
        row(&[], &[], &[(16, 0)], vec![5, 1])?,
        row(&[], &[], &[(16, 0)], vec![1, 255])?,
        row(&[], &[(None, 0)], &[(30, 0)], vec![1, 0])?,
        row(&[], &[(None, 1)], &[(29, 0), (1, 2)], vec![1, 0])?,
        row(&["a"], &[(Some(0), 1)], &[(30, 0), (1, 2)], vec![1, 0])?,
        row(&[], &[], &[(15, 0)], vec![39, 0])?,
        row(&[], &[], &[(31, 0)], vec![3, 128, 0, 1])?,
        row(&[], &[], &[(32, 0)], vec![2, 8, 255])?,
        row(&[], &[], &[(26, 0)], vec![255; 8])?,
    ];
    for source in cases {
        assert!(matches!(
            decoded(&source, &mut Budget::new()),
            Err(Error::Corrupt(_))
        ));
    }
    let source = row(&[], &[], &[(16, 0)], vec![1, b'a'])?;
    assert!(matches!(
        decoded(
            &source,
            &mut Budget {
                nodes: 0,
                bytes: 64,
                query: None,
            }
        ),
        Err(Error::Resource(_))
    ));
    assert!(matches!(
        decoded(
            &source,
            &mut Budget {
                nodes: 64,
                bytes: 0,
                query: None,
            }
        ),
        Err(Error::Resource(_))
    ));
    let mut budget = Budget::new();
    let payload = Unshredded::new(&source, &mut budget)?.unwrap();
    assert!(matches!(
        payload.decode_from(0, 65, &mut budget),
        Err(Error::Resource(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_variant_shredded_missing_null_and_selected_typed_value_are_distinct() -> Result<()> {
    let ty = NestedType::Struct(vec![
        ("typed_value".into(), DataType::Integer),
        ("untyped_value_index".into(), DataType::UInteger),
    ])
    .data_type();
    for (typed, index, expected) in [
        (Value::Null, Value::Unsigned(0), None),
        (
            Value::Null,
            Value::Null,
            Some((DataType::Null, Value::Null)),
        ),
        // A present scalar never reads the unused OBJECT overlay reference.
        (
            Value::Integer(7),
            Value::Unsigned(u32::MAX as u128),
            Some((DataType::Integer, Value::Integer(7))),
        ),
    ] {
        let value = NestedValue::value(ty.clone(), NestedPayload::Struct(vec![typed, index]))?;
        assert_eq!(
            super::super::shredded::decode(&ty, &value, None, 0, &mut Budget::new())?,
            expected
        );
    }
    let malformed = NestedType::Struct(vec![("wrong".into(), DataType::Integer)]).data_type();
    assert!(matches!(
        super::super::validate_shredded_metadata(&malformed),
        Err(Error::Corrupt(_))
    ));
    let value = NestedValue::value(
        ty.clone(),
        NestedPayload::Struct(vec![Value::Null, Value::Unsigned(1)]),
    )?;
    assert!(matches!(
        super::super::shredded::decode(&ty, &value, None, 0, &mut Budget::new()),
        Err(Error::Corrupt(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_variant_reconstruction_orders_leftover_objects_only_for_shredded_columns() -> Result<()> {
    let source = row(
        &["z", "a"],
        &[(Some(0), 1), (Some(1), 2)],
        &[(29, 0), (1, 2), (2, 2)],
        vec![2, 0],
    )?;
    let mut budget = Budget::new();
    let stored = Unshredded::new(&source, &mut budget)?.unwrap();
    let (_, value) = stored.decode(0, &mut budget)?;
    assert_eq!(value.to_string(), "{'z': true, 'a': false}");
    let (_, value) = stored.with_ordered_objects().decode(0, &mut budget)?;
    assert_eq!(value.to_string(), "{'a': false, 'z': true}");
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_variant_objects_preserve_empty_case_distinct_and_zero_byte_names() -> Result<()> {
    // Source-backed canonical row, not an independently JSON-produced file:
    // the pinned CLI has no JSON extension. The raw key strings are UTF-8,
    // not SQL identifiers, including a zero byte and the empty string.
    let mut data = vec![4, 0];
    for value in [1i32, 2, 3] {
        data.extend_from_slice(&value.to_le_bytes());
    }
    let source = row(
        &["", "A", "a", "nul\0key"],
        &[(Some(0), 1), (Some(1), 2), (Some(2), 3), (Some(3), 4)],
        &[(29, 0), (5, 2), (5, 6), (0, 10), (5, 10)],
        data,
    )?;
    let (ty, value) = decoded(&source, &mut Budget::new())?;
    let DataType::Nested(metadata) = &ty else {
        unreachable!()
    };
    let NestedType::Object(fields) = metadata.as_ref() else {
        panic!("native logical OBJECT must not become SQL STRUCT")
    };
    assert_eq!(
        fields
            .iter()
            .map(|field| field.0.as_str())
            .collect::<Vec<_>>(),
        ["", "A", "a", "nul\0key"]
    );
    let value = envelope((ty, value))?;
    let mut connection = crate::Database::memory()?.connect();
    let query = connection.prepare("SELECT variant_extract($1,$2::VARCHAR)::INTEGER")?;
    for (name, expected) in [
        ("", Value::Integer(1)),
        ("A", Value::Integer(2)),
        ("a", Value::Null),
        ("nul\0key", Value::Integer(3)),
        ("missing", Value::Null),
    ] {
        assert_eq!(
            connection
                .execute_prepared(&query, &[value.clone(), Value::Varchar(name.into())])?
                .rows,
            vec![vec![expected]]
        );
    }
    for keys in [&["a"][..], &["a", "a"][..]] {
        let source = row(
            keys,
            &[(Some(0), 1), (Some((keys.len() - 1) as u32), 2)],
            &[(29, 0), (1, 2), (2, 2)],
            vec![2, 0],
        )?;
        assert!(matches!(
            decoded(&source, &mut Budget::new()),
            Err(Error::Corrupt(_))
        ));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_variant_shredded_objects_merge_exact_names_and_reject_duplicate_members() -> Result<()> {
    let typed_type = NestedType::Struct(vec![("A".into(), DataType::Integer)]).data_type();
    let ty = NestedType::Struct(vec![
        ("typed_value".into(), typed_type.clone()),
        ("untyped_value_index".into(), DataType::UInteger),
    ])
    .data_type();
    let typed = NestedValue::value(typed_type, NestedPayload::Struct(vec![Value::Integer(7)]))?;
    let value = NestedValue::value(
        ty.clone(),
        NestedPayload::Struct(vec![typed, Value::Unsigned(1)]),
    )?;
    for name in ["a", "A", ""] {
        let source = row(&[name], &[(Some(0), 1)], &[(29, 0), (1, 2)], vec![1, 0])?;
        let mut budget = Budget::new();
        let unshredded = Unshredded::new(&source, &mut budget)?.unwrap();
        let decoded =
            super::super::shredded::decode(&ty, &value, Some(&unshredded), 0, &mut budget);
        if name == "A" {
            assert!(matches!(decoded, Err(Error::Corrupt(_))));
        } else {
            let (ty, value) = decoded?.unwrap();
            assert!(
                matches!(ty, DataType::Nested(metadata) if matches!(metadata.as_ref(), NestedType::Object(_)))
            );
            assert_eq!(
                value.to_string(),
                if name.is_empty() {
                    "{'': true, 'A': 7}"
                } else {
                    "{'A': 7, 'a': true}"
                }
            );
        }
    }
    Ok(())
}
