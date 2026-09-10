mod alp;
mod alprd;
mod bitpacking;
mod chimp;
mod constant;
mod floating;
mod layout;
mod packed;
mod patas;
use super::primitive;
mod rle;
mod strings;
mod uncompressed;

pub use alp::AlpDecoder;
pub use alprd::AlpRdDecoder;
pub use bitpacking::{BitPackingDecoder, ScalarBitPackingDecoder};
pub use chimp::ChimpDecoder;
pub use constant::ConstantDecoder;
pub use patas::PatasDecoder;
pub use rle::RleDecoder;
pub use strings::{DictionaryDecoder, FsstDecoder};
pub use uncompressed::UncompressedDecoder;

use crate::storage::compression::{DecoderRegistry, SegmentDecoder};
use std::sync::Arc;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Native readers selected by the DuckDB format's composition root. Encoders
/// remain a separate checkpoint concern; replacing a reader never changes bytes
/// written by the current uncompressed writer.
pub fn decoders() -> DecoderRegistry {
    let mut registry = DecoderRegistry::default();
    for decoder in [
        Arc::new(UncompressedDecoder) as Arc<dyn SegmentDecoder>,
        Arc::new(ConstantDecoder),
        Arc::new(RleDecoder),
        Arc::new(DictionaryDecoder),
        Arc::new(BitPackingDecoder),
        Arc::new(FsstDecoder),
        Arc::new(ChimpDecoder),
        Arc::new(AlpDecoder),
        Arc::new(AlpRdDecoder),
        Arc::new(PatasDecoder),
    ] {
        registry
            .register(decoder)
            .expect("distinct native codec IDs");
    }
    registry
}
