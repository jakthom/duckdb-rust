mod binary;
mod catalog;
mod columns;
pub mod compression;
mod nested;
mod primitive;
mod temporal;
// Typed literal codec prerequisite; the parsed-expression owner wires callers.
#[cfg(test)]
mod value;
#[cfg(test)]
mod version_tests;
mod visibility;
pub mod wal;
mod write_support;
mod writer;

use std::collections::HashSet;

use super::{format::SnapshotFormat, table::Snapshot};
use crate::common::{Error, Result};
use binary::{Reader, checksum, corrupt, u64_at};

/// Native DuckDB checkpoint representation. Unsupported metadata is rejected
/// before publication; this format has no dependency on filesystem or SQL APIs.
pub struct DuckDbFormat {
    decoders: super::compression::DecoderRegistry,
    new_file_version: u64,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Default for DuckDbFormat {
    fn default() -> Self {
        Self::new(compression::decoders())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl DuckDbFormat {
    pub fn new(decoders: super::compression::DecoderRegistry) -> Self {
        Self {
            decoders,
            new_file_version: 64,
        }
    }
    /// Select the native storage version for newly created images. Existing
    /// files retain their validated version; this is not an implicit upgrade.
    /// The provisional legacy default remains 64. VARIANT needs 68, and TUPLE
    /// or empty STRUCT needs 69, matching the pinned development type gates.
    pub fn with_storage_version(mut self, version: u64) -> Result<Self> {
        write_support::new_headers(version)?;
        self.new_file_version = version;
        Ok(self)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SnapshotFormat for DuckDbFormat {
    fn format_id(&self) -> super::format::FormatId {
        super::format::DUCKDB_FORMAT
    }
    fn name(&self) -> &'static str {
        "duckdb-checkpoint"
    }
    fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        let mut adapters = vec![("format", self.name())];
        adapters.extend(self.decoders.adapters());
        adapters
    }
    fn decode(
        &self,
        bytes: Vec<u8>,
        types: std::sync::Arc<crate::common::type_registry::TypeRegistry>,
    ) -> Result<Snapshot> {
        self.decode_with_context(
            bytes,
            &crate::parallel::QueryContext::background().with_types(types),
        )
    }
    fn decode_with_context(
        &self,
        bytes: Vec<u8>,
        query: &crate::parallel::QueryContext,
    ) -> Result<Snapshot> {
        query.check()?;
        let blocks = Blocks::new(bytes)?;
        let snapshot = catalog::load(&columns::ReadContext {
            blocks: &blocks,
            decoders: &self.decoders,
            query,
        })?;
        query.check()?;
        Ok(snapshot)
    }
    fn encode(&self, snapshot: &Snapshot) -> Result<Vec<u8>> {
        snapshot.validate()?;
        if self.new_file_version == 64 {
            writer::encode(snapshot)
        } else {
            writer::encode_version(snapshot, self.new_file_version)
        }
    }
    fn checkpoint_encoder(
        &self,
        bytes: &[u8],
    ) -> Result<Option<Box<dyn super::format::CheckpointEncoder>>> {
        Ok(Some(Box::new(NativeCheckpointEncoder(
            CheckpointIdentity::read(bytes)?,
        ))))
    }
    fn checkpoint_value_equivalent(
        &self,
        selected: &crate::common::type_registry::BoundType,
        source: &crate::common::Value,
        decoded: &crate::common::Value,
        context: &crate::parallel::QueryContext,
    ) -> Result<Option<bool>> {
        if selected.data_type() == &crate::common::NestedType::Variant.data_type() {
            nested::variant::exact::equivalent(source, decoded, selected, context).map(Some)
        } else {
            Ok(None)
        }
    }
    fn encode_successor(
        &self,
        snapshot: &Snapshot,
        previous: &[u8],
    ) -> Result<super::layout::CheckpointImage> {
        snapshot.validate()?;
        Ok(super::layout::CheckpointImage {
            bytes: writer::encode_successor(snapshot, CheckpointIdentity::read(previous)?)?,
            layout: super::layout::CheckpointLayout::compacted(snapshot)?,
        })
    }
    fn supports_successor(&self) -> bool {
        true
    }
}

struct NativeCheckpointEncoder(CheckpointIdentity);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl super::format::CheckpointEncoder for NativeCheckpointEncoder {
    fn storage_version(&self) -> Option<super::format::StorageVersion> {
        Some(super::format::StorageVersion {
            format: super::format::DUCKDB_FORMAT,
            version: self.0.storage_version(),
        })
    }
    fn encode(&self, snapshot: &Snapshot) -> Result<Vec<u8>> {
        snapshot.validate()?;
        writer::encode_successor(snapshot, self.0)
    }
}

#[derive(Clone, Copy, Debug)]
struct CheckpointIdentity {
    identifier: [u8; 16],
    iteration: u64,
    root: u64,
    main_version: u64,
    database_version: u64,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CheckpointIdentity {
    fn storage_version(self) -> u64 {
        storage_version(self.database_version)
    }
    fn read(bytes: &[u8]) -> Result<Self> {
        if bytes.get(8..12) != Some(b"DUCK") {
            return Err(corrupt("missing DuckDB magic"));
        }
        verify(
            bytes
                .get(..4096)
                .ok_or_else(|| corrupt("truncated main header"))?,
        )?;
        let header = database_header(bytes)?;
        let (main_version, database_version) = header_versions(bytes, header)?;
        Ok(Self {
            identifier: bytes
                .get(124..140)
                .ok_or_else(|| corrupt("missing database identifier"))?
                .try_into()
                .map_err(|_| corrupt("database identifier"))?,
            iteration: u64_at(header, 8)?,
            root: u64_at(header, 16)?,
            main_version,
            database_version,
        })
    }
}

struct Blocks {
    bytes: Vec<u8>,
    block_size: usize,
    block_count: u64,
    root: u64,
    vector_size: usize,
    storage_version: u64,
}

mod free_tail;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn storage_version(database_version: u64) -> u64 {
    match database_version {
        0..=3 | 64 => 64,
        4..=7 => database_version + 61,
        69 => 69,
        _ => unreachable!("validated checkpoint storage version"),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Blocks {
    fn new(bytes: Vec<u8>) -> Result<Self> {
        if bytes.get(8..12) != Some(b"DUCK") {
            return Err(corrupt("missing DuckDB magic"));
        }
        verify(
            bytes
                .get(..4096)
                .ok_or_else(|| corrupt("truncated main header"))?,
        )?;
        let header = database_header(&bytes)?;
        let (_, database_version) = header_versions(&bytes, header)?;
        let block_size = match u64_at(header, 40)? {
            0 => 262144,
            n => usize::try_from(n).map_err(|_| corrupt("block size overflow"))?,
        };
        if !block_size.is_power_of_two() || !(16384..=262144).contains(&block_size) {
            return Err(Error::Unsupported(format!(
                "DuckDB block allocation size {block_size}"
            )));
        }
        let vector_size = match u64_at(header, 48)? {
            0 => 2048,
            n => usize::try_from(n).map_err(|_| corrupt("vector size overflow"))?,
        };
        if vector_size == 0 || vector_size > 65536 {
            return Err(corrupt("invalid stored vector size"));
        }
        let root = u64_at(header, 16)?;
        let block_count = u64_at(header, 32)?;
        let free_list = u64_at(header, 24)?;
        let blocks = Self {
            bytes,
            block_size,
            block_count,
            root,
            vector_size,
            storage_version: storage_version(database_version),
        };
        blocks.validate_free_tail(free_list)?;
        Ok(blocks)
    }
    fn block(&self, id: u64) -> Result<&[u8]> {
        if id >= self.block_count {
            return Err(corrupt(format!("block {id} outside file")));
        }
        let offset = usize::try_from(id)
            .ok()
            .and_then(|id| id.checked_mul(self.block_size))
            .and_then(|offset| offset.checked_add(12288))
            .ok_or_else(|| corrupt("block offset overflow"))?;
        let end = offset
            .checked_add(self.block_size)
            .ok_or_else(|| corrupt("block end overflow"))?;
        let bytes = self
            .bytes
            .get(offset..end)
            .ok_or_else(|| corrupt("truncated block"))?;
        verify(bytes)?;
        Ok(&bytes[8..])
    }
    fn metadata(&self, pointer: (u64, usize)) -> Result<Reader> {
        let mut output = Vec::new();
        let mut visited = HashSet::new();
        let (mut pointer, mut offset) = pointer;
        let size = ((self.block_size - 8) / 64) & !7;
        loop {
            if !visited.insert(pointer) {
                return Err(corrupt("cyclic metadata chain"));
            }
            let index = (pointer >> 56) as usize;
            if index >= 64 {
                return Err(corrupt("metadata index outside block"));
            }
            let block = self.block(pointer & 0x00ff_ffff_ffff_ffff)?;
            let part = block
                .get(index * size..(index + 1) * size)
                .ok_or_else(|| corrupt("invalid metadata block"))?;
            let next = u64_at(part, 0)?;
            offset = offset.max(8);
            let part = part
                .get(offset..)
                .ok_or_else(|| corrupt("metadata offset outside block"))?;
            if output.len().saturating_add(part.len()) > self.bytes.len() {
                return Err(corrupt("metadata exceeds file size"));
            }
            output.extend(part);
            if next == u64::MAX {
                break;
            }
            pointer = next;
            offset = 8;
        }
        Ok(Reader::new(output))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn header_versions(bytes: &[u8], header: &[u8]) -> Result<(u64, u64)> {
    let main = u64_at(bytes, 12)?;
    if !(64..=69).contains(&main) && main != 999 {
        return Err(Error::Unsupported(format!("DuckDB storage version {main}")));
    }
    for offset in [20, 28, 36, 44] {
        if u64_at(bytes, offset)? != 0 {
            return Err(Error::Unsupported(
                "DuckDB header flags or encryption".into(),
            ));
        }
    }
    let database = u64_at(header, 56)?;
    validate_storage_version(main, database)?;
    Ok((main, database))
}

/// Development v2 uses 999 in the main header and stores storage version 69
/// in the selected database header. Older files store serialization versions
/// there instead (including a historical mistaken storage-version value 64).
/// Keep the two namespaces separate and reject unknown future layouts.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate_storage_version(main: u64, database: u64) -> Result<()> {
    let supported = if main == 999 || database >= 69 {
        database == 69
    } else {
        matches!(database, 0..=7 | 64)
    };
    if !supported {
        return Err(Error::Unsupported(format!(
            "DuckDB database storage/serialization version {database} (main {main})"
        )));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn verify(bytes: &[u8]) -> Result<()> {
    if u64_at(bytes, 0)?
        != checksum(
            bytes
                .get(8..)
                .ok_or_else(|| corrupt("truncated checksum"))?,
        )?
    {
        return Err(corrupt("DuckDB checksum mismatch"));
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl super::compression::BlockSource for Blocks {
    fn block(&self, id: u64) -> Result<&[u8]> {
        Blocks::block(self, id)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn database_header(bytes: &[u8]) -> Result<&[u8]> {
    let first = bytes
        .get(4096..8192)
        .ok_or_else(|| corrupt("missing database header"))?;
    let second = bytes
        .get(8192..12288)
        .ok_or_else(|| corrupt("missing database header"))?;
    let header = match (verify(first), verify(second)) {
        (Ok(()), Ok(())) => {
            if u64_at(first, 8)? > u64_at(second, 8)? {
                first
            } else {
                second
            }
        }
        (Ok(()), Err(_)) => first,
        (Err(_), Ok(())) => second,
        (Err(error), Err(_)) => return Err(error),
    };
    Ok(header)
}
