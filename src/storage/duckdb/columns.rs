use super::{
    Blocks,
    binary::{Reader, corrupt},
    catalog::block_pointer,
};
mod row_identity;
mod string_statistics;
use crate::{
    catalog::TableDefinition,
    common::{DataType, Error, Result, Row, Value},
};

/// One selected request context follows every descendant column and decoder.
/// Wire ownership stays with Blocks; no ambient execution services are created.
pub(super) struct ReadContext<'a> {
    pub blocks: &'a Blocks,
    pub decoders: &'a DecoderRegistry,
    pub query: &'a QueryContext,
}

use crate::{
    parallel::QueryContext,
    storage::compression::{
        CodecId, DecodeContext, DecodeInput, DecoderRegistry, SegmentStatistics as Statistics,
        SegmentType,
    },
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_table(
    context: &ReadContext<'_>,
    pointer: (u64, usize),
    table: &TableDefinition,
    total: usize,
    next_row_id: u64,
) -> Result<Vec<(crate::storage::RowId, Row)>> {
    let blocks = context.blocks;
    context.query.check()?;
    let mut identities =
        row_identity::RowIdentity::new(blocks.storage_version, total, next_row_id)?;
    let mut reader = blocks.metadata(pointer)?;
    reader.field(100)?;
    let count = reader.length()?;
    if count != table.columns.len() {
        return Err(corrupt("table statistics column count mismatch"));
    }
    for column in &table.columns {
        context.query.check()?;
        if !reader.boolean()? {
            return Err(corrupt("null column statistics"));
        }
        reader.field(100)?;
        statistics(&mut reader, Some(&column.data_type))?;
        if reader.optional(101)? && reader.boolean()? {
            reader.optional_unsigned(100, 0)?;
            reader.optional_unsigned(101, 0)?;
            if reader.optional(102)? && reader.boolean()? {
                reader.field(100)?;
                reader.unsigned()?;
                reader.field(101)?;
                reader.blob()?;
                reader.end()?;
            }
            reader.end()?;
        }
        reader.end()?;
    }
    if reader.optional(101)? && reader.boolean()? {
        sample(&mut reader)?;
    }
    reader.end()?;
    let groups = reader.fixed_u64()?;
    if groups > total as u64 + 1 {
        return Err(corrupt("too many row groups"));
    }
    let mut rows = Vec::new();
    for _ in 0..groups {
        context.query.check()?;
        reader.field(100)?;
        let start = reader.unsigned()?;
        reader.field(101)?;
        let count = reader.length()?;
        context.query.check_rows(count)?;
        let row_start = identities.push(start, count)?;
        reader.field(102)?;
        let column_count = reader.length()?;
        if column_count != table.columns.len() {
            return Err(corrupt("row group column count mismatch"));
        }
        let pointers = (0..column_count)
            .map(|_| reader.pointer())
            .collect::<Result<Vec<_>>>()?;
        reader.field(103)?;
        let delete_pointers = (0..reader.length()?)
            .map(|_| reader.pointer())
            .collect::<Result<Vec<_>>>()?;
        let deleted = super::visibility::deleted_rows(blocks, &delete_pointers, count)?;
        if reader.optional(104)? {
            reader.boolean()?;
        }
        if reader.optional(105)? {
            for _ in 0..reader.length()? {
                reader.unsigned()?;
            }
        }
        if reader.optional(106)? {
            reader.boolean()?;
        }
        read_column_ownership(&mut reader, blocks, column_count)?;
        reader.end()?;
        let mut group = vec![vec![Value::Null; column_count]; count];
        for (index, (column, pointer)) in table.columns.iter().zip(pointers).enumerate() {
            let mut column_reader = blocks.metadata(pointer)?;
            let values = read_column(
                context,
                &mut column_reader,
                Some(&column.data_type),
                count,
                row_start,
            )?;
            for (row, value) in group.iter_mut().zip(values) {
                row[index] = value;
            }
        }
        rows.extend(group.into_iter().zip(deleted).enumerate().filter_map(
            |(index, (row, deleted))| (!deleted).then_some((start + index as u64, row)),
        ));
    }
    identities.finish()?;
    context.query.check()?;
    Ok(rows)
}

/// Ownership records support C++ incremental column checkpoints. They do not
/// replace the column pointers: this reader materializes all columns and its
/// writer publishes a fresh compacted image. Consume and validate the packed
/// column markers/metadata block identifiers without treating them as values.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_column_ownership(
    reader: &mut Reader,
    blocks: &Blocks,
    columns: usize,
) -> Result<()> {
    if reader.optional(107)? {
        let mut previous = None;
        for _ in 0..reader.length()? {
            let entry = reader.unsigned()?;
            if entry >> 63 != 0 {
                let column = entry & !(1_u64 << 63);
                if column >= columns as u64 || previous.is_some_and(|previous| previous >= column) {
                    return Err(corrupt("invalid per-column metadata ownership marker"));
                }
                previous = Some(column);
            } else if previous.is_none()
                || entry >> 56 >= 64
                || (entry & 0x00ff_ffff_ffff_ffff) >= blocks.block_count
            {
                return Err(corrupt("invalid per-column metadata block identifier"));
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_column(
    context: &ReadContext<'_>,
    reader: &mut Reader,
    data_type: Option<&DataType>,
    count: usize,
    row_start: usize,
) -> Result<Vec<Value>> {
    context.query.check_rows(count)?;
    if let Some(data_type @ DataType::Nested(_)) = data_type {
        return super::nested::read_column(context, reader, data_type, count, row_start);
    }
    let mut output = read_segments(context, reader, data_type, data_type, count, row_start)?;
    if output.len() != count {
        return Err(corrupt("column row count mismatch"));
    }
    if data_type.is_some() {
        reader.field(101)?;
        let validity = read_column(context, reader, None, count, row_start)?;
        for (index, (value, valid)) in output.iter_mut().zip(validity).enumerate() {
            if index % 1024 == 0 {
                context.query.check()?;
            }
            if valid.is_null() {
                // The selected validity decoder explicitly preserves the base
                // decoder's inline NULLs (e.g. DICT_FSST dictionary entry zero).
                continue;
            } else if valid != Value::Boolean(true) {
                *value = Value::Null;
            } else if value.is_null() {
                return Err(corrupt("valid row has no decoded value"));
            }
        }
    }
    reader.end()?;
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_segments(
    context: &ReadContext<'_>,
    reader: &mut Reader,
    data_type: Option<&DataType>,
    physical: Option<&DataType>,
    count: usize,
    row_start: usize,
) -> Result<Vec<Value>> {
    let blocks = context.blocks;
    context.query.check_rows(count)?;
    let mut output = Vec::new();
    if reader.optional(100)? {
        for _ in 0..reader.length()? {
            context.query.check()?;
            if reader.optional(100)? {
                let actual = reader.unsigned()?;
                let expected = (row_start + output.len()) as u64;
                if actual != expected {
                    return Err(corrupt(format!(
                        "noncontiguous column segment identity: {actual}, expected {expected}, type {data_type:?}"
                    )));
                }
            }
            let segment_count = usize::try_from(reader.optional_unsigned(101, 0)?)
                .map_err(|_| corrupt("segment count overflow"))?;
            if segment_count > count.saturating_sub(output.len()) {
                return Err(corrupt("segment exceeds row group"));
            }
            reader.field(102)?;
            let (block, offset) = block_pointer(reader)?;
            reader.field(103)?;
            let compression = reader.unsigned()?;
            reader.field(104)?;
            let statistics = statistics(reader, data_type)?;
            if reader.optional(105)? && reader.boolean()? {
                // The uncompressed string state records overflow block ownership.
                if compression != 1
                    || !matches!(
                        data_type,
                        Some(DataType::Varchar | DataType::Blob | DataType::Bit | DataType::Bignum)
                    )
                {
                    return Err(Error::Unsupported(
                        "DuckDB compression segment state".into(),
                    ));
                }
                if reader.optional(1)? {
                    for _ in 0..reader.length()? {
                        reader.signed()?;
                    }
                }
                reader.end()?;
            }
            let byte_size = segment_byte_size(reader)?;
            reader.end()?;
            let data = if block == -1 {
                &[][..]
            } else {
                let id = u64::try_from(block).map_err(|_| corrupt("invalid segment block ID"))?;
                blocks
                    .block(id)?
                    .get(offset..)
                    .ok_or_else(|| corrupt("segment offset outside block"))?
            };
            let data = match byte_size {
                // C++ records size before replacing constant segments with
                // statistics-only storage, so an absent block can retain a
                // nonzero former payload extent. No decoder may read it.
                Some(size) if block == -1 && compression == 2 => {
                    if size > blocks.block_size - 8 {
                        return Err(corrupt("constant segment byte size exceeds block capacity"));
                    }
                    data
                }
                Some(size) => data
                    .get(..size)
                    .ok_or_else(|| corrupt(format!("segment byte size {size} exceeds {} available bytes (block {block}, offset {offset}, codec {compression})", data.len())))?,
                None => data,
            };
            output.extend(context.decoders.decode(
                CodecId(compression),
                DecodeInput {
                    kind: physical.map_or(SegmentType::Validity, SegmentType::Values),
                    count: segment_count,
                    data,
                    statistics: &statistics,
                },
                &DecodeContext {
                    blocks,
                    query: context.query,
                    vector_size: blocks.vector_size,
                },
            )?);
        }
    }
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn segment_byte_size(reader: &mut Reader) -> Result<Option<usize>> {
    if reader.optional(106)? && reader.boolean()? {
        let size = u32::try_from(reader.unsigned()?)
            .map_err(|_| corrupt("segment byte size overflows uint32"))?;
        Ok(Some(size as usize))
    } else {
        Ok(None)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn statistics(reader: &mut Reader, data_type: Option<&DataType>) -> Result<Statistics> {
    reader.field(100)?;
    reader.boolean()?;
    reader.field(101)?;
    let has_values = reader.boolean()?;
    reader.field(102)?;
    reader.unsigned()?;
    reader.field(103)?;
    let mut minimum = Value::Null;
    match data_type {
        Some(DataType::Nested(metadata)) => super::nested::read_statistics(reader, metadata)?,
        Some(DataType::Varchar | DataType::Blob | DataType::Bit | DataType::Bignum) => {
            string_statistics::read(reader)?;
        }
        Some(data_type) => {
            if *data_type == DataType::Interval {
                if reader.optional(200)? {
                    minimum = numeric_stat(reader, data_type)?;
                    reader.field(201)?;
                    numeric_stat(reader, data_type)?;
                }
            } else {
                reader.field(200)?;
                minimum = numeric_stat(reader, data_type)?;
                reader.field(201)?;
                numeric_stat(reader, data_type)?;
            }
        }
        None => {}
    }
    reader.end()?;
    reader.end()?;
    Ok(Statistics {
        has_values,
        minimum,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn numeric_stat(reader: &mut Reader, data_type: &DataType) -> Result<Value> {
    reader.field(100)?;
    let value = if reader.boolean()? {
        reader.field(101)?;
        match data_type {
            DataType::Boolean => Value::Boolean(reader.boolean()?),
            DataType::Date => Value::Date(super::binary::date(reader.signed()?)?),
            t if t.is_temporal() => super::temporal::read_metadata(reader, t)?,
            DataType::Float => Value::Float(reader.float()?),
            DataType::Double => Value::Double(reader.double()?),
            _ => super::primitive::read_numeric(reader, data_type)?,
        }
    } else {
        Value::Null
    };
    reader.end()?;
    Ok(value)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sample(reader: &mut Reader) -> Result<()> {
    if reader.optional(100)? && reader.boolean()? {
        reader.optional_unsigned(100, 0)?;
        reader.field(101)?;
        reader.double()?;
        for field in [102, 103, 104] {
            reader.optional_unsigned(field, 0)?;
        }
        if reader.optional(105)? {
            for _ in 0..reader.length()? {
                reader.field(100)?;
                reader.double()?;
                reader.field(101)?;
                reader.unsigned()?;
                reader.end()?;
            }
        }
        reader.end()?;
    }
    reader.field(101)?;
    reader.unsigned()?;
    if reader.optional(102)? {
        reader.boolean()?;
    }
    reader.optional_unsigned(200, 0)?;
    if reader.optional(201)? && reader.boolean()? {
        return Err(Error::Unsupported(
            "persisted DuckDB reservoir sample".into(),
        ));
    }
    reader.end()
}
