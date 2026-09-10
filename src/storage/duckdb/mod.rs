mod binary;
mod catalog;
mod columns;
pub mod compression;
mod primitive;
mod visibility;
pub mod wal;
mod writer;

use std::collections::HashSet;

use super::{format::SnapshotFormat, table::Snapshot};
use crate::common::{Error, Result};
use binary::{Reader, checksum, corrupt, u64_at};

/// Native DuckDB checkpoint representation. Unsupported metadata is rejected
/// before publication; this format has no dependency on filesystem or SQL APIs.
pub struct DuckDbFormat {
    decoders: super::compression::DecoderRegistry,
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
        Self { decoders }
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
        catalog::load(&Blocks::new(bytes)?, &self.decoders, types)
    }
    fn encode(&self, snapshot: &Snapshot) -> Result<Vec<u8>> {
        snapshot.validate()?;
        writer::encode(snapshot)
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

#[derive(Clone, Copy, Debug)]
struct CheckpointIdentity {
    identifier: [u8; 16],
    iteration: u64,
    root: u64,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CheckpointIdentity {
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
        Ok(Self {
            identifier: bytes
                .get(124..140)
                .ok_or_else(|| corrupt("missing database identifier"))?
                .try_into()
                .map_err(|_| corrupt("database identifier"))?,
            iteration: u64_at(header, 8)?,
            root: u64_at(header, 16)?,
        })
    }
}

struct Blocks {
    bytes: Vec<u8>,
    block_size: usize,
    block_count: u64,
    root: u64,
    vector_size: usize,
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
        let version = u64_at(&bytes, 12)?;
        if !(64..=67).contains(&version) {
            return Err(Error::Unsupported(format!(
                "DuckDB storage version {version}"
            )));
        }
        for offset in [20, 28, 36, 44] {
            if u64_at(&bytes, offset)? != 0 {
                return Err(Error::Unsupported(
                    "DuckDB header flags or encryption".into(),
                ));
            }
        }
        let header = database_header(&bytes)?;
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
        if block_count > ((bytes.len() - 12288) / block_size) as u64 {
            return Err(corrupt("truncated block storage"));
        }
        Ok(Self {
            bytes,
            block_size,
            block_count,
            root,
            vector_size,
        })
    }
    fn block(&self, id: u64) -> Result<&[u8]> {
        if id >= self.block_count {
            return Err(corrupt(format!("block {id} outside file")));
        }
        let offset = 12288
            + usize::try_from(id).map_err(|_| corrupt("block ID overflow"))? * self.block_size;
        let bytes = self
            .bytes
            .get(offset..offset + self.block_size)
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
