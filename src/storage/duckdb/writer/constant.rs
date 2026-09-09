use super::{DataType, Encoder, Error, Result, Value, type_id};

pub(super) fn write(output: &mut Encoder, value: &Value, data_type: &DataType) -> Result<()> {
    output.property(100, 7); // ParsedExpressionClass::CONSTANT
    output.property(101, 75); // ExpressionType::VALUE_CONSTANT
    output.field(200);
    output.field(100);
    output.property(100, type_id(data_type)?);
    output.end();
    output.field(101);
    output.boolean(value.is_null());
    if !value.is_null() {
        output.field(102);
        match value.cast(data_type)? {
            Value::Extension(_) => {
                return Err(Error::Unsupported(
                    "native extension constant encoding".into(),
                ));
            }
            Value::Boolean(v) => output.boolean(v),
            Value::Date(v) => output.signed(i64::from(v.days())),
            Value::Integer(v) if *data_type == DataType::HugeInt => {
                output.signed((v >> 64) as i64);
                output.unsigned(v as u64);
            }
            Value::Integer(v) => output.signed(
                i64::try_from(v)
                    .map_err(|_| Error::Internal("serialized default integer width".into()))?,
            ),
            Value::Float(v) => output.0.extend(v.to_le_bytes()),
            Value::Double(v) => output.0.extend(v.to_le_bytes()),
            Value::Varchar(v) => output.string(&v)?,
            Value::Null => return Err(Error::Internal("unexpected NULL default cast".into())),
        }
    }
    output.end();
    output.end();
    Ok(())
}
