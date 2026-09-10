use super::{DataType, Encoder, Error, Result, Value};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::storage::duckdb) fn write(
    output: &mut Encoder,
    value: &Value,
    data_type: &DataType,
) -> Result<()> {
    output.property(100, 7); // ParsedExpressionClass::CONSTANT
    output.property(101, 75); // ExpressionType::VALUE_CONSTANT
    output.field(200);
    output.field(100);
    super::super::primitive::write_type(output, data_type)?;
    output.field(101);
    output.boolean(value.is_null());
    if !value.is_null() {
        output.field(102);
        match value.cast(data_type)? {
            Value::Nested(_) | Value::Extension(_) => {
                return Err(Error::Unsupported(
                    "native extension constant encoding".into(),
                ));
            }
            Value::Boolean(v) => output.boolean(v),
            Value::Enum(value) => output.unsigned(u64::from(value.ordinal)),
            Value::Date(v) => output.signed(i64::from(v.days())),
            value @ Value::Blob(_) => output.string(&value.to_string())?,
            Value::Uuid(v) => {
                output.signed(((v ^ (1_u128 << 127)) >> 64) as i64);
                output.unsigned(v as u64);
            }
            Value::Temporal(value) => super::super::temporal::write_metadata(output, value)?,
            Value::Integer(v) if *data_type == DataType::HugeInt => {
                output.signed((v >> 64) as i64);
                output.unsigned(v as u64);
            }
            Value::Integer(v) => output.signed(
                i64::try_from(v)
                    .map_err(|_| Error::Internal("serialized default integer width".into()))?,
            ),
            Value::Float(v) => output.0.extend(v.to_le_bytes()),
            v @ (Value::Unsigned(_) | Value::Decimal { .. }) => {
                super::super::primitive::write_numeric(output, &v, data_type)?
            }
            Value::Double(v) => output.0.extend(v.to_le_bytes()),
            Value::Varchar(v) => output.string(&v)?,
            Value::Null => return Err(Error::Internal("unexpected NULL default cast".into())),
        }
    }
    output.end();
    output.end();
    Ok(())
}
