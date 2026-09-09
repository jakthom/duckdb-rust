use crate::{
    common::{DataType, Error, Result, Value},
    storage::compression::{CodecId, DecodeContext, DecodeInput, SegmentDecoder, SegmentType},
};
pub struct ConstantDecoder;
impl SegmentDecoder for ConstantDecoder {
    fn id(&self) -> CodecId {
        CodecId(2)
    }
    fn name(&self) -> &'static str {
        "duckdb-constant"
    }
    fn supports(&self, kind: SegmentType<'_>) -> bool {
        kind != SegmentType::Values(&DataType::Null)
    }
    fn decode(&self, input: DecodeInput<'_>, context: &DecodeContext<'_>) -> Result<Vec<Value>> {
        context.query.check_rows(input.count)?;
        let value = match input.kind {
            SegmentType::Validity => Value::Boolean(input.statistics.has_values),
            _ if !input.statistics.has_values => Value::Null,
            SegmentType::Values(DataType::Varchar) => {
                return Err(Error::Unsupported("constant VARCHAR storage".into()));
            }
            _ => input.statistics.minimum.clone(),
        };
        Ok(vec![value; input.count])
    }
}
