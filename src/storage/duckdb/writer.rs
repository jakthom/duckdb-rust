pub(super) mod constant;
mod index;

use super::{
    binary::{Encoder, checksum, corrupt},
    primitive::type_id,
};
mod catalog;
use crate::{
    catalog::{Catalog, TableDefinition},
    common::{DataType, Error, Result, Row, Value},
    storage::{TableStorage, table::Snapshot},
};
pub(super) use catalog::{column_definition, table_definition};

const ALLOCATION: usize = 262144;
const PAYLOAD: usize = ALLOCATION - 8;
const META_SIZE: usize = 4088;
const META_PAYLOAD: usize = META_SIZE - 8;

#[derive(Default)]
struct Arena {
    blocks: Vec<Vec<u8>>,
    metadata: Vec<(u64, u64)>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Arena {
    fn overflow(&mut self, data: &[u8]) -> Result<Vec<u64>> {
        if data.len() > 16 * 1024 * 1024 {
            return Err(Error::Resource("overflow string exceeds 16 MiB".into()));
        }
        let mut bytes = (data.len() as u32).to_le_bytes().to_vec();
        bytes.extend(data);
        let mut ids = Vec::new();
        for chunk in bytes.chunks(PAYLOAD - 8) {
            let id = self.block(chunk)?;
            if let Some(previous) = ids.last().copied() {
                self.blocks[previous as usize][PAYLOAD - 8..].copy_from_slice(&id.to_le_bytes());
            }
            self.blocks[id as usize][PAYLOAD - 8..].copy_from_slice(&u64::MAX.to_le_bytes());
            ids.push(id);
        }
        Ok(ids)
    }
    fn block(&mut self, data: &[u8]) -> Result<u64> {
        if data.len() > PAYLOAD {
            return Err(corrupt("checkpoint payload exceeds block"));
        }
        if self.blocks.len() >= 2047 {
            return Err(Error::Resource(
                "DuckDB checkpoint writer exceeds 512 MiB".into(),
            ));
        }
        let id = self.blocks.len() as u64;
        let mut block = vec![0; PAYLOAD];
        block[..data.len()].copy_from_slice(data);
        self.blocks.push(block);
        Ok(id)
    }
    fn slot(&mut self) -> Result<u64> {
        if self
            .metadata
            .last()
            .is_none_or(|(_, used)| *used == u64::MAX)
        {
            let id = self.block(&[])?;
            self.metadata.push((id, 0));
        }
        let (id, used) = self
            .metadata
            .last_mut()
            .ok_or_else(|| corrupt("missing metadata block"))?;
        let index = used.trailing_ones();
        *used |= 1u64 << index;
        Ok(*id | (u64::from(index) << 56))
    }
    fn write_slot(&mut self, pointer: u64, next: u64, data: &[u8]) {
        let block = &mut self.blocks[(pointer & 0x00ff_ffff_ffff_ffff) as usize];
        let offset = (pointer >> 56) as usize * META_SIZE;
        block[offset..offset + 8].copy_from_slice(&next.to_le_bytes());
        block[offset + 8..offset + 8 + data.len()].copy_from_slice(data);
    }
    fn metadata(&mut self, data: &[u8]) -> Result<u64> {
        let count = data.len().div_ceil(META_PAYLOAD).max(1);
        let pointers = (0..count)
            .map(|_| self.slot())
            .collect::<Result<Vec<_>>>()?;
        for (i, &pointer) in pointers.iter().enumerate() {
            let start = i * META_PAYLOAD;
            let end = (start + META_PAYLOAD).min(data.len());
            self.write_slot(
                pointer,
                pointers.get(i + 1).copied().unwrap_or(u64::MAX),
                &data[start..end],
            );
        }
        Ok(pointers[0])
    }
    fn finish(mut self, root: u64, previous: Option<super::CheckpointIdentity>) -> Result<Vec<u8>> {
        let free = self.slot()?;
        let mut free_data = Vec::new();
        free_data.extend(0u64.to_le_bytes());
        free_data.extend(0u64.to_le_bytes());
        free_data.extend((self.metadata.len() as u64).to_le_bytes());
        for &(id, used) in &self.metadata {
            free_data.extend(id.to_le_bytes());
            free_data.extend((!used).to_le_bytes());
        }
        if free_data.len() > META_PAYLOAD {
            return Err(Error::Resource(
                "checkpoint metadata registry exceeds one page".into(),
            ));
        }
        self.write_slot(free, u64::MAX, &free_data);
        let mut output = Vec::with_capacity(12288 + self.blocks.len() * ALLOCATION);
        let mut main = vec![0; 4088];
        main[..4].copy_from_slice(b"DUCK");
        main[4..12].copy_from_slice(&64u64.to_le_bytes());
        main[44..54].copy_from_slice(b"v0.1-rust\0");
        let iteration = if let Some(previous) = previous {
            main[116..132].copy_from_slice(&previous.identifier);
            previous
                .iteration
                .checked_add(1)
                .ok_or_else(|| Error::Resource("checkpoint generation exhausted".into()))?
        } else {
            1
        };
        append_checked(&mut output, &main)?;
        for (iteration, meta, list, count) in [
            (iteration, root, free, self.blocks.len() as u64),
            (0, u64::MAX, u64::MAX, 0),
        ] {
            let mut header = vec![0; 4088];
            for (i, value) in [iteration, meta, list, count, ALLOCATION as u64, 2048, 1]
                .into_iter()
                .enumerate()
            {
                header[i * 8..i * 8 + 8].copy_from_slice(&value.to_le_bytes());
            }
            append_checked(&mut output, &header)?;
        }
        for block in self.blocks {
            append_checked(&mut output, &block)?;
        }
        Ok(output)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn append_checked(output: &mut Vec<u8>, data: &[u8]) -> Result<()> {
    output.extend(checksum(data)?.to_le_bytes());
    output.extend(data);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn encode(snapshot: &Snapshot) -> Result<Vec<u8>> {
    encode_checkpoint(snapshot, None)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn encode_successor(
    snapshot: &Snapshot,
    previous: super::CheckpointIdentity,
) -> Result<Vec<u8>> {
    encode_checkpoint(snapshot, Some(previous))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn encode_checkpoint(
    snapshot: &Snapshot,
    previous: Option<super::CheckpointIdentity>,
) -> Result<Vec<u8>> {
    let tables = snapshot.tables()?;
    let schemas = snapshot.schemas()?;
    let mut arena = Arena::default();
    let mut catalog = Encoder::default();
    catalog.property(100, (tables.len() + schemas.len()) as u64);
    for schema in schemas {
        catalog.property(99, 2);
        catalog.field(100);
        catalog.boolean(true);
        catalog.property(100, 2);
        catalog.field(102);
        catalog.string(&schema)?;
        catalog.property(105, 0);
        catalog.end();
        catalog.end();
    }
    for table in tables {
        let rows: Vec<Row> = snapshot
            .scan(&table.name, &crate::parallel::QueryContext::background())?
            .into_iter()
            .map(|(_, row)| row)
            .collect();
        let pointer = table_data(&mut arena, &table, &rows)?;
        catalog.property(99, 1);
        catalog.field(100);
        catalog.boolean(true);
        table_definition(&mut catalog, &table)?;
        catalog.field(101);
        catalog.pointer(pointer);
        catalog.property(102, rows.len() as u64);
        catalog.property(103, 0);
        if !table.unique_keys.is_empty() {
            catalog.property(104, table.unique_keys.len() as u64);
            for (ordinal, key) in table.unique_keys.iter().enumerate() {
                index::serialize(&mut arena, &mut catalog, &table, key, ordinal, &rows)?;
            }
        }
        catalog.end();
    }
    catalog.end();
    let mut root = arena.metadata(&catalog.0)?;
    if previous.is_some_and(|previous| previous.root == root) {
        // Only the database header references the first catalog slot. Move it
        // without changing table/continuation pointers, then release its old
        // slot to the metadata allocator (normally reused by the free list).
        let replacement = arena.slot()?;
        let old_block = (root & 0x00ff_ffff_ffff_ffff) as usize;
        let old_offset = (root >> 56) as usize * META_SIZE;
        let data = arena.blocks[old_block][old_offset..old_offset + META_SIZE].to_vec();
        let new_block = (replacement & 0x00ff_ffff_ffff_ffff) as usize;
        let new_offset = (replacement >> 56) as usize * META_SIZE;
        arena.blocks[new_block][new_offset..new_offset + META_SIZE].copy_from_slice(&data);
        let (_, used) = arena
            .metadata
            .iter_mut()
            .find(|(id, _)| *id == old_block as u64)
            .ok_or_else(|| corrupt("catalog root outside metadata allocator"))?;
        *used &= !(1u64 << (root >> 56));
        root = replacement;
    }
    arena.finish(root, previous)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn table_data(arena: &mut Arena, table: &TableDefinition, rows: &[Row]) -> Result<u64> {
    let mut output = Encoder::default();
    output.property(100, table.columns.len() as u64);
    for (index, column) in table.columns.iter().enumerate() {
        output.boolean(true);
        output.field(100);
        statistics(
            &mut output,
            Some(&column.data_type),
            &rows.iter().map(|r| r[index].clone()).collect::<Vec<_>>(),
        )?;
        output.end();
    }
    output.end();
    output
        .0
        .extend((rows.len().div_ceil(122880) as u64).to_le_bytes());
    for (group, rows) in rows.chunks(122880).enumerate() {
        output.property(100, (group * 122880) as u64);
        output.property(101, rows.len() as u64);
        output.property(102, table.columns.len() as u64);
        for (index, column) in table.columns.iter().enumerate() {
            let values: Vec<_> = rows.iter().map(|r| r[index].clone()).collect();
            let pointer = column_data(arena, &column.data_type, &values, group * 122880)?;
            output.pointer(pointer);
        }
        output.property(103, 0);
        output.end();
    }
    arena.metadata(&output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn column_data(
    arena: &mut Arena,
    data_type: &DataType,
    values: &[Value],
    row_start: usize,
) -> Result<u64> {
    let mut segments = Vec::new();
    for (chunk, values) in values.chunks(2048).enumerate() {
        if *data_type == DataType::Varchar {
            let mut start = 0;
            while start < values.len() {
                let mut size = 8;
                let mut end = start;
                while end < values.len() {
                    let length = match &values[end] {
                        Value::Varchar(v) => v.len(),
                        _ => 0,
                    };
                    let length = if length > 4096 { 12 } else { length };
                    if size + length + 4 > PAYLOAD {
                        break;
                    }
                    size += length + 4;
                    end += 1;
                }
                segments.push(segment(
                    arena,
                    data_type,
                    &values[start..end],
                    row_start + chunk * 2048 + start,
                )?);
                start = end;
            }
        } else {
            segments.push(segment(arena, data_type, values, row_start + chunk * 2048)?);
        }
    }
    let mut output = Encoder::default();
    output.property(100, segments.len() as u64);
    for segment in segments {
        output.0.extend(segment);
    }
    output.field(101);
    output.property(100, 1);
    output.property(100, row_start as u64);
    output.property(101, values.len() as u64);
    output.field(102);
    let any_null = values.iter().any(Value::is_null);
    let all_null = values.iter().all(Value::is_null);
    let codec = if !any_null || all_null { 2 } else { 1 };
    output.field(100);
    if codec == 2 {
        output.signed(-1);
    } else {
        let mut bitmap = vec![255; values.len().div_ceil(64) * 8];
        for (i, value) in values.iter().enumerate() {
            if value.is_null() {
                bitmap[i / 8] &= !(1 << (i % 8));
            }
        }
        output.signed(arena.block(&bitmap)? as i64);
    }
    output.end();
    output.property(103, codec);
    output.field(104);
    statistics(&mut output, None, values)?;
    output.end();
    output.end();
    output.end();
    arena.metadata(&output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn segment(
    arena: &mut Arena,
    data_type: &DataType,
    values: &[Value],
    row_start: usize,
) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut overflow_blocks = Vec::new();
    if *data_type == DataType::Varchar {
        let mut strings = Vec::new();
        let mut offsets = Vec::new();
        let mut size = 0u32;
        for value in values {
            let bytes = match value {
                Value::Varchar(v) => v.as_bytes(),
                _ => &[],
            };
            if bytes.len() > 4096 {
                let blocks = arena.overflow(bytes)?;
                let mut marker = blocks[0].to_le_bytes().to_vec();
                marker.extend(0u32.to_le_bytes());
                size += 12;
                offsets.push(-(size as i32));
                strings.push(marker);
                overflow_blocks.extend(blocks);
            } else {
                size += bytes.len() as u32;
                offsets.push(size as i32);
                strings.push(bytes.to_vec());
            }
        }
        data.extend(size.to_le_bytes());
        data.extend((8 + 4 * values.len() as u32 + size).to_le_bytes());
        for offset in offsets {
            data.extend(offset.to_le_bytes());
        }
        for string in strings.into_iter().rev() {
            data.extend(string);
        }
    } else {
        for value in values {
            match data_type {
                DataType::Boolean => data.push(u8::from(*value == Value::Boolean(true))),
                DataType::Date => data.extend(
                    if value.is_null() {
                        0_i32
                    } else {
                        value.as_date()?.days()
                    }
                    .to_le_bytes(),
                ),
                DataType::Float => data.extend(
                    if value.is_null() {
                        0f32
                    } else {
                        value.as_f32()?
                    }
                    .to_le_bytes(),
                ),
                DataType::Double => data.extend(
                    if value.is_null() {
                        0f64
                    } else {
                        value.as_f64()?
                    }
                    .to_le_bytes(),
                ),
                _ => {
                    let value = if value.is_null() { 0 } else { value.as_i128()? };
                    let width = match data_type {
                        DataType::TinyInt => 1,
                        DataType::SmallInt => 2,
                        DataType::Integer => 4,
                        DataType::BigInt => 8,
                        _ => 16,
                    };
                    data.extend(&value.to_le_bytes()[..width]);
                }
            }
        }
    }
    let block = arena.block(&data)?;
    let mut output = Encoder::default();
    output.property(100, row_start as u64);
    output.property(101, values.len() as u64);
    output.field(102);
    output.field(100);
    output.signed(block as i64);
    output.end();
    output.property(103, 1);
    output.field(104);
    statistics(&mut output, Some(data_type), values)?;
    if !overflow_blocks.is_empty() {
        output.field(105);
        output.boolean(true);
        output.property(1, overflow_blocks.len() as u64);
        for block in overflow_blocks {
            output.signed(block as i64);
        }
        output.end();
    }
    output.end();
    Ok(output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn statistics(output: &mut Encoder, data_type: Option<&DataType>, values: &[Value]) -> Result<()> {
    output.field(100);
    output.boolean(values.iter().any(Value::is_null));
    output.field(101);
    output.boolean(values.iter().any(|v| !v.is_null()));
    output.property(102, 0);
    output.field(103);
    match data_type {
        None => {}
        Some(DataType::Varchar) => {
            output.field(200);
            output.blob(&[0; 8]);
            output.field(201);
            output.blob(&[255; 8]);
            output.field(202);
            output.boolean(true);
            output.field(203);
            output.boolean(true);
            output.property(
                204,
                values
                    .iter()
                    .map(|v| match v {
                        Value::Varchar(v) => v.len() as u64,
                        _ => 0,
                    })
                    .max()
                    .unwrap_or(0),
            );
        }
        Some(data_type) => {
            let mut minimum = None;
            let mut maximum = None;
            for value in values.iter().filter(|v| !v.is_null()) {
                if minimum.is_none() || value.compare(minimum.unwrap())?.is_lt() {
                    minimum = Some(value);
                }
                if maximum.is_none() || value.compare(maximum.unwrap())?.is_gt() {
                    maximum = Some(value);
                }
            }
            for (field, value) in [(200, minimum), (201, maximum)] {
                output.field(field);
                output.field(100);
                output.boolean(value.is_some());
                if let Some(value) = value {
                    output.field(101);
                    match data_type {
                        DataType::Boolean => output.boolean(*value == Value::Boolean(true)),
                        DataType::Date => output.signed(i64::from(value.as_date()?.days())),
                        DataType::Float => output.0.extend(value.as_f32()?.to_le_bytes()),
                        DataType::Double => output.0.extend(value.as_f64()?.to_le_bytes()),
                        DataType::HugeInt => {
                            let v = value.as_i128()?;
                            output.signed((v >> 64) as i64);
                            output.unsigned(v as u64);
                        }
                        _ => output.signed(
                            i64::try_from(value.as_i128()?)
                                .map_err(|_| corrupt("numeric statistics overflow"))?,
                        ),
                    }
                }
                output.end();
            }
        }
    }
    output.end();
    output.end();
    Ok(())
}
