use super::*;
use std::{fs, io::Read, path::Path};

struct TruncatedVariantData;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::storage::compression::SegmentDecoder for TruncatedVariantData {
    fn id(&self) -> duckdb_rust::storage::compression::CodecId {
        duckdb_rust::storage::compression::CodecId(1)
    }
    fn name(&self) -> &'static str {
        "truncated-variant-test"
    }
    fn supports(&self, kind: duckdb_rust::storage::compression::SegmentType<'_>) -> bool {
        duckdb_rust::storage::duckdb::compression::UncompressedDecoder.supports(kind)
    }
    fn decode(
        &self,
        input: duckdb_rust::storage::compression::DecodeInput<'_>,
        context: &duckdb_rust::storage::compression::DecodeContext<'_>,
    ) -> Result<Vec<Value>> {
        let mut values = duckdb_rust::storage::duckdb::compression::UncompressedDecoder
            .decode(input, context)?;
        if matches!(
            input.kind,
            duckdb_rust::storage::compression::SegmentType::Values(DataType::Blob)
        ) {
            for value in &mut values {
                if let Value::Blob(bytes) = value {
                    bytes.clear();
                }
            }
        }
        Ok(values)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn independent_variant_child_streams_preserve_dynamic_types_and_nested_nulls() -> Result<()> {
    let directory = tempfile::tempdir()?;
    for (target, name, count) in [
        ("development", "nested_variant_unshredded", 23),
        ("development", "nested_variant_shredded", 8),
        ("release", "nested_variant_shredded", 8),
    ] {
        let path = directory.path().join(format!("{target}-{name}.duckdb"));
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("test/data/duckdb/nested-variant-{target}"))
            .join(format!("{name}.duckdb.gz"));
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(fs::File::open(source)?).read_to_end(&mut bytes)?;
        fs::write(&path, &bytes)?;
        let mut c = Database::open_read_only(&path)?.connect();
        assert_eq!(
            c.query("SELECT count(*) FROM t")?.rows,
            vec![vec![Value::Integer(count)]]
        );
        assert_eq!(
            c.query("SELECT count(*) FROM t WHERE v=xs[1]")?.rows,
            vec![vec![Value::Integer(count - 1)]]
        );
        assert_eq!(
            c.query("SELECT count(*) FROM t WHERE xs[2] IS NULL")?.rows,
            vec![vec![Value::Integer(count)]]
        );
        assert_eq!(
            c.query("SELECT s.d,s.ts::VARCHAR FROM t WHERE id=1")?.rows,
            vec![vec![
                Value::Decimal {
                    value: 100,
                    width: 12,
                    scale: 2
                },
                Value::Varchar("2000-01-01 00:00:00.123456789".into())
            ]]
        );
        if name.ends_with("unshredded") {
            assert_eq!(
                c.query("SELECT v::BIGNUM::VARCHAR FROM t WHERE id IN (1,2) ORDER BY id")?
                    .rows,
                vec![
                    vec![Value::Varchar("-0".into())],
                    vec![Value::Varchar(
                        "340282366920938463463374607431768211456".into()
                    )]
                ]
            );
            assert_eq!(
                c.query(
                    "SELECT variant_typeof(v) FROM t WHERE id IN (3,13,16,17,18,19) ORDER BY id"
                )?
                .rows,
                [
                    "UINT64",
                    "TIMESTAMP_NANOS",
                    "INT8",
                    "UINT16",
                    "FLOAT",
                    "DOUBLE"
                ]
                .into_iter()
                .map(|ty| vec![Value::Varchar(ty.into())])
                .collect::<Vec<_>>()
            );
        } else {
            assert_eq!(
                c.query("SELECT variant_typeof(v),v::VARCHAR FROM t WHERE id=0")?
                    .rows,
                vec![vec![
                    Value::Varchar("OBJECT(a, d, items)".into()),
                    Value::Varchar("{'a': 1, 'd': 12.50, 'items': [1, 2]}".into())
                ]]
            );
            assert_eq!(
                c.query(
                    "SELECT variant_exists(v,'a'),v.a::VARCHAR FROM t WHERE id IN (2,4) ORDER BY id"
                )?
                .rows,
                vec![
                    vec![Value::Boolean(true), Value::Null],
                    vec![Value::Boolean(false), Value::Null]
                ]
            );
            assert_eq!(
                c.query("SELECT v.extra::VARCHAR,v.a::VARCHAR FROM t WHERE id=3")?
                    .rows,
                vec![vec![
                    Value::Varchar("leftover".into()),
                    Value::Varchar("str".into())
                ]]
            );
        }
        let expected = c.query("SELECT * FROM t ORDER BY id")?.rows;
        let parameter = c.prepare("SELECT v FROM t WHERE id=$1")?;
        assert_eq!(
            c.execute_prepared(&parameter, &[Value::Integer(1)])?.rows,
            vec![vec![expected[1][1].clone()]]
        );
        assert_eq!(
            c.query("SELECT count(*) FROM t a JOIN (SELECT xs[1] k FROM t) b ON a.v=b.k")?
                .rows,
            vec![vec![Value::Integer(count - 1)]]
        );
        assert_eq!(
            c.query("SELECT count(*) OVER (PARTITION BY v) FROM t ORDER BY id")?
                .rows,
            vec![vec![Value::Integer(1)]; count as usize]
        );
        drop(c);
        let mut c = Database::open(&path)?.connect();
        assert!(matches!(
            c.execute("UPDATE t SET id=id+100"),
            Err(duckdb_rust::Error::Unsupported(_))
        ));
        assert_eq!(c.query("SELECT * FROM t ORDER BY id")?.rows, expected);
        drop(c);
        assert_eq!(fs::read(&path)?, bytes);
        assert_eq!(
            Database::open_read_only(&path)?
                .connect()
                .query("SELECT * FROM t ORDER BY id")?
                .rows,
            expected
        );
        // The selected codec can produce a correctly typed BLOB while its
        // internal VARIANT offsets/lengths are invalid. Reject that logical
        // corruption before publishing any catalog or table state.
        let mut decoders = duckdb_rust::storage::duckdb::compression::decoders();
        decoders.replace(Arc::new(TruncatedVariantData))?;
        let malformed = DatabaseBuilder::new()
            .durability(Arc::new(FileCheckpoint::open(
                &path,
                OpenMode::ReadOnly,
                Arc::new(duckdb_rust::storage::duckdb::DuckDbFormat::new(decoders)),
            )?))
            .build();
        assert!(matches!(malformed, Err(duckdb_rust::Error::Corrupt(_))));
        assert_eq!(fs::read(&path)?, bytes);
    }
    Ok(())
}
