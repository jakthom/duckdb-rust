//! Test-only framing around production vector encoding. It deliberately does
//! not call or enable the public WAL session's closed VARIANT capability.
use super::*;
use crate::{
    catalog::{Catalog, CatalogMut, TableName},
    storage::{
        TableStorage,
        duckdb::{
            DuckDbFormat, primitive,
            wal::{DuckDbWalRecovery, append_frame},
        },
        format::SnapshotFormat,
        recovery::{Recovery, RecoveryInput},
        table::Snapshot,
    },
};
use std::{fs, io::Read, path::Path};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn decompress(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(path)?).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn insert_log(types: &[DataType], rows: &[Vec<Value>], query: &QueryContext) -> Result<Vec<u8>> {
    let mut bytes = vec![100, 0, 98, 101, 0, 2, 255, 255];
    let mut named = Encoder::default();
    named.property(100, 25);
    named.field(101);
    named.string("main")?;
    named.field(102);
    named.string("t")?;
    named.end();
    append_frame(&named.0, &mut bytes)?;
    for rows in rows.chunks(2048) {
        let mut insert = Encoder::default();
        insert.property(100, 26);
        insert.field(101);
        insert.property(100, rows.len() as u64);
        insert.property(101, types.len() as u64);
        for ty in types {
            primitive::write_type(&mut insert, ty)?;
        }
        insert.property(102, types.len() as u64);
        let mut remaining = MAX_CELLS;
        for (column, ty) in types.iter().enumerate() {
            crate::storage::duckdb::wal::writer::vector(
                &mut insert,
                ty,
                rows.iter().map(|row| &row[column]),
                0,
                &mut remaining,
                query,
            )?;
            insert.end();
        }
        insert.end();
        insert.end();
        append_frame(&insert.0, &mut bytes)?;
    }
    let mut flush = Encoder::default();
    flush.property(100, 100);
    flush.end();
    append_frame(&flush.0, &mut bytes)?;
    Ok(bytes)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn manual_variant_wal_frames_roundtrip_without_opening_session_capabilities() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let export = std::env::var_os("DUCKDB_VARIANT_WAL_CODEC_EXPORT").map(std::path::PathBuf::from);
    let directory = export.as_deref().unwrap_or(temporary.path());
    if !directory.is_dir() {
        return Err(Error::Internal(
            "manual WAL export requires an existing isolated directory".into(),
        ));
    }
    let query = QueryContext::background();
    for (target, name, version) in [
        ("release", "variant_v1_5_0", 68),
        ("development", "variant_v1_5_0", 68),
        ("development", "variant_v2_0_0", 69),
    ] {
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("test/data/wal-variant-{target}"));
        let recovered = DuckDbWalRecovery.recover(
            RecoveryInput {
                checkpoint: decompress(&fixture.join(format!("{name}.duckdb.gz")))?,
                log: decompress(&fixture.join(format!("{name}.wal.gz")))?,
            },
            &DuckDbFormat::default(),
            &query,
        )?;
        let name_in_catalog = TableName::main("t");
        let definition = recovered.table(&name_in_catalog)?;
        let types = definition
            .columns
            .iter()
            .map(|column| column.data_type.clone())
            .collect::<Vec<_>>();
        let mut empty = Snapshot::new(recovered.type_registry());
        empty.create_table(definition, false)?;
        let format = DuckDbFormat::default().with_storage_version(version)?;
        let checkpoint = format.encode(&empty)?;
        let rows = recovered
            .scan(&name_in_catalog, &query)?
            .into_iter()
            .map(|(_, row)| row)
            .collect::<Vec<_>>();
        let log = insert_log(
            &types,
            &rows,
            &query.clone().with_types(recovered.type_registry()),
        )?;
        let decoded = DuckDbWalRecovery.recover(
            RecoveryInput {
                checkpoint: checkpoint.clone(),
                log: log.clone(),
            },
            &format,
            &query,
        )?;
        let actual = decoded
            .scan(&name_in_catalog, &query)?
            .into_iter()
            .map(|(_, row)| row)
            .collect::<Vec<_>>();
        // Both sides are native canonical rows. Serde carries raw float bits,
        // unlike Value's SQL-facing floating PartialEq (NaNs and signed zero).
        assert_eq!(
            serde_json::to_vec(&actual).unwrap(),
            serde_json::to_vec(&rows).unwrap(),
            "{target} {name}"
        );
        for (suffix, bytes) in [("duckdb", checkpoint), ("duckdb.wal", log)] {
            let path = directory.join(format!("{target}-{name}.{suffix}"));
            if path.exists() {
                return Err(Error::Internal(
                    "manual WAL export refuses to overwrite evidence".into(),
                ));
            }
            fs::write(path, bytes)?;
        }
    }
    Ok(())
}
