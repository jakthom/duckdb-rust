//! Native temporal payloads, independent of header/catalog version negotiation.
use super::binary::{Encoder, Reader, corrupt, u32_at, u64_at};
use crate::common::{DataType, Result, TemporalValue, Value};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn scalar(data: &[u8], offset: usize, data_type: &DataType) -> Result<Value> {
    let value = match data_type {
        DataType::Interval => TemporalValue::Interval {
            months: u32_at(data, offset)? as i32,
            days: u32_at(
                data,
                offset
                    .checked_add(4)
                    .ok_or_else(|| corrupt("interval offset"))?,
            )? as i32,
            micros: u64_at(
                data,
                offset
                    .checked_add(8)
                    .ok_or_else(|| corrupt("interval offset"))?,
            )? as i64,
        },
        DataType::TimeTz => TemporalValue::from_packed_time_tz(u64_at(data, offset)?)?,
        _ => {
            let ticks = u64_at(data, offset)? as i64;
            // Timestamp MIN is a finite value. Independent column/vector
            // validity masks, applied by the caller, identify its NULL rows.
            if ticks == i64::MIN && data_type.timestamp_precision().is_none() {
                return Ok(Value::Null);
            }
            TemporalValue::from_ticks(data_type, ticks)?
        }
    };
    Ok(Value::Temporal(value))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write_metadata(output: &mut Encoder, value: TemporalValue) -> Result<()> {
    value.validate()?;
    match value {
        TemporalValue::TimeTz { .. } => output.unsigned(value.packed_time_tz()?),
        TemporalValue::Interval {
            months,
            days,
            micros,
        } => {
            for (field, number) in [(1, i64::from(months)), (2, i64::from(days)), (3, micros)] {
                if number != 0 {
                    output.field(field);
                    output.signed(number);
                }
            }
            output.end();
        }
        _ => output.signed(value.ticks()?),
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_metadata(reader: &mut Reader, data_type: &DataType) -> Result<Value> {
    let value = match data_type {
        DataType::TimeTz => TemporalValue::from_packed_time_tz(reader.unsigned()?)?,
        DataType::Interval => {
            let months = if reader.optional(1)? {
                reader.signed()?
            } else {
                0
            };
            let days = if reader.optional(2)? {
                reader.signed()?
            } else {
                0
            };
            let micros = if reader.optional(3)? {
                reader.signed()?
            } else {
                0
            };
            reader.end()?;
            TemporalValue::Interval {
                months: i32::try_from(months).map_err(|_| corrupt("interval months range"))?,
                days: i32::try_from(days).map_err(|_| corrupt("interval days range"))?,
                micros,
            }
        }
        _ => TemporalValue::from_ticks(data_type, reader.signed()?)?,
    };
    Ok(Value::Temporal(value))
}
