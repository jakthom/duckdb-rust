//! EMPTY_VALIDITY deliberately preserves validity already decoded with values;
//! it must not turn dictionary NULLs into all-valid rows.
use crate::{
    common::{Result, Value},
    storage::compression::{CodecId, DecodeContext, DecodeInput, SegmentDecoder, SegmentType},
};

pub struct EmptyValidityDecoder;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SegmentDecoder for EmptyValidityDecoder {
    fn id(&self) -> CodecId {
        CodecId(14)
    }
    fn name(&self) -> &'static str {
        "duckdb-empty-validity"
    }
    fn supports(&self, kind: SegmentType<'_>) -> bool {
        kind == SegmentType::Validity
    }
    fn preserves_decoded_validity(&self) -> bool {
        true
    }
    fn decode(&self, input: DecodeInput<'_>, context: &DecodeContext<'_>) -> Result<Vec<Value>> {
        context.query.check_rows(input.count)?;
        Ok(vec![Value::Null; input.count])
    }
}
