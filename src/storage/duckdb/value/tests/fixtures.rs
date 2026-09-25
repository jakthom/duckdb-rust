use super::*;
use std::io::Write;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_cpp_typed_value_metadata_fixtures_and_rust_exports() -> Result<()> {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../test/data/native-value-metadata.json"
    ))
    .unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    let query = QueryContext::background();
    let mut exported = Vec::new();
    for case in cases {
        let input = hex_decode(case["wire_hex"].as_str().unwrap());
        let version = case["version"].as_u64().unwrap();
        let (ty, value) = decode(&input, version, &query).unwrap_or_else(|error| {
            panic!(
                "{} {} {}: {error}",
                case["producer"], case["version"], case["name"]
            )
        });
        let output = encode(&ty, &value, version, &query)?;
        let (second_type, second_value) = decode(&output, version, &query)?;
        assert_eq!(ty, second_type);
        assert_eq!(encode(&ty, &second_value, version, &query)?, output);
        let mut result = case.clone();
        result["rust_wire_hex"] = hex_encode(&output).into();
        exported.push(result);
    }
    assert_eq!(cases.len(), 81);
    if let Some(path) = std::env::var_os("DUCKDB_NATIVE_VALUE_CODEC_EXPORT") {
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
fn hex_decode(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
