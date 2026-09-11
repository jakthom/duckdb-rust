use super::*;
use crate::common::{DataType, Value};
use std::io::Write;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn encode(expression: &StoredExpression, version: u64, query: &QueryContext) -> Result<Vec<u8>> {
    let mut output = Encoder::default();
    write(&mut output, expression, version, query)?;
    Ok(output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decode(bytes: Vec<u8>, version: u64, query: &QueryContext) -> Result<StoredExpression> {
    let mut reader = Reader::new(bytes);
    let result = read(&mut reader, version, query)?;
    assert!(reader.finished());
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn header(bytes: Vec<u8>) -> Result<(u64, u64)> {
    let mut reader = Reader::new(bytes);
    reader.field(100)?;
    let class = reader.unsigned()?;
    reader.field(101)?;
    Ok((class, reader.unsigned()?))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn column_reference(names: &[&[u8]]) -> Vec<u8> {
    let mut wire = Encoder::default();
    wire.property(100, 4);
    wire.property(101, 203);
    wire.property(200, names.len() as u64);
    for name in names {
        wire.blob(name);
    }
    wire.end();
    wire.0
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn wire_bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_current_timestamp_node_matches_both_pins_and_rejects_other_column_refs() -> Result<()> {
    let manifests = [
        include_str!("../../../../test/data/duckdb/current-timestamp-development/manifest.json"),
        include_str!("../../../../test/data/duckdb/current-timestamp-release/manifest.json"),
    ];
    let query = QueryContext::background();
    for manifest in manifests {
        let manifest: serde_json::Value = serde_json::from_str(manifest).unwrap();
        for (fixture, expected_name) in [
            ("get_call", "get_current_timestamp"),
            ("now_call", "now"),
            ("transaction_call", "transaction_timestamp"),
        ] {
            let wire = wire_bytes(
                manifest["parsed_expressions"][fixture]["wire_hex"]
                    .as_str()
                    .unwrap(),
            );
            let expression = decode(wire.clone(), 68, &query)?;
            assert!(
                matches!(&expression.kind, StoredExpressionKind::Function { name, arguments, .. }
                    if name == &[expected_name] && arguments.is_empty())
            );
            assert_eq!(encode(&expression, 68, &query)?, wire);
        }

        let wire = wire_bytes(
            manifest["parsed_expressions"]["keyword"]["wire_hex"]
                .as_str()
                .unwrap(),
        );
        let decoded = decode(wire.clone(), 68, &query)?;
        assert!(matches!(
            &decoded.kind,
            StoredExpressionKind::CurrentTimestamp
        ));
        assert_eq!(decoded.alias, None);
        assert_eq!(decoded.source_span.unwrap().offset, 7);
        assert_eq!(encode(&decoded, 68, &query)?, wire);
        for end in 0..wire.len() {
            assert!(decode(wire[..end].to_vec(), 68, &query).is_err());
        }
    }

    let expression = StoredExpression {
        alias: None,
        source_span: None,
        kind: StoredExpressionKind::CurrentTimestamp,
    };
    let canonical = encode(&expression, 68, &query)?;
    assert_eq!(canonical, column_reference(&[b"CURRENT_TIMESTAMP"]));
    assert_eq!(decode(canonical, 68, &query)?, expression);

    let mut missing = Encoder::default();
    missing.property(100, 4);
    missing.property(101, 203);
    missing.end();
    assert!(matches!(
        decode(missing.0, 68, &query),
        Err(Error::Corrupt(_))
    ));
    assert!(matches!(
        decode(column_reference(&[]), 68, &query),
        Err(Error::Corrupt(_))
    ));
    assert!(matches!(
        decode(
            column_reference(&[b"main", b"current_timestamp"]),
            68,
            &query
        ),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        decode(column_reference(&[b"current_date"]), 68, &query),
        Err(Error::Unsupported(_))
    ));
    assert!(matches!(
        decode(column_reference(&[b""]), 68, &query),
        Err(Error::Corrupt(_))
    ));

    let mut oversized = Encoder::default();
    oversized.property(100, 4);
    oversized.property(101, 203);
    oversized.property(200, 65);
    assert!(matches!(
        decode(oversized.0, 68, &query),
        Err(Error::Resource(_))
    ));
    let mut state = State::new(68, &query)?;
    state.identifiers = 2;
    assert!(matches!(
        read::expression(
            &mut Reader::new(column_reference(&[b"CURRENT_TIMESTAMP"])),
            0,
            &mut state
        ),
        Err(Error::Resource(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_conditional_and_predicate_nodes_use_duckdb_parsed_classes() -> Result<()> {
    let query = QueryContext::background();
    let integer = |value| StoredExpression::literal(DataType::Integer, Value::Integer(value));
    let boolean = |value| StoredExpression::literal(DataType::Boolean, Value::Boolean(value));
    let comparison = StoredExpression {
        alias: None,
        source_span: None,
        kind: StoredExpressionKind::Comparison {
            kind: StoredComparison::Equal,
            left: Box::new(integer(1)),
            right: Box::new(integer(1)),
        },
    };
    let cases = vec![
        (
            StoredExpression {
                alias: None,
                source_span: None,
                kind: StoredExpressionKind::Case {
                    checks: vec![StoredCaseCheck {
                        when_expression: comparison.clone(),
                        then_expression: integer(7),
                    }],
                    otherwise: Box::new(integer(0)),
                },
            },
            (2, 150),
        ),
        (comparison, (5, 25)),
        (
            StoredExpression {
                alias: None,
                source_span: None,
                kind: StoredExpressionKind::Conjunction {
                    kind: StoredConjunction::And,
                    children: vec![boolean(true), boolean(false)],
                },
            },
            (6, 50),
        ),
        (
            StoredExpression {
                alias: None,
                source_span: None,
                kind: StoredExpressionKind::Between {
                    input: Box::new(integer(2)),
                    lower: Box::new(integer(1)),
                    upper: Box::new(integer(3)),
                },
            },
            (19, 38),
        ),
    ];
    for (expression, expected_header) in cases {
        for version in 64..=69 {
            let bytes = encode(&expression, version, &query)?;
            assert_eq!(header(bytes.clone())?, expected_header);
            assert_eq!(decode(bytes, version, &query)?, expression);
        }
    }
    for (kind, expression_kind) in [
        (StoredOperator::Not, 13),
        (StoredOperator::IsNull, 14),
        (StoredOperator::IsNotNull, 15),
        (StoredOperator::In, 35),
        (StoredOperator::NotIn, 36),
    ] {
        let children = if matches!(kind, StoredOperator::In | StoredOperator::NotIn) {
            vec![integer(1), integer(2)]
        } else {
            vec![boolean(true)]
        };
        let expression = StoredExpression {
            alias: None,
            source_span: None,
            kind: StoredExpressionKind::Operator { kind, children },
        };
        for version in 64..=69 {
            let bytes = encode(&expression, version, &query)?;
            assert_eq!(header(bytes.clone())?, (10, expression_kind));
            assert_eq!(decode(bytes, version, &query)?, expression);
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn retained_native_tree_preserves_typed_nulls_failing_casts_aliases_and_argument_provenance()
-> Result<()> {
    let query = QueryContext::background();
    let null = StoredExpression::literal(DataType::UTinyInt, Value::Null);
    let bad_cast = StoredExpression {
        alias: Some("child_alias".into()),
        source_span: None,
        kind: StoredExpressionKind::Cast {
            expression: Box::new(StoredExpression::literal(
                DataType::Varchar,
                Value::Varchar("bad".into()),
            )),
            target: DataType::Integer,
            try_cast: false,
        },
    };
    let mut tree = StoredExpression {
        alias: Some("root_alias".into()),
        source_span: None,
        kind: StoredExpressionKind::Function {
            name: vec!["catalog".into(), "schema".into(), "struct_pack".into()],
            arguments: vec![
                StoredArgument {
                    name: Some("Amount".into()),
                    expression: null,
                },
                StoredArgument {
                    name: Some("Unexecuted".into()),
                    expression: bad_cast,
                },
            ],
            is_operator: false,
            argument_style: StoredArgumentStyle::Named,
        },
    };
    assert_eq!(decode(encode(&tree, 69, &query)?, 69, &query)?, tree);
    let mut output = Encoder(vec![42]);
    assert!(matches!(
        write(&mut output, &tree, 68, &query),
        Err(Error::Unsupported(_))
    ));
    assert_eq!(output.0, [42]);
    let named_legacy_ambiguous = StoredExpression {
        alias: None,
        source_span: None,
        kind: StoredExpressionKind::Function {
            name: vec!["named".into()],
            arguments: vec![StoredArgument {
                name: Some("value".into()),
                expression: StoredExpression {
                    alias: Some("value".into()),
                    source_span: None,
                    kind: StoredExpressionKind::Literal {
                        data_type: DataType::Integer,
                        value: Value::Integer(1),
                    },
                },
            }],
            is_operator: false,
            argument_style: StoredArgumentStyle::Named,
        },
    };
    assert!(matches!(
        encode(&named_legacy_ambiguous, 68, &query),
        Err(Error::Unsupported(_))
    ));
    let StoredExpressionKind::Function {
        arguments,
        argument_style,
        ..
    } = &mut tree.kind
    else {
        unreachable!()
    };
    *argument_style = StoredArgumentStyle::LegacyAliases;
    for arg in arguments {
        arg.expression.alias = arg.name.clone();
    }
    for version in 64..=69 {
        assert_eq!(
            decode(encode(&tree, version, &query)?, version, &query)?,
            tree
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_expression_read_is_bounded_and_writes_are_atomic() -> Result<()> {
    let query = QueryContext::background();
    let leaf = StoredExpression::literal(DataType::Integer, Value::Integer(1));
    let bytes = encode(&leaf, 69, &query)?;
    for end in 0..bytes.len() {
        assert!(read(&mut Reader::new(bytes[..end].to_vec()), 69, &query).is_err());
    }
    let mut state = State::new(69, &query)?;
    state.identifiers = 2;
    let mut oversized = Encoder::default();
    oversized.property(100, 7);
    oversized.property(101, 75);
    oversized.field(102);
    oversized.blob(b"abc");
    assert!(matches!(
        read::expression(&mut Reader::new(oversized.0), 0, &mut state),
        Err(Error::Resource(_))
    ));
    let mut oversized = Encoder::default();
    oversized.property(100, 10);
    oversized.property(101, 156);
    oversized.property(200, 16_384);
    assert!(matches!(
        read(&mut Reader::new(oversized.0), 69, &query),
        Err(Error::Resource(_))
    ));
    let mut tree = leaf;
    for _ in 0..65 {
        tree = StoredExpression {
            alias: None,
            source_span: None,
            kind: StoredExpressionKind::Cast {
                expression: Box::new(tree),
                target: DataType::Integer,
                try_cast: false,
            },
        };
    }
    let mut output = Encoder(vec![1, 2, 3]);
    assert!(matches!(
        write(&mut output, &tree, 69, &query),
        Err(Error::Resource(_))
    ));
    assert_eq!(output.0, [1, 2, 3]);
    let interrupt = crate::parallel::InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 2048, usize::MAX)?;
    interrupt.interrupt();
    assert!(matches!(
        read(&mut Reader::new(vec![]), 69, &cancelled),
        Err(Error::Interrupted)
    ));
    assert!(matches!(
        write(&mut output, &tree, 69, &cancelled),
        Err(Error::Interrupted)
    ));
    assert_eq!(output.0, [1, 2, 3]);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_cpp_parsed_expression_fixtures_remain_unevaluated() -> Result<()> {
    let report: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../test/fixtures/native/nested_parsed_expression_reference.fixture"
    ))
    .unwrap();
    let query = QueryContext::background();
    let mut passed = 0;
    let mut unsupported = Vec::new();
    let mut exported = Vec::new();
    for fixture in report["fixtures"].as_array().unwrap() {
        let Some(hex) = fixture["parsed"]["wire_hex"].as_str() else {
            continue;
        };
        let bytes = hex
            .as_bytes()
            .chunks_exact(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect();
        let version = fixture["version"].as_u64().unwrap();
        let mut record = fixture.clone();
        match decode(bytes, version, &query) {
            Ok(tree) => {
                let output = encode(&tree, version, &query)?;
                let restored = decode(output.clone(), version, &query)?;
                assert_eq!(tree, restored);
                assert_eq!(encode(&restored, version, &query)?, output);
                record["rust_wire_hex"] = output
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
                    .into();
                passed += 1;
            }
            Err(Error::Unsupported(message)) => {
                record["rust_unsupported"] = message.clone().into();
                unsupported.push(format!(
                    "{}/{}/{}: {message}",
                    fixture["producer"], version, fixture["name"]
                ));
            }
            Err(error) => panic!(
                "{}/{}/{}: {error}",
                fixture["producer"], version, fixture["name"]
            ),
        }
        exported.push(record);
    }
    eprintln!("parsed fixtures: {passed} supported, unsupported: {unsupported:?}");
    assert_eq!(passed, 186);
    assert_eq!(unsupported.len(), 14);
    assert!(unsupported.iter().all(|message| {
        message.contains("native retained column reference other than CURRENT_TIMESTAMP")
    }));
    if let Some(path) = std::env::var_os("DUCKDB_NATIVE_PARSED_CODEC_EXPORT") {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .unwrap();
        file.write_all(serde_json::to_string_pretty(&exported).unwrap().as_bytes())
            .unwrap();
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_expression_pending_children_share_root_budget_and_literal_bits_survive() -> Result<()> {
    let query = QueryContext::background();
    let mut state = State::new(69, &query)?;
    state.visit(0)?;
    state.count(MAX_NODES - 1)?;
    state.visit(1)?;
    assert!(
        matches!(state.count(1), Err(Error::Resource(_))),
        "unvisited siblings already own the remaining nodes"
    );
    for bits in [0, 1_u64 << 63, 0x7ff8_0000_0000_0001, 0xfff8_0000_0000_0002] {
        let tree = StoredExpression::literal(DataType::Double, Value::Double(f64::from_bits(bits)));
        let bytes = encode(&tree, 69, &query)?;
        let decoded = decode(bytes.clone(), 69, &query)?;
        let StoredExpressionKind::Literal {
            value: Value::Double(value),
            ..
        } = decoded.kind
        else {
            panic!("wrong literal")
        };
        assert_eq!(value.to_bits(), bits);
        assert_eq!(encode(&tree, 69, &query)?, bytes);
    }
    // QualifiedName::Catalog is the FIRST path component, not the third
    // component from the end for nested schemas.
    let tree = StoredExpression {
        alias: None,
        source_span: None,
        kind: StoredExpressionKind::Function {
            name: vec![
                "catalog".into(),
                "outer".into(),
                "inner".into(),
                "fn".into(),
            ],
            arguments: vec![],
            is_operator: true,
            argument_style: StoredArgumentStyle::Named,
        },
    };
    assert_eq!(decode(encode(&tree, 69, &query)?, 69, &query)?, tree);
    assert!(matches!(
        encode(&tree, 68, &query),
        Err(Error::Unsupported(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn native_expression_rejects_unrepresented_function_state_and_bad_pointers() -> Result<()> {
    let query = QueryContext::background();
    for field in [203, 205, 207] {
        let mut wire = Encoder::default();
        wire.property(100, 9);
        wire.property(101, 140);
        wire.field(200);
        wire.blob(b"fn");
        wire.field(field);
        wire.boolean(true);
        wire.end();
        assert!(matches!(
            read(&mut Reader::new(wire.0), 69, &query),
            Err(Error::Unsupported(_))
        ));
    }
    let mut wire = Encoder::default();
    wire.property(100, 3);
    wire.property(101, 12);
    wire.field(200);
    wire.boolean(false);
    wire.end();
    assert!(matches!(
        read(&mut Reader::new(wire.0), 69, &query),
        Err(Error::Corrupt(_))
    ));
    for kind in [13, 14, 15, 35, 36, 153, 155] {
        let mut wire = Encoder::default();
        wire.property(100, 10);
        wire.property(101, kind);
        wire.end();
        assert!(matches!(
            read(&mut Reader::new(wire.0), 69, &query),
            Err(Error::Corrupt(_))
        ));
    }
    let child = StoredExpression::literal(DataType::Boolean, Value::Boolean(true));
    for (kind, count) in [(13, 2), (14, 2), (15, 2), (35, 1), (36, 1)] {
        let mut wire = Encoder::default();
        wire.property(100, 10);
        wire.property(101, kind);
        wire.property(200, count);
        for _ in 0..count {
            wire.boolean(true);
            wire.0.extend(encode(&child, 69, &query)?);
        }
        wire.end();
        assert!(matches!(
            read(&mut Reader::new(wire.0), 69, &query),
            Err(Error::Corrupt(_))
        ));
    }
    let mut wire = Encoder::default();
    wire.property(100, 9);
    wire.property(101, 140);
    wire.field(200);
    wire.blob(b"fn");
    wire.field(210);
    wire.property(100, 1);
    wire.blob(b"different");
    wire.end();
    wire.end();
    assert!(matches!(
        read(&mut Reader::new(wire.0), 69, &query),
        Err(Error::Unsupported(_))
    ));
    Ok(())
}
