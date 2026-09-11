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
#[test]
fn retained_native_tree_preserves_typed_nulls_failing_casts_aliases_and_argument_provenance()
-> Result<()> {
    let query = QueryContext::background();
    let null = StoredExpression::literal(DataType::UTinyInt, Value::Null);
    let bad_cast = StoredExpression {
        alias: Some("child_alias".into()),
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
    assert_eq!(passed, 129);
    assert_eq!(unsupported.len(), 71);
    assert!(
        unsupported
            .iter()
            .all(|message| message.contains("native literal logical type 4")
                || message.contains("native retained expression class 4, kind 203"))
    );
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
