//! DuckDB ART checkpoint representation. Runtime index algorithms do not depend
//! on this encoding; tree nodes and allocator metadata are published together.
use super::{Arena, Encoder, PAYLOAD};
use crate::{
    catalog::{TableDefinition, UniqueKey},
    common::{DataType, Error, Result, Row, Value},
};

// Legacy ART prefix/leaf/Node4/Node16/Node48/Node256 allocation strides.
const STRIDES: [usize; 6] = [24, 48, 40, 152, 648, 2056];
const PREFIX_BYTES: usize = 15;
const INLINE_LEAF: u64 = 7 << 56;

struct Buffer {
    bytes: Vec<u8>,
    count: usize,
}
struct Allocator {
    stride: usize,
    capacity: usize,
    bitmap: usize,
    buffers: Vec<Buffer>,
}

impl Allocator {
    fn new(stride: usize) -> Self {
        let (mut capacity, mut words, mut bytes) = (0, 0, 0);
        while bytes < PAYLOAD {
            if words == 0 || (words * 64) % capacity == 0 {
                words += 1;
                bytes += 8;
            }
            let remaining = ((PAYLOAD - bytes) / stride).min(64);
            if remaining == 0 {
                break;
            }
            capacity += remaining;
            bytes += remaining * stride;
        }
        Self {
            stride,
            capacity,
            bitmap: words * 8,
            buffers: Vec::new(),
        }
    }
    fn allocate(&mut self, kind: u64, bytes: &[u8]) -> Result<u64> {
        if bytes.len() != self.stride {
            return Err(Error::Internal("ART allocation stride".into()));
        }
        if self.buffers.last().is_none_or(|b| b.count == self.capacity) {
            if self.buffers.len() >= 2047 {
                return Err(Error::Resource(
                    "ART allocation exceeds checkpoint limit".into(),
                ));
            }
            let mut bytes = vec![0; PAYLOAD];
            bytes[..self.bitmap].fill(255);
            self.buffers.push(Buffer { bytes, count: 0 });
        }
        let id = self.buffers.len() - 1;
        let buffer = &mut self.buffers[id];
        let index = buffer.count;
        let offset = self.bitmap + index * self.stride;
        buffer.bytes[offset..offset + self.stride].copy_from_slice(bytes);
        buffer.bytes[index / 8] &= !(1 << (index % 8));
        buffer.count += 1;
        Ok((kind << 56) | ((index as u64) << 32) | id as u64)
    }
    fn serialize(&self, arena: &mut Arena, output: &mut Encoder) -> Result<()> {
        output.property(100, self.stride as u64);
        if !self.buffers.is_empty() {
            output.property(101, self.buffers.len() as u64);
            for id in 0..self.buffers.len() {
                output.unsigned(id as u64);
            }
            output.property(102, self.buffers.len() as u64);
            for buffer in &self.buffers {
                let size = self.bitmap + buffer.count * self.stride;
                output.field(100);
                output.signed(arena.block(&buffer.bytes[..size])? as i64);
                output.end();
            }
            output.property(103, self.buffers.len() as u64);
            for buffer in &self.buffers {
                output.unsigned(buffer.count as u64);
            }
            output.property(104, self.buffers.len() as u64);
            for buffer in &self.buffers {
                output.unsigned((self.bitmap + buffer.count * self.stride) as u64);
            }
            let free: Vec<_> = self
                .buffers
                .iter()
                .enumerate()
                .filter(|(_, b)| b.count < self.capacity)
                .collect();
            output.property(105, free.len() as u64);
            for (id, _) in free {
                output.unsigned(id as u64);
            }
        }
        output.end();
        Ok(())
    }
}

struct Tree {
    allocators: [Allocator; 6],
}

