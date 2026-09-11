//! Read-only segment decoding, independent of catalog metadata and file I/O.
//!
//! A registry belongs to a file format: codec IDs describe that format's wire
//! representation, not a universal numbering scheme. Registering a decoder does
//! not provide encoding, analysis, reclamation, or transactional access.
use std::{collections::BTreeMap, sync::Arc};

use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CodecId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentType<'a> {
    Validity,
    Values(&'a DataType),
}

#[derive(Debug, Clone)]
pub struct SegmentStatistics {
    pub has_values: bool,
    pub minimum: Value,
}

/// Bytes and statistics remain borrowed for the call. The containing format
/// validates checksums and segment placement; the decoder validates its payload.
/// Values may contain NULL placeholders; separate validity is applied afterward.
#[derive(Clone, Copy)]
pub struct DecodeInput<'a> {
    pub kind: SegmentType<'a>,
    pub count: usize,
    pub data: &'a [u8],
    pub statistics: &'a SegmentStatistics,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Immutable, checksum-validated block payloads. IDs and payload layout belong
/// to the containing file format. Returned storage lives at least as long as the
/// borrow of this source; missing or corrupt blocks return an error.
pub trait BlockSource: Send + Sync {
    fn block(&self, id: u64) -> Result<&[u8]>;
}

pub struct DecodeContext<'a> {
    pub blocks: &'a dyn BlockSource,
    pub query: &'a QueryContext,
    /// Vector size persisted by the file, independent of execution batch size.
    pub vector_size: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Reentrant, side-effect-free full-segment decoding. Output is independently
/// owned and contains exactly `count` physically typed values, in stored order.
/// Validity output is Boolean: false sets NULL, true requires a decoded value.
/// An adapter opting into `preserves_decoded_validity` may also return NULL as
/// an explicit instruction to preserve the base decoder's validity. That marker
/// is a protocol value, not an invalid row; ordinary adapters cannot emit it.
/// Implementations reject
/// malformed input, cooperate with cancellation, and check the row limit before
/// allocating output. No partial scan, fetch, or encoding capability is implied.
pub trait SegmentDecoder: Send + Sync {
    fn id(&self) -> CodecId;
    fn name(&self) -> &'static str;
    fn supports(&self, kind: SegmentType<'_>) -> bool;
    fn preserves_decoded_validity(&self) -> bool {
        false
    }
    fn decode(&self, input: DecodeInput<'_>, context: &DecodeContext<'_>) -> Result<Vec<Value>>;
}

/// Configure before sharing. Duplicate registration is an error; replacement
/// must be explicit and retains the same wire ID. No concrete adapter downcasts.
#[derive(Clone, Default)]
pub struct DecoderRegistry {
    decoders: BTreeMap<CodecId, Arc<dyn SegmentDecoder>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl DecoderRegistry {
    pub fn register(&mut self, decoder: Arc<dyn SegmentDecoder>) -> Result<()> {
        if self.decoders.contains_key(&decoder.id()) {
            return Err(Error::Bind(format!(
                "compression codec {} is already registered",
                decoder.id().0
            )));
        }
        self.decoders.insert(decoder.id(), decoder);
        Ok(())
    }

    pub fn replace(&mut self, decoder: Arc<dyn SegmentDecoder>) -> Result<()> {
        let slot = self.decoders.get_mut(&decoder.id()).ok_or_else(|| {
            Error::Bind(format!(
                "compression codec {} is not registered",
                decoder.id().0
            ))
        })?;
        *slot = decoder;
        Ok(())
    }

    pub fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        self.decoders
            .values()
            .map(|decoder| ("segment-decoder", decoder.name()))
            .collect()
    }

    /// Checked dispatch is the contract boundary for built-in and external
    /// decoders. Failure returns no partially decoded output. Resource limits
    /// currently count rows; they are not a global memory or I/O byte budget.
    pub fn decode(
        &self,
        id: CodecId,
        input: DecodeInput<'_>,
        context: &DecodeContext<'_>,
    ) -> Result<Vec<Value>> {
        context.query.check_rows(input.count)?;
        if input.count > isize::MAX as usize / std::mem::size_of::<Value>() {
            return Err(Error::Resource(
                "decoded segment allocation exceeds address space".into(),
            ));
        }
        if context.vector_size == 0 || context.vector_size > 65536 {
            return Err(Error::Corrupt("invalid stored vector size".into()));
        }
        let decoder = self
            .decoders
            .get(&id)
            .filter(|decoder| decoder.supports(input.kind))
            .ok_or_else(|| {
                Error::Unsupported(format!("compression codec {} for {:?}", id.0, input.kind))
            })?;
        let values = decoder.decode(input, context)?;
        context.query.check()?;
        if values.len() != input.count {
            return Err(Error::Internal(format!(
                "{} returned the wrong segment length",
                decoder.name()
            )));
        }
        for (i, value) in values.iter().enumerate() {
            if i % 1024 == 0 {
                context.query.check()?;
            }
            let valid = match input.kind {
                SegmentType::Validity => {
                    matches!(value, Value::Boolean(_))
                        || (value.is_null() && decoder.preserves_decoded_validity())
                }
                SegmentType::Values(data_type) => value.fits_type(data_type),
            };
            if !valid {
                return Err(Error::Internal(format!(
                    "{} returned the wrong physical type for {:?} at row {i}",
                    decoder.name(),
                    input.kind
                )));
            }
        }
        Ok(values)
    }
}
