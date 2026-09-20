mod index;

use super::binary::{Encoder, checksum, corrupt};
mod catalog;
use crate::{
    catalog::{Catalog, TableDefinition},
    common::{DataType, Error, Result, Row, Value},
    parallel::QueryContext,
    storage::table::Snapshot,
};
pub(super) use catalog::{column_definition, table_definition, type_definition};

const ALLOCATION: usize = 262144;
const PAYLOAD: usize = ALLOCATION - 8;
const META_SIZE: usize = 4088;
const META_PAYLOAD: usize = META_SIZE - 8;

#[derive(Default)]
pub(super) struct Arena {
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
    fn finish(
        mut self,
        root: u64,
        previous: Option<super::CheckpointIdentity>,
        version: u64,
    ) -> Result<Vec<u8>> {
        let (new_main, new_database) = super::write_support::new_headers(version)?;
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
        main[4..12].copy_from_slice(&previous.map_or(new_main, |p| p.main_version).to_le_bytes());
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
            for (i, value) in [
                iteration,
                meta,
                list,
                count,
                ALLOCATION as u64,
                2048,
                previous.map_or(new_database, |p| p.database_version),
            ]
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
    encode_version(snapshot, 64)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn encode_version(snapshot: &Snapshot, version: u64) -> Result<Vec<u8>> {
    encode_version_with_context(snapshot, version, &QueryContext::background())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn encode_version_with_context(
    snapshot: &Snapshot,
    version: u64,
    context: &QueryContext,
) -> Result<Vec<u8>> {
    context.check()?;
    super::write_support::new_headers(version)?;
    encode_checkpoint(snapshot, None, version, context, false)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn encode_successor(
    snapshot: &Snapshot,
    previous: super::CheckpointIdentity,
) -> Result<Vec<u8>> {
    encode_successor_with_context(snapshot, previous, &QueryContext::background())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn encode_successor_with_context(
    snapshot: &Snapshot,
    previous: super::CheckpointIdentity,
    context: &QueryContext,
) -> Result<Vec<u8>> {
    encode_checkpoint(
        snapshot,
        Some(previous),
        previous.storage_version(),
        context,
        false,
    )
}

#[cfg(test)]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn encode_version_generic(snapshot: &Snapshot, version: u64) -> Result<Vec<u8>> {
    encode_checkpoint(snapshot, None, version, &QueryContext::background(), true)
}

#[cfg(test)]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn encode_successor_generic(
    snapshot: &Snapshot,
    previous: super::CheckpointIdentity,
) -> Result<Vec<u8>> {
    encode_checkpoint(
        snapshot,
        Some(previous),
        previous.storage_version(),
        &QueryContext::background(),
        true,
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn encode_checkpoint(
    snapshot: &Snapshot,
    previous: Option<super::CheckpointIdentity>,
    version: u64,
    context: &QueryContext,
    force_generic: bool,
) -> Result<Vec<u8>> {
    // Physical values belong to the snapshot's retained type registry. Keep
    // every other selected caller service, setting and cancellation token.
    let context = context.clone().with_types(snapshot.type_registry());
    context.check()?;
    let tables = snapshot.tables()?;
    let named_types = snapshot.named_types()?;
    let views = snapshot.views()?;
    let scalar_macros = snapshot.scalar_macros()?;
    for definition in &named_types {
        context.check()?;
        definition.validate()?;
        super::write_support::checkpoint_type(&definition.data_type, version)?;
    }
    for table in &tables {
        context.check()?;
        for column in &table.columns {
            super::write_support::checkpoint_type(&column.data_type, version)?;
        }
    }
    let schemas = snapshot.schemas()?;
    let mut arena = Arena::default();
    let mut catalog = Encoder::default();
    let catalog_entries = tables
        .len()
        .checked_add(named_types.len())
        .and_then(|count| count.checked_add(views.len()))
        .and_then(|count| count.checked_add(scalar_macros.len()))
        .and_then(|count| count.checked_add(schemas.len()))
        .filter(|count| *count <= 16_777_216)
        .ok_or_else(|| Error::Resource("checkpoint catalog exceeds 16 million entries".into()))?;
    catalog.property(100, catalog_entries as u64);
    for schema in schemas {
        context.check()?;
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
    // DuckDB checkpoints restore named types before binding table entries.
    // Table columns still retain their concrete ENUM dictionaries and never
    // acquire a runtime link to the current same-name catalog type.
    for definition in named_types {
        context.check()?;
        catalog.property(99, 8);
        catalog.field(100);
        catalog.boolean(true);
        type_definition(&mut catalog, &definition, version)?;
        catalog.end();
    }
    for definition in views {
        context.check()?;
        catalog.property(99, 3);
        catalog.field(100);
        catalog.boolean(true);
        super::view::write(&mut catalog, &definition, version, &context)?;
        catalog.end();
    }
    for definition in scalar_macros {
        context.check()?;
        catalog.property(99, 30);
        catalog.field(100);
        catalog.boolean(true);
        super::macro_definition::write(&mut catalog, &definition, version, &context)?;
        catalog.end();
    }
    for table in tables {
        let packed = if force_generic {
            None
        } else {
            snapshot.implicit_append_bigints(&table, &context)?
        };
        let (pointer, row_count, rows) = if let Some(values) = packed.as_ref() {
            (
                table_data_packed_bigint_segments(&mut arena, &table, values.segments(), &context)?,
                values.len(),
                None,
            )
        } else {
            let rows: Vec<Row> = snapshot
                .scan_physical(&table.name, &context)?
                .into_iter()
                .map(|(_, row)| row)
                .collect();
            let pointer = table_data(&mut arena, &table, &rows, &context)?;
            (pointer, rows.len(), Some(rows))
        };
        catalog.property(99, 1);
        catalog.field(100);
        catalog.boolean(true);
        table_definition(&mut catalog, &table, version, &context)?;
        catalog.field(101);
        catalog.pointer(pointer);
        catalog.property(102, row_count as u64);
        catalog.property(103, 0);
        if !table.unique_keys.is_empty() {
            let rows = rows.as_deref().expect("indexed tables use generic rows");
            catalog.property(104, table.unique_keys.len() as u64);
            for (ordinal, key) in table.unique_keys.iter().enumerate() {
                index::serialize(&mut arena, &mut catalog, &table, key, ordinal, rows)?;
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
    let bytes = arena.finish(root, previous, version)?;
    context.check()?;
    Ok(bytes)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn table_data(
    arena: &mut Arena,
    table: &TableDefinition,
    rows: &[Row],
    context: &QueryContext,
) -> Result<u64> {
    let mut output = Encoder::default();
    output.property(100, table.columns.len() as u64);
    for (index, column) in table.columns.iter().enumerate() {
        output.boolean(true);
        output.field(100);
        statistics(
            &mut output,
            Some(&column.data_type),
            &rows.iter().map(|r| r[index].clone()).collect::<Vec<_>>(),
            context,
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
            let pointer = column_data(arena, &column.data_type, &values, group * 122880, context)?;
            output.pointer(pointer);
        }
        output.property(103, 0);
        output.end();
    }
    arena.metadata(&output.0)
}

/// Native equivalent of `table_data` for the single borrowed lane admitted by
/// `Snapshot::implicit_append_bigints`. Keep this intentionally narrow: the
/// generic row path remains the format reference for every other table.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn table_data_packed_bigint_segments(
    arena: &mut Arena,
    table: &TableDefinition,
    segments: &[&[i64]],
    context: &QueryContext,
) -> Result<u64> {
    let count = segments.iter().map(|segment| segment.len()).sum::<usize>();
    if let [values] = segments {
        return table_data_packed_bigint(arena, table, values, context);
    }
    context.check_rows(count)?;
    let mut output = Encoder::default();
    output.property(100, 1);
    output.boolean(true);
    output.field(100);
    packed_bigint_statistics_segments(&mut output, segments, true, context)?;
    output.end();
    output.end();
    output
        .0
        .extend((count.div_ceil(122880) as u64).to_le_bytes());
    let mut lane = PackedBigIntReader::new(segments);
    let mut start = 0;
    while start < count {
        let group = (count - start).min(122880);
        output.property(100, start as u64);
        output.property(101, group as u64);
        output.property(102, 1);
        output.pointer(packed_bigint_column_reader(
            arena, &mut lane, group, start, context,
        )?);
        output.property(103, 0);
        output.end();
        start += group;
    }
    arena.metadata(&output.0)
}

struct PackedBigIntReader<'a> {
    segments: &'a [&'a [i64]],
    segment: usize,
    offset: usize,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> PackedBigIntReader<'a> {
    fn new(segments: &'a [&'a [i64]]) -> Self {
        Self {
            segments,
            segment: 0,
            offset: 0,
        }
    }
    fn take(&mut self, count: usize) -> Option<std::borrow::Cow<'a, [i64]>> {
        debug_assert!(count <= 2048);
        let values = *self.segments.get(self.segment)?;
        let available = values.get(self.offset..)?;
        if available.len() >= count {
            let output = &available[..count];
            self.offset += count;
            if self.offset == values.len() {
                self.segment += 1;
                self.offset = 0;
            }
            return Some(std::borrow::Cow::Borrowed(output));
        }
        let mut output = Vec::with_capacity(count);
        while output.len() < count {
            let values = *self.segments.get(self.segment)?;
            let available = values.get(self.offset..)?;
            let take = (count - output.len()).min(available.len());
            output.extend_from_slice(&available[..take]);
            self.offset += take;
            if self.offset == values.len() {
                self.segment += 1;
                self.offset = 0;
            }
        }
        Some(std::borrow::Cow::Owned(output))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn packed_bigint_column_reader(
    arena: &mut Arena,
    lane: &mut PackedBigIntReader<'_>,
    count: usize,
    row_start: usize,
    context: &QueryContext,
) -> Result<u64> {
    let mut segments = Vec::new();
    for offset in (0..count).step_by(2048) {
        context.check()?;
        let values = lane
            .take((count - offset).min(2048))
            .ok_or_else(|| corrupt("packed BIGINT lane truncated"))?;
        segments.push(packed_bigint_segment(
            arena,
            &values,
            row_start + offset,
            context,
        )?);
    }
    let mut output = Encoder::default();
    output.property(100, segments.len() as u64);
    for segment in segments {
        output.0.extend(segment);
    }
    output.field(101);
    output
        .0
        .extend(packed_bigint_validity_count(count, row_start, context)?);
    output.end();
    arena.metadata(&output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn packed_bigint_validity_count(
    count: usize,
    row_start: usize,
    context: &QueryContext,
) -> Result<Vec<u8>> {
    if count == 0 {
        return packed_bigint_validity(&[], row_start, context);
    }
    let mut output = Encoder::default();
    context.check()?;
    output.property(100, 1);
    output.property(100, row_start as u64);
    output.property(101, count as u64);
    output.field(102);
    output.field(100);
    output.signed(-1);
    output.end();
    output.property(103, 2);
    output.field(104);
    packed_bigint_statistics_bounds(&mut output, count, None, false, context)?;
    output.end();
    output.end();
    Ok(output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn packed_bigint_statistics_segments(
    output: &mut Encoder,
    segments: &[&[i64]],
    typed: bool,
    context: &QueryContext,
) -> Result<()> {
    let mut count = 0usize;
    let mut bounds: Option<(i64, i64)> = None;
    for segment in segments {
        for values in segment.chunks(2048) {
            context.check()?;
            count += values.len();
            for value in values {
                bounds = Some(match bounds {
                    Some((minimum, maximum)) => (minimum.min(*value), maximum.max(*value)),
                    None => (*value, *value),
                });
            }
        }
    }
    packed_bigint_statistics_bounds(output, count, bounds, typed, context)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn table_data_packed_bigint(
    arena: &mut Arena,
    table: &TableDefinition,
    values: &[i64],
    context: &QueryContext,
) -> Result<u64> {
    debug_assert_eq!(table.columns.len(), 1);
    debug_assert_eq!(table.columns[0].data_type, DataType::BigInt);
    context.check_rows(values.len())?;
    let mut output = Encoder::default();
    output.property(100, 1);
    output.boolean(true);
    output.field(100);
    packed_bigint_statistics(&mut output, values, true, context)?;
    output.end();
    output.end();
    output
        .0
        .extend((values.len().div_ceil(122880) as u64).to_le_bytes());
    for (group, values) in values.chunks(122880).enumerate() {
        context.check()?;
        output.property(100, (group * 122880) as u64);
        output.property(101, values.len() as u64);
        output.property(102, 1);
        output.pointer(packed_bigint_column(
            arena,
            values,
            group * 122880,
            context,
        )?);
        output.property(103, 0);
        output.end();
    }
    arena.metadata(&output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn packed_bigint_column(
    arena: &mut Arena,
    values: &[i64],
    row_start: usize,
    context: &QueryContext,
) -> Result<u64> {
    let mut segments = Vec::new();
    for (chunk, values) in values.chunks(2048).enumerate() {
        segments.push(packed_bigint_segment(
            arena,
            values,
            row_start + chunk * 2048,
            context,
        )?);
    }
    let mut output = Encoder::default();
    output.property(100, segments.len() as u64);
    for segment in segments {
        output.0.extend(segment);
    }
    output.field(101);
    output
        .0
        .extend(packed_bigint_validity(values, row_start, context)?);
    output.end();
    arena.metadata(&output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn packed_bigint_validity(
    values: &[i64],
    row_start: usize,
    context: &QueryContext,
) -> Result<Vec<u8>> {
    context.check()?;
    let mut output = Encoder::default();
    if values.is_empty() {
        output.property(100, 0);
        output.end();
        return Ok(output.0);
    }
    output.property(100, 1);
    output.property(100, row_start as u64);
    output.property(101, values.len() as u64);
    output.field(102);
    output.field(100);
    output.signed(-1);
    output.end();
    output.property(103, 2);
    output.field(104);
    packed_bigint_statistics(&mut output, values, false, context)?;
    output.end();
    output.end();
    Ok(output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn packed_bigint_segment(
    arena: &mut Arena,
    values: &[i64],
    row_start: usize,
    context: &QueryContext,
) -> Result<Vec<u8>> {
    context.check()?;
    let mut data = Vec::with_capacity(std::mem::size_of_val(values));
    for value in values {
        data.extend(value.to_le_bytes());
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
    packed_bigint_statistics(&mut output, values, true, context)?;
    output.end();
    Ok(output.0)
}

/// Emit the byte-for-byte `statistics` shape for an all-valid BIGINT slice.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn packed_bigint_statistics(
    output: &mut Encoder,
    values: &[i64],
    typed: bool,
    context: &QueryContext,
) -> Result<()> {
    if !typed {
        return packed_bigint_statistics_bounds(output, values.len(), None, false, context);
    }
    let mut bounds: Option<(i64, i64)> = None;
    for values in values.chunks(2048) {
        context.check()?;
        for value in values {
            bounds = Some(match bounds {
                Some((minimum, maximum)) => (minimum.min(*value), maximum.max(*value)),
                None => (*value, *value),
            });
        }
    }
    packed_bigint_statistics_bounds(output, values.len(), bounds, typed, context)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn packed_bigint_statistics_bounds(
    output: &mut Encoder,
    count: usize,
    bounds: Option<(i64, i64)>,
    typed: bool,
    context: &QueryContext,
) -> Result<()> {
    context.check()?;
    output.field(100);
    output.boolean(false);
    output.field(101);
    output.boolean(count != 0);
    output.property(102, 0);
    output.field(103);
    if typed {
        for (field, value) in [
            (200, bounds.map(|(minimum, _)| minimum)),
            (201, bounds.map(|(_, maximum)| maximum)),
        ] {
            output.field(field);
            output.field(100);
            output.boolean(value.is_some());
            if let Some(value) = value {
                output.field(101);
                output.signed(value);
            }
            output.end();
        }
    }
    output.end();
    output.end();
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn column_data(
    arena: &mut Arena,
    data_type: &DataType,
    values: &[Value],
    row_start: usize,
    context: &QueryContext,
) -> Result<u64> {
    let bytes = column_bytes(arena, data_type, values, row_start, context)?;
    arena.metadata(&bytes)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn column_bytes(
    arena: &mut Arena,
    data_type: &DataType,
    values: &[Value],
    row_start: usize,
    context: &QueryContext,
) -> Result<Vec<u8>> {
    context.check()?;
    if matches!(data_type, DataType::Nested(_)) {
        return super::nested::write_column(arena, data_type, values, row_start, context);
    }
    let mut segments = Vec::new();
    for (chunk, values) in values.chunks(2048).enumerate() {
        if matches!(
            data_type,
            DataType::Varchar | DataType::Blob | DataType::Bit | DataType::Bignum
        ) {
            let mut start = 0;
            while start < values.len() {
                let mut size = 8;
                let mut end = start;
                while end < values.len() {
                    let length = match &values[end] {
                        Value::Varchar(v) => v.len(),
                        Value::Blob(v) => v.len(),
                        Value::Bit(v) => v.bytes().len() + 1,
                        Value::Bignum(v) => v.byte_len() + 3,
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
                    context,
                )?);
                start = end;
            }
        } else {
            segments.push(segment(
                arena,
                data_type,
                values,
                row_start + chunk * 2048,
                context,
            )?);
        }
    }
    let mut output = Encoder::default();
    output.property(100, segments.len() as u64);
    for segment in segments {
        output.0.extend(segment);
    }
    output.field(101);
    output
        .0
        .extend(validity_bytes(arena, values, row_start, context)?);
    output.end();
    Ok(output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn validity_bytes(
    arena: &mut Arena,
    values: &[Value],
    row_start: usize,
    context: &QueryContext,
) -> Result<Vec<u8>> {
    context.check()?;
    let mut output = Encoder::default();
    if values.is_empty() {
        output.property(100, 0);
        output.end();
        return Ok(output.0);
    }
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
    statistics(&mut output, None, values, context)?;
    output.end();
    output.end();
    Ok(output.0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn segment(
    arena: &mut Arena,
    data_type: &DataType,
    values: &[Value],
    row_start: usize,
    context: &QueryContext,
) -> Result<Vec<u8>> {
    segment_with_statistics(
        arena, data_type, values, row_start, data_type, values, context,
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn segment_with_statistics(
    arena: &mut Arena,
    data_type: &DataType,
    values: &[Value],
    row_start: usize,
    statistics_type: &DataType,
    statistics_values: &[Value],
    context: &QueryContext,
) -> Result<Vec<u8>> {
    context.check()?;
    let mut data = Vec::new();
    let mut overflow_blocks = Vec::new();
    if matches!(
        data_type,
        DataType::Varchar | DataType::Blob | DataType::Bit | DataType::Bignum
    ) {
        let mut strings = Vec::new();
        let mut offsets = Vec::new();
        let mut size = 0u32;
        for value in values {
            let bit_bytes;
            let bytes = match value {
                Value::Varchar(v) => v.as_bytes(),
                Value::Blob(v) => v.as_slice(),
                Value::Bit(v) => {
                    bit_bytes = v.to_native(|| context.check())?;
                    &bit_bytes
                }
                Value::Bignum(v) => {
                    bit_bytes = v.to_native(|| context.check())?;
                    &bit_bytes
                }
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
                t if t.is_temporal() => {
                    if value.is_null() {
                        data.resize(data.len() + super::primitive::width(t)?, 0);
                    } else {
                        value.as_temporal()?.append_storage(&mut data)?;
                    }
                }
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
                    let value = match value {
                        Value::Null => 0,
                        Value::Decimal { value, .. } => *value,
                        Value::Unsigned(value) => *value as i128,
                        Value::Uuid(value) => (*value ^ (1_u128 << 127)) as i128,
                        Value::Enum(value) => i128::from(value.ordinal),
                        _ => value.as_i128()?,
                    };
                    let width = super::primitive::width(data_type)?;
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
    statistics(
        &mut output,
        Some(statistics_type),
        statistics_values,
        context,
    )?;
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
pub(super) fn statistics(
    output: &mut Encoder,
    data_type: Option<&DataType>,
    values: &[Value],
    context: &QueryContext,
) -> Result<()> {
    context.check()?;
    output.field(100);
    output.boolean(values.iter().any(Value::is_null));
    output.field(101);
    output.boolean(values.iter().any(|v| !v.is_null()));
    output.property(102, 0);
    output.field(103);
    match data_type {
        Some(DataType::Nested(metadata)) => {
            super::nested::write_statistics(output, metadata, values, context)?
        }
        None => {}
        // The writer's legacy compatibility target predates interval stats.
        Some(DataType::Interval) => {}
        Some(DataType::Varchar | DataType::Blob | DataType::Bit | DataType::Bignum) => {
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
                        Value::Blob(v) => v.len() as u64,
                        Value::Bit(v) => v.bytes().len() as u64 + 1,
                        Value::Bignum(v) => v.byte_len() as u64 + 3,
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
                        t if t.is_temporal() => {
                            super::temporal::write_metadata(output, value.as_temporal()?)?
                        }
                        DataType::Float => output.0.extend(value.as_f32()?.to_le_bytes()),
                        DataType::Double => output.0.extend(value.as_f64()?.to_le_bytes()),
                        _ => super::primitive::write_numeric(output, value, data_type)?,
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

#[cfg(test)]
mod packed_bigint_tests {
    use super::*;
    use crate::{
        catalog::{Catalog, CatalogMut, ColumnDefinition, TableName, UniqueKey},
        common::type_registry::{KeyWriter, PrimitiveTypes, TypeAdapter, TypeRegistry},
        common::vector::{DataChunk, Vector},
        parallel::InterruptHandle,
        storage::{TableStorageMut, format::SnapshotFormat},
    };
    use std::{cmp::Ordering, sync::Arc};

    #[derive(Debug)]
    struct LogicalBigint;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl TypeAdapter for LogicalBigint {
        fn name(&self) -> &'static str {
            "packed-bigint-logical-guard"
        }
        fn validate_type(&self, ty: &DataType) -> Result<()> {
            PrimitiveTypes.validate_type(ty)
        }
        fn validate_value(&self, ty: &DataType, value: &Value, query: &QueryContext) -> Result<()> {
            PrimitiveTypes.validate_value(ty, value, query)
        }
        fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
            PrimitiveTypes.common_type(left, right)
        }
        fn compare(
            &self,
            ty: &DataType,
            left: &Value,
            right: &Value,
            query: &QueryContext,
        ) -> Result<Ordering> {
            PrimitiveTypes.compare(ty, left, right, query)
        }
        fn write_key(
            &self,
            ty: &DataType,
            value: &Value,
            output: &mut KeyWriter<'_>,
            query: &QueryContext,
        ) -> Result<()> {
            PrimitiveTypes.write_key(ty, value, output, query)
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn bigint_snapshot(values: impl IntoIterator<Item = i64>) -> Result<Snapshot> {
        let name = TableName::main("packed_bigints");
        let mut snapshot = Snapshot::default();
        snapshot.create_table(
            TableDefinition {
                name: name.clone(),
                columns: vec![ColumnDefinition::new("v", DataType::BigInt)],
                unique_keys: vec![],
            },
            false,
        )?;
        snapshot.insert(
            &name,
            values
                .into_iter()
                .map(|value| vec![Value::Integer(i128::from(value))])
                .collect(),
            &QueryContext::background(),
        )?;
        Ok(snapshot)
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn packed_bigint_checkpoint_matches_generic_bytes_for_supported_versions_and_boundaries()
    -> Result<()> {
        let mut values = vec![i64::MIN, -1, 0, i64::MAX];
        values.extend(1..=10_000);
        let snapshot = bigint_snapshot(values)?;
        let table = Catalog::tables(&snapshot)?.pop().expect("table exists");
        assert!(
            snapshot
                .implicit_append_bigints(&table, &QueryContext::background())?
                .is_some()
        );
        for version in [64, 68] {
            assert_eq!(
                encode_version(&snapshot, version)?,
                encode_version_generic(&snapshot, version)?,
                "version {version}"
            );
        }
        let original = encode_version(&snapshot, 64)?;
        let previous = super::super::CheckpointIdentity::read(&original)?;
        let packed_successor = encode_successor(&snapshot, previous)?;
        let generic_successor = encode_successor_generic(&snapshot, previous)?;
        assert_eq!(packed_successor, generic_successor);
        let packed_identity = super::super::CheckpointIdentity::read(&packed_successor)?;
        let generic_identity = super::super::CheckpointIdentity::read(&generic_successor)?;
        assert_eq!(packed_identity.identifier, generic_identity.identifier);
        assert_eq!(packed_identity.iteration, generic_identity.iteration);
        assert_eq!(packed_identity.root, generic_identity.root);
        assert_eq!(packed_identity.main_version, generic_identity.main_version);
        assert_eq!(
            packed_identity.database_version,
            generic_identity.database_version
        );
        let restored = super::super::DuckDbFormat::default()
            .decode(packed_successor, snapshot.type_registry())?;
        assert_eq!(Catalog::tables(&restored)?, Catalog::tables(&snapshot)?);
        assert_eq!(
            restored.scan_physical(&table.name, &QueryContext::background())?,
            snapshot.scan_physical(&table.name, &QueryContext::background())?
        );
        Ok(())
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn packed_bigint_checkpoint_matches_generic_empty_and_layout_boundaries() -> Result<()> {
        for values in [Vec::new(), (0..(122_880 + 2_048)).map(i64::from).collect()] {
            let snapshot = bigint_snapshot(values)?;
            let table = Catalog::tables(&snapshot)?.pop().expect("table exists");
            let packed = snapshot.implicit_append_bigints(&table, &QueryContext::background())?;
            assert_eq!(packed.is_some(), snapshot.next_row_id(&table.name)? != 0);
            assert_eq!(
                encode_version(&snapshot, 64)?,
                encode_version_generic(&snapshot, 64)?
            );
        }
        Ok(())
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn packed_bigint_checkpoint_rejects_definition_null_and_physical_order_guards() -> Result<()> {
        let mut snapshot = bigint_snapshot(0..10_000)?;
        let table = Catalog::tables(&snapshot)?.pop().expect("table exists");
        assert!(
            snapshot
                .implicit_append_bigints(&table, &QueryContext::background())?
                .is_some()
        );

        let mut different_type = table.clone();
        different_type.columns[0].data_type = DataType::Integer;
        assert!(
            snapshot
                .implicit_append_bigints(&different_type, &QueryContext::background())?
                .is_none()
        );
        different_type
            .columns
            .push(ColumnDefinition::new("extra", DataType::BigInt));
        assert!(
            snapshot
                .implicit_append_bigints(&different_type, &QueryContext::background())?
                .is_none()
        );

        snapshot.delete(&table.name, &[0], &QueryContext::background())?;
        assert!(
            snapshot
                .implicit_append_bigints(&table, &QueryContext::background())?
                .is_none()
        );

        let name = TableName::main("nullable_bigints");
        let mut nullable = Snapshot::default();
        nullable.create_table(
            TableDefinition {
                name: name.clone(),
                columns: vec![ColumnDefinition::new("v", DataType::BigInt)],
                unique_keys: vec![],
            },
            false,
        )?;
        nullable.insert(&name, vec![vec![Value::Null]], &QueryContext::background())?;
        let table = Catalog::tables(&nullable)?.pop().expect("table exists");
        assert!(
            nullable
                .implicit_append_bigints(&table, &QueryContext::background())?
                .is_none()
        );

        let indexed_name = TableName::main("indexed_bigints");
        let mut indexed = Snapshot::default();
        indexed.create_table(
            TableDefinition {
                name: indexed_name.clone(),
                columns: vec![ColumnDefinition::new("v", DataType::BigInt)],
                unique_keys: vec![UniqueKey {
                    columns: vec![0],
                    primary: false,
                }],
            },
            false,
        )?;
        indexed.insert(
            &indexed_name,
            vec![vec![Value::Integer(1)]],
            &QueryContext::background(),
        )?;
        let table = Catalog::tables(&indexed)?.pop().expect("table exists");
        assert!(
            indexed
                .implicit_append_bigints(&table, &QueryContext::background())?
                .is_none()
        );

        let multi_name = TableName::main("multi_bigints");
        let mut multi = Snapshot::default();
        multi.create_table(
            TableDefinition {
                name: multi_name.clone(),
                columns: vec![
                    ColumnDefinition::new("left", DataType::BigInt),
                    ColumnDefinition::new("right", DataType::BigInt),
                ],
                unique_keys: vec![],
            },
            false,
        )?;
        multi.insert(
            &multi_name,
            vec![vec![Value::Integer(1), Value::Integer(2)]],
            &QueryContext::background(),
        )?;
        let table = Catalog::tables(&multi)?.pop().expect("table exists");
        assert!(
            multi
                .implicit_append_bigints(&table, &QueryContext::background())?
                .is_none()
        );

        let interrupted = InterruptHandle::default();
        let cancelled = QueryContext::new(interrupted.clone(), None, 2, 10)?;
        interrupted.interrupt();
        let snapshot = bigint_snapshot(0..10_000)?;
        let table = Catalog::tables(&snapshot)?.pop().expect("table exists");
        assert!(matches!(
            snapshot.implicit_append_bigints(&table, &cancelled),
            Err(Error::Interrupted)
        ));

        let mut types = TypeRegistry::builtins();
        types.replace(DataType::BigInt.family(), Arc::new(LogicalBigint))?;
        let types = Arc::new(types);
        let mut selected = Snapshot::new(types.clone());
        let name = TableName::main("selected_bigints");
        selected.create_table(
            TableDefinition {
                name: name.clone(),
                columns: vec![ColumnDefinition::new("v", DataType::BigInt)],
                unique_keys: vec![],
            },
            false,
        )?;
        selected.insert(
            &name,
            (0..10_000)
                .map(|value| vec![Value::Integer(i128::from(value))])
                .collect(),
            &QueryContext::background().with_types(types),
        )?;
        let table = Catalog::tables(&selected)?.pop().expect("table exists");
        assert!(
            selected
                .implicit_append_bigints(
                    &table,
                    &QueryContext::background().with_types(selected.type_registry())
                )?
                .is_none()
        );
        Ok(())
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn packed_bigint_checkpoint_matches_generic_ctas_chunks_and_crosses_native_boundaries()
    -> Result<()> {
        for widths in [
            vec![17usize, 2_031, 2_049, 5_903],
            vec![17, 2_031, 2_049, 5_903, 112_881, 2_048],
        ] {
            let name = TableName::main("ctas_chunks");
            let mut snapshot = Snapshot::default();
            snapshot.create_table(
                TableDefinition {
                    name: name.clone(),
                    columns: vec![ColumnDefinition::new("v", DataType::BigInt)],
                    unique_keys: vec![],
                },
                false,
            )?;
            let mut next = 0i64;
            let mut chunks = Vec::new();
            for width in widths {
                let values =
                    Vector::try_bigints((next..next + width as i64).map(|value| Ok(Some(value))))?;
                next += width as i64;
                chunks.push(DataChunk::new(vec![values], width)?);
            }
            snapshot.insert_chunks(&name, chunks, &QueryContext::background())?;
            let table = Catalog::tables(&snapshot)?.pop().expect("table exists");
            assert!(matches!(
                snapshot.implicit_append_bigints(&table, &QueryContext::background())?,
                Some(crate::storage::table::PackedBigInts::Chunks(_))
            ));
            for version in [64, 68] {
                let bytes = encode_version(&snapshot, version)?;
                assert_eq!(bytes, encode_version_generic(&snapshot, version)?);
                let previous = super::super::CheckpointIdentity::read(&bytes)?;
                let successor = encode_successor(&snapshot, previous)?;
                assert_eq!(successor, encode_successor_generic(&snapshot, previous)?);
                let restored = super::super::DuckDbFormat::default()
                    .decode(successor, snapshot.type_registry())?;
                assert_eq!(
                    restored.scan_physical(&name, &QueryContext::background())?,
                    snapshot.scan_physical(&name, &QueryContext::background())?
                );
            }
        }
        Ok(())
    }
}