impl Tree {
    fn prefix(&mut self, bytes: &[u8], mut child: u64) -> Result<u64> {
        for part in bytes.chunks(PREFIX_BYTES).rev() {
            let mut node = [0; 24];
            node[..part.len()].copy_from_slice(part);
            node[PREFIX_BYTES] = part.len() as u8;
            node[16..].copy_from_slice(&child.to_le_bytes());
            child = self.allocators[0].allocate(1, &node)?;
        }
        Ok(child)
    }
    fn build(&mut self, entries: &[(Vec<u8>, u64)]) -> Result<u64> {
        // Explicit postorder traversal also handles adversarial prefix keys
        // without consuming one call frame per byte of branching depth.
        enum Work<'a> {
            Build(&'a [(Vec<u8>, u64)], usize),
            Prefix(&'a [u8]),
            Branch(Vec<u8>),
        }
        let mut work = vec![Work::Build(entries, 0)];
        let mut output = Vec::new();
        while let Some(task) = work.pop() {
            match task {
                Work::Prefix(bytes) => {
                    let child = output
                        .pop()
                        .ok_or_else(|| Error::Internal("ART traversal".into()))?;
                    output.push(self.prefix(bytes, child)?);
                }
                Work::Branch(bytes) => {
                    let start = output
                        .len()
                        .checked_sub(bytes.len())
                        .ok_or_else(|| Error::Internal("ART traversal".into()))?;
                    let children: Vec<_> = bytes.into_iter().zip(output.drain(start..)).collect();
                    output.push(self.branch(&children)?);
                }
                Work::Build(entries, depth) => {
                    let Some(first) = entries.first() else {
                        output.push(0);
                        continue;
                    };
                    if entries.len() == 1 {
                        output.push(self.prefix(&first.0[depth..], INLINE_LEAF | first.1)?);
                        continue;
                    }
                    let last = &entries[entries.len() - 1];
                    let shared = first.0[depth..]
                        .iter()
                        .zip(&last.0[depth..])
                        .take_while(|(a, b)| a == b)
                        .count();
                    if shared > 0 {
                        work.push(Work::Prefix(&first.0[depth..depth + shared]));
                        work.push(Work::Build(entries, depth + shared));
                        continue;
                    }
                    if first.0.len() <= depth {
                        return Err(Error::Constraint("duplicate or invalid ART key".into()));
                    }
                    let mut bytes = Vec::new();
                    let mut branches = Vec::new();
                    let mut start = 0;
                    while start < entries.len() {
                        let byte = entries[start].0[depth];
                        let mut end = start + 1;
                        while end < entries.len() && entries[end].0.get(depth) == Some(&byte) {
                            end += 1;
                        }
                        bytes.push(byte);
                        branches.push(Work::Build(&entries[start..end], depth + 1));
                        start = end;
                    }
                    work.push(Work::Branch(bytes));
                    work.extend(branches.into_iter().rev());
                }
            }
        }
        output
            .pop()
            .ok_or_else(|| Error::Internal("ART traversal has no root".into()))
    }
    fn branch(&mut self, children: &[(u8, u64)]) -> Result<u64> {
        let (kind, offset) = match children.len() {
            0..=4 => (3, 8),
            5..=16 => (4, 24),
            17..=48 => (5, 264),
            _ => (6, 8),
        };
        let mut node = vec![0; STRIDES[kind - 1]];
        if kind == 6 {
            node[..2].copy_from_slice(&(children.len() as u16).to_le_bytes());
        } else {
            node[0] = children.len() as u8;
        }
        if kind == 5 {
            node[1..257].fill(48);
        }
        for (i, &(byte, child)) in children.iter().enumerate() {
            let slot = match kind {
                3 | 4 => {
                    node[1 + i] = byte;
                    i
                }
                5 => {
                    node[1 + byte as usize] = i as u8;
                    i
                }
                _ => byte as usize,
            };
            node[offset + 8 * slot..offset + 8 * slot + 8].copy_from_slice(&child.to_le_bytes());
        }
        self.allocators[kind - 1].allocate(kind as u64, &node)
    }
}

pub(super) fn serialize(
    arena: &mut Arena,
    output: &mut Encoder,
    table: &TableDefinition,
    key: &UniqueKey,
    ordinal: usize,
    rows: &[Row],
) -> Result<()> {
    let mut entries = Vec::new();
    for (id, row) in rows.iter().enumerate() {
        let mut bytes = Vec::new();
        if key.columns.iter().any(|&i| row[i].is_null()) {
            continue;
        }
        for &column in &key.columns {
            encode_value(&row[column], &table.columns[column].data_type, &mut bytes)?;
        }
        if bytes.len() > 8192 * PREFIX_BYTES {
            return Err(Error::Resource(
                "ART key exceeds 122880 encoded bytes".into(),
            ));
        }
        entries.push((bytes, id as u64));
    }
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    if entries.windows(2).any(|w| w[0].0 == w[1].0) {
        return Err(Error::Constraint("duplicate ART key".into()));
    }
    let mut tree = Tree {
        allocators: STRIDES.map(Allocator::new),
    };
    let root = tree.build(&entries)?;
    output.field(100);
    output.string(&format!(
        "{}_{}_{}",
        if key.primary { "PRIMARY" } else { "UNIQUE" },
        table.name.name,
        ordinal
    ))?;
    output.property(101, root);
    output.property(102, tree.allocators.len() as u64);
    for allocator in &tree.allocators {
        allocator.serialize(arena, output)?;
    }
    output.end();
    Ok(())
}

fn encode_value(value: &Value, data_type: &DataType, output: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Extension(_) => {
            return Err(Error::Unsupported(
                "native extension ART key encoding".into(),
            ));
        }
        Value::Boolean(v) => output.push(u8::from(*v)),
        Value::Date(v) => output.extend((v.days() as u32 ^ (1 << 31)).to_be_bytes()),
        Value::Integer(v) => {
            let width = match data_type {
                DataType::TinyInt => 1,
                DataType::SmallInt => 2,
                DataType::Integer => 4,
                DataType::BigInt => 8,
                DataType::HugeInt => 16,
                _ => return Err(Error::Internal("ART integer type".into())),
            };
            let mut bytes = v.to_be_bytes();
            bytes[16 - width] ^= 128;
            output.extend(&bytes[16 - width..]);
        }
        Value::Float(v) => {
            let bits = if *v == 0.0 {
                1 << 31
            } else if v.is_nan() {
                u32::MAX
            } else if *v == f32::INFINITY {
                u32::MAX - 1
            } else if *v == f32::NEG_INFINITY {
                0
            } else if *v > 0.0 {
                v.to_bits() ^ (1 << 31)
            } else {
                !v.to_bits()
            };
            output.extend(bits.to_be_bytes());
        }
        Value::Double(v) => {
            let bits = if *v == 0.0 {
                1 << 63
            } else if v.is_nan() {
                u64::MAX
            } else if *v == f64::INFINITY {
                u64::MAX - 1
            } else if *v == f64::NEG_INFINITY {
                0
            } else if *v > 0.0 {
                v.to_bits() ^ (1 << 63)
            } else {
                !v.to_bits()
            };
            output.extend(bits.to_be_bytes());
        }
        Value::Varchar(v) => {
            for byte in v.bytes() {
                if byte <= 1 {
                    output.push(1);
                }
                output.push(byte);
            }
            output.push(0);
        }
        Value::Null => return Err(Error::Internal("NULL ART key".into())),
    }
    Ok(())
}
