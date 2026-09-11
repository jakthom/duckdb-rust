use super::{layout::CheckpointImage, table::Snapshot};
use crate::common::{Error, Result};

/// Serialized checkpoint family, used to validate recovery/codec composition.
/// Wire versions and feature support are checked by the selected decoders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatId(pub &'static str);

pub const DUCKDB_FORMAT: FormatId = FormatId("duckdb");
pub const JSON_FORMAT: FormatId = FormatId("duckdb-rust-json");

/// Compatibility of a validated existing checkpoint, not a fresh-file preference
/// or publication identity. The selected format defines this version namespace;
/// a composed log must validate it before enabling version-dependent encodings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StorageVersion {
    pub format: FormatId,
    pub version: u64,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// A selected format's owned publication state, bound from a validated image.
/// It retains only the metadata needed to encode successors, not the previous
/// table data. Encoding has no effects and leaves this state unchanged. The
/// caller binds the returned image's successor state before publishing bytes.
pub trait CheckpointEncoder: Send + Sync {
    fn encode(&self, snapshot: &Snapshot) -> Result<Vec<u8>>;
    /// Compact metadata already bound from validated bytes. None grants no
    /// version-dependent log capability. Never retain/reread the old table image.
    fn storage_version(&self) -> Option<StorageVersion> {
        None
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// A complete checkpoint representation, independent of I/O and transaction
/// publication. Decoding owns its input and returns an independently owned
/// catalog/data snapshot. Unsupported types or metadata must fail explicitly;
/// implementations must never discard information when encoding or decoding.
/// Decoding preserves the input's physical row identities and append high-water
/// marks for recovery. Encoding may compact/reassign IDs in a new checkpoint;
/// publication must retire any log addressed to the previous checkpoint.
pub trait SnapshotFormat: Send + Sync {
    fn name(&self) -> &'static str;
    fn format_id(&self) -> FormatId;
    fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        vec![("format", self.name())]
    }
    fn decode(
        &self,
        bytes: Vec<u8>,
        types: std::sync::Arc<crate::common::type_registry::TypeRegistry>,
    ) -> Result<Snapshot>;
    /// Contextual decoding keeps caller-selected services available to formats
    /// that retain catalog expressions. The legacy default delegates without
    /// constructing expression services; it checks cancellation at its boundary.
    fn decode_with_context(
        &self,
        bytes: Vec<u8>,
        context: &crate::parallel::QueryContext,
    ) -> Result<Snapshot> {
        context.check()?;
        let snapshot = self.decode(bytes, context.type_registry())?;
        context.check()?;
        Ok(snapshot)
    }
    fn encode(&self, snapshot: &Snapshot) -> Result<Vec<u8>>;
    /// Compare a declared value after this format's documented canonicalization.
    /// None retains exact physical comparison, including floating-point bits.
    /// This hook cannot waive catalog or row-identity checks. The caller retains
    /// the source snapshot's selected type and bounds the complete row traversal;
    /// implementations must preserve selected validation, cancellation and their
    /// own codec limits. SQL equality, casts and grouping keys are not substitutes
    /// for exact stored content. This is pure and grants no publication support.
    fn checkpoint_value_equivalent(
        &self,
        _selected: &crate::common::type_registry::BoundType,
        _source: &crate::common::Value,
        _decoded: &crate::common::Value,
        _context: &crate::parallel::QueryContext,
    ) -> Result<Option<bool>> {
        Ok(None)
    }
    /// Retain format-owned identity/version state for ordinary checkpoint
    /// publication. None keeps this format's stateless encode implementation.
    /// A binding may not borrow the bytes or retain the whole prior image.
    /// Unsupported metadata must fail before publication, not be discarded.
    fn checkpoint_encoder(&self, _bytes: &[u8]) -> Result<Option<Box<dyn CheckpointEncoder>>> {
        Ok(None)
    }
    fn supports_successor(&self) -> bool {
        false
    }
    /// Encode a successor to a validated checkpoint. The format preserves its
    /// database identity, advances its generation and chooses a different root
    /// where its recovery protocol uses roots to identify publication. The
    /// complete row mapping must match decoding of the returned bytes. No I/O
    /// occurs. Formats without this protocol reject it before publication.
    fn encode_successor(&self, _snapshot: &Snapshot, _previous: &[u8]) -> Result<CheckpointImage> {
        Err(Error::Unsupported("successor checkpoint encoding".into()))
    }
}

/// Private v0 representation for every type and constraint in Snapshot.
/// Floating-point values use their IEEE bits, including NaN and infinities.
pub struct JsonSnapshotFormat;

const MAGIC: &[u8] = b"DDBRUST\n";

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SnapshotFormat for JsonSnapshotFormat {
    fn format_id(&self) -> FormatId {
        JSON_FORMAT
    }
    fn name(&self) -> &'static str {
        "rust-json-snapshot"
    }
    fn decode(
        &self,
        bytes: Vec<u8>,
        types: std::sync::Arc<crate::common::type_registry::TypeRegistry>,
    ) -> Result<Snapshot> {
        let payload = bytes
            .strip_prefix(MAGIC)
            .ok_or_else(|| Error::Corrupt("missing Rust snapshot signature".into()))?;
        let checksum = payload
            .get(..4)
            .ok_or_else(|| Error::Corrupt("missing snapshot checksum".into()))?;
        let payload = &payload[4..];
        if checksum != crc32fast::hash(payload).to_le_bytes() {
            return Err(Error::Corrupt("Rust snapshot checksum mismatch".into()));
        }
        Snapshot::decode_json(payload, types).map_err(|error| match error {
            Error::Unsupported(_) | Error::Resource(_) => error,
            other => Error::Corrupt(other.to_string()),
        })
    }
    fn encode(&self, snapshot: &Snapshot) -> Result<Vec<u8>> {
        snapshot.validate()?;
        let mut bytes = MAGIC.to_vec();
        let payload = serde_json::to_vec(snapshot).map_err(|e| Error::Execution(e.to_string()))?;
        bytes.extend(crc32fast::hash(&payload).to_le_bytes());
        bytes.extend(payload);
        Ok(bytes)
    }
}
