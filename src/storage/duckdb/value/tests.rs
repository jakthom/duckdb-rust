use super::*;
mod failures;
mod fixtures;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn literal_sessions_share_budgets_and_cannot_resume_after_failure() -> Result<()> {
    let query = QueryContext::background();
    let ty = DataType::Varchar;
    let value = Value::Varchar("abc".into());
    let bytes = encode(&ty, &value, 69, &query)?;
    for byte_limited in [false, true] {
        let mut writer = ValueCodec::new(69, query.types(), &query)?;
        if byte_limited {
            writer.state.bytes = 5;
        } else {
            writer.state.nodes = 3;
        }
        let mut output = Encoder::default();
        writer.write_typed(&mut output, &ty, &value)?;
        assert_eq!(output.0, bytes);
        assert!(matches!(
            writer.write_typed(&mut output, &ty, &value),
            Err(Error::Resource(_))
        ));
        assert_eq!(
            output.0, bytes,
            "failed member must not append a partial object"
        );
        assert!(matches!(
            writer.write_typed(&mut output, &ty, &Value::Null),
            Err(Error::Internal(_))
        ));
        assert_eq!(output.0, bytes);

        let mut reader = Reader::new([bytes.as_slice(), bytes.as_slice()].concat());
        let mut codec = ValueCodec::new(69, query.types(), &query)?;
        if byte_limited {
            codec.state.bytes = 5;
        } else {
            codec.state.nodes = 3;
        }
        assert_eq!(codec.read_typed(&mut reader)?, (ty.clone(), value.clone()));
        assert!(matches!(
            codec.read_typed(&mut reader),
            Err(Error::Resource(_))
        ));
        let position = reader.position;
        assert!(matches!(
            codec.read_typed(&mut reader),
            Err(Error::Internal(_))
        ));
        assert_eq!(
            reader.position, position,
            "failed session must not consume another member"
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn literal_sessions_retain_mixed_metadata_and_cancel_between_members() -> Result<()> {
    let interrupt = crate::parallel::InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 2048, usize::MAX)?;
    let values = [
        (DataType::UTinyInt, Value::Unsigned(255)),
        (
            DataType::Decimal {
                width: 12,
                scale: 2,
            },
            Value::Decimal {
                width: 12,
                scale: 2,
                value: 125,
            },
        ),
        (
            NestedType::List(DataType::TimestampNs).data_type(),
            Value::Null,
        ),
    ];
    let mut writer = ValueCodec::new(69, query.types(), &query)?;
    let mut output = Encoder::default();
    for (ty, value) in &values {
        writer.write_typed(&mut output, ty, value)?;
    }
    let mut reader = Reader::new(output.0.clone());
    let mut codec = ValueCodec::new(69, query.types(), &query)?;
    for value in values {
        assert_eq!(codec.read_typed(&mut reader)?, value);
    }
    assert!(reader.finished());
    let original = output.0.clone();
    interrupt.interrupt();
    assert!(matches!(
        writer.write_typed(&mut output, &DataType::Integer, &Value::Integer(1)),
        Err(Error::Interrupted)
    ));
    assert_eq!(output.0, original);
    assert!(matches!(
        codec.read_typed(&mut Reader::new(vec![])),
        Err(Error::Interrupted)
    ));
    interrupt.reset();
    assert!(matches!(
        writer.write_typed(&mut output, &DataType::Integer, &Value::Integer(1)),
        Err(Error::Internal(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn encode(ty: &DataType, value: &Value, version: u64, query: &QueryContext) -> Result<Vec<u8>> {
    let mut output = Encoder::default();
    write_typed(&mut output, ty, value, version, query.types(), query)?;
    Ok(output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decode(bytes: &[u8], version: u64, query: &QueryContext) -> Result<(DataType, Value)> {
    let mut reader = Reader::new(bytes.to_vec());
    let result = read_typed(&mut reader, version, query.types(), query)?;
    assert!(reader.finished());
    Ok(result)
}

#[derive(Clone)]
enum RawTypeArgument {
    Type {
        alias: Option<String>,
        name: String,
        children: Vec<RawTypeArgument>,
    },
    Integer(i64),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl RawTypeArgument {
    fn ty(name: &str, children: Vec<Self>) -> Self {
        Self::Type {
            alias: None,
            name: name.into(),
            children,
        }
    }

    fn field(alias: &str, name: &str, children: Vec<Self>) -> Self {
        Self::Type {
            alias: Some(alias.into()),
            name: name.into(),
            children,
        }
    }

    fn write(&self, output: &mut Encoder) -> Result<()> {
        match self {
            Self::Type {
                alias,
                name,
                children,
            } => {
                output.property(100, 21);
                output.property(101, 207);
                if let Some(alias) = alias {
                    output.field(102);
                    output.string(alias)?;
                }
                output.field(202);
                output.string(name)?;
                if !children.is_empty() {
                    output.property(203, children.len() as u64);
                    for child in children {
                        output.boolean(true);
                        child.write(output)?;
                    }
                }
                output.end();
            }
            Self::Integer(value) => {
                output.property(100, 7);
                output.property(101, 75);
                output.field(200);
                output.field(100);
                output.property(100, 14);
                output.end();
                output.field(101);
                output.boolean(false);
                output.field(102);
                output.signed(*value);
                output.end();
                output.end();
            }
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn unbound_null(root: &RawTypeArgument) -> Result<Vec<u8>> {
    let mut output = Encoder::default();
    output.field(100);
    output.property(100, 4);
    output.field(101);
    output.boolean(true);
    output.property(100, 7);
    output.field(204);
    output.boolean(true);
    root.write(&mut output)?;
    output.end();
    output.end();
    output.field(101);
    output.boolean(true);
    output.end();
    Ok(output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn unbound_literal_types_resolve_nested_shapes_and_reject_invalid_metadata() -> Result<()> {
    let query = QueryContext::background();
    let integer = || RawTypeArgument::ty("INTEGER", vec![]);
    let cases = [
        (
            RawTypeArgument::ty("LIST", vec![integer()]),
            NestedType::List(DataType::Integer).data_type(),
        ),
        (
            RawTypeArgument::ty("ARRAY", vec![integer(), RawTypeArgument::Integer(3)]),
            NestedType::Array {
                element: DataType::Integer,
                length: 3,
            }
            .data_type(),
        ),
        (
            RawTypeArgument::ty(
                "STRUCT",
                vec![
                    RawTypeArgument::field(
                        "amount",
                        "DECIMAL",
                        vec![RawTypeArgument::Integer(12), RawTypeArgument::Integer(2)],
                    ),
                    RawTypeArgument::field(
                        "items",
                        "LIST",
                        vec![RawTypeArgument::ty("TIMESTAMP_NS", vec![])],
                    ),
                ],
            ),
            NestedType::Struct(vec![
                (
                    "amount".into(),
                    DataType::Decimal {
                        width: 12,
                        scale: 2,
                    },
                ),
                (
                    "items".into(),
                    NestedType::List(DataType::TimestampNs).data_type(),
                ),
            ])
            .data_type(),
        ),
    ];
    for (raw, expected) in cases {
        assert_eq!(
            decode(&unbound_null(&raw)?, 69, &query)?,
            (expected, Value::Null)
        );
    }

    let zero_array = RawTypeArgument::ty("ARRAY", vec![integer(), RawTypeArgument::Integer(0)]);
    assert!(matches!(
        decode(&unbound_null(&zero_array)?, 69, &query),
        Err(Error::Bind(_))
    ));
    let unnamed_struct = RawTypeArgument::ty("STRUCT", vec![integer()]);
    assert!(matches!(
        decode(&unbound_null(&unnamed_struct)?, 69, &query),
        Err(Error::Unsupported(_))
    ));
    let mut missing_metadata = unbound_null(&integer())?;
    assert_eq!(&missing_metadata[..8], &[100, 0, 100, 0, 4, 101, 0, 1]);
    missing_metadata[7] = 0;
    assert!(matches!(
        decode(&missing_metadata, 69, &query),
        Err(Error::Corrupt(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn unbound_literal_types_share_value_budgets_depth_and_cancellation() -> Result<()> {
    let raw = RawTypeArgument::ty("INTEGER", vec![]);
    let bytes = unbound_null(&raw)?;
    let query = QueryContext::background();
    let mut codec = ValueCodec::new(69, query.types(), &query)?;
    codec.state.bytes = 6;
    assert!(matches!(
        codec.read_typed(&mut Reader::new(bytes.clone())),
        Err(Error::Resource(_))
    ));

    let mut deep = raw;
    for _ in 0..65 {
        deep = RawTypeArgument::ty("LIST", vec![deep]);
    }
    assert!(matches!(
        decode(&unbound_null(&deep)?, 69, &query),
        Err(Error::Resource(_))
    ));

    let interrupt = crate::parallel::InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 2048, usize::MAX)?;
    let mut codec = ValueCodec::new(69, query.types(), &query)?;
    interrupt.interrupt();
    assert!(matches!(
        codec.read_typed(&mut Reader::new(bytes)),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn typed_null_and_inherited_children_round_trip_without_casts() -> Result<()> {
    let query = QueryContext::background();
    let ty = NestedType::Struct(vec![
        (
            "items".into(),
            NestedType::List(DataType::Decimal {
                width: 12,
                scale: 2,
            })
            .data_type(),
        ),
        ("null".into(), DataType::TimeTz),
    ])
    .data_type();
    let child = NestedType::List(DataType::Decimal {
        width: 12,
        scale: 2,
    })
    .data_type();
    let value = NestedValue::value(
        ty.clone(),
        NestedPayload::Struct(vec![
            NestedValue::value(
                child,
                NestedPayload::Sequence(vec![
                    Value::Decimal {
                        value: 12345,
                        width: 12,
                        scale: 2,
                    },
                    Value::Null,
                ]),
            )?,
            Value::Null,
        ]),
    )?;
    for version in 64..=69 {
        for value in [&value, &Value::Null] {
            let mut output = Encoder::default();
            write_typed(&mut output, &ty, value, version, query.types(), &query)?;
            let mut reader = Reader::new(output.0);
            let (decoded_type, decoded) = read_typed(&mut reader, version, query.types(), &query)?;
            assert_eq!(decoded_type, ty);
            assert_eq!(&decoded, value);
            assert!(reader.finished());
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn typed_scalar_and_nested_sql_values_preserve_exact_payloads() -> Result<()> {
    let query = QueryContext::background();
    let mut connection = crate::Database::memory()?.connect();
    for expression in [
        "[NULL,NULL]",
        "[]",
        "[1,NULL]::INTEGER[2]",
        "map(['','A','a'],[[1,NULL],[],NULL])",
        "[union_value(i:=42)::UNION(i INTEGER,s VARCHAR),union_value(s:=NULL::VARCHAR)::UNION(i INTEGER,s VARCHAR),NULL]",
        "(1::UTINYINT,1.25::DECIMAL(12,2),NULL)",
        "{'f': 'NaN'::FLOAT, 'd': '-0.0'::DOUBLE, 'n': (-0.5::DOUBLE)::BIGNUM, 'e': 'a'::ENUM('', 'A', 'a')}",
        "{'v': {'d': 1.25::DECIMAL(12,2), 'l': [1,NULL]}::VARIANT, 'l': [1::VARIANT,'x'::VARIANT,NULL]}",
        "map(['','A','a'], [1,NULL,3])::VARIANT",
        "{'t': TIME '24:00:00', 'z': TIMETZ '00:00:00+00', 'i': INTERVAL '1 month -2 days 3 microseconds'}",
        "{'b': from_hex('005c222780ff'), 'bit': '100000001'::BIT, 'u': 'ffffffff-ffff-ffff-ffff-ffffffffffff'::UUID}",
    ] {
        let result = connection.query(&format!("SELECT {expression}"))?;
        let ty = &result.columns[0].data_type;
        let bytes = encode(ty, &result.rows[0][0], 69, &query)?;
        let (decoded_type, decoded) = decode(&bytes, 69, &query)?;
        assert_eq!(&decoded_type, ty, "{expression}");
        assert_eq!(encode(ty, &decoded, 69, &query)?, bytes, "{expression}");
    }
    for (ty, value) in [
        (DataType::Float, Value::Float(f32::from_bits(0x7fc01234))),
        (
            DataType::Double,
            Value::Double(f64::from_bits(0xfff8000000001234)),
        ),
        (DataType::Double, Value::Double(-0.0)),
    ] {
        let child = NestedType::List(ty.clone()).data_type();
        let cases = [
            (ty.clone(), value.clone()),
            (
                child.clone(),
                NestedValue::value(
                    child,
                    NestedPayload::Sequence(vec![value.clone(), Value::Null]),
                )?,
            ),
            (
                NestedType::Variant.data_type(),
                NestedValue::value(
                    NestedType::Variant.data_type(),
                    NestedPayload::Variant {
                        data_type: ty,
                        value,
                    },
                )?,
            ),
        ];
        for (ty, value) in cases {
            let bytes = encode(&ty, &value, 69, &query)?;
            let (_, decoded) = decode(&bytes, 69, &query)?;
            assert_eq!(encode(&ty, &decoded, 69, &query)?, bytes);
        }
    }
    Ok(())
}
