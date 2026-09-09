//! Checked views of metadata at the end of native compressed segments.
use super::super::binary::{corrupt, u32_at};
use crate::common::Result;

pub(super) struct ReverseMetadata<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> ReverseMetadata<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self> {
        let position = u32_at(data, 0)? as usize;
        if position < 4 || position > data.len() {
            return Err(corrupt("compression metadata outside segment"));
        }
        Ok(Self { data, position })
    }
    pub fn position(&self) -> usize {
        self.position
    }
    pub fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let start = self
            .position
            .checked_sub(count)
            .filter(|&start| start >= 4)
            .ok_or_else(|| corrupt("truncated compression metadata"))?;
        let bytes = &self.data[start..self.position];
        self.position = start;
        Ok(bytes)
    }
    pub fn offset(&mut self) -> Result<usize> {
        Ok(u32_at(self.take(4)?, 0)? as usize)
    }
    /// Chimp aligns packed metadata at its low address, leaving any padding
    /// between the packed values and the fields above them.
    pub fn aligned(&mut self, count: usize) -> Result<&'a [u8]> {
        let start = self
            .position
            .checked_sub(count)
            .ok_or_else(|| corrupt("compression metadata size overflow"))?;
        let bytes = self.take(count + (start & 1))?;
        Ok(&bytes[..count])
    }
}

pub(super) struct OffsetGroups<'a> {
    data: &'a [u8],
    metadata_start: usize,
    metadata_end: usize,
    header_size: usize,
    count: usize,
}

impl<'a> OffsetGroups<'a> {
    pub fn new(data: &'a [u8], header_size: usize, count: usize) -> Result<Self> {
        let mut metadata = ReverseMetadata::new(data)?;
        let metadata_end = metadata.position();
        metadata.take(
            count
                .checked_mul(4)
                .ok_or_else(|| corrupt("compression group count overflow"))?,
        )?;
        let metadata_start = metadata.position();
        if metadata_start < header_size {
            return Err(corrupt("compression groups overlap header"));
        }
        Ok(Self {
            data,
            metadata_start,
            metadata_end,
            header_size,
            count,
        })
    }
    pub fn group(&self, index: usize) -> Result<&'a [u8]> {
        if index >= self.count {
            return Err(corrupt("compression group outside segment"));
        }
        let start = u32_at(self.data, self.metadata_end - 4 * (index + 1))? as usize;
        let end = if index + 1 == self.count {
            self.metadata_start
        } else {
            u32_at(self.data, self.metadata_end - 4 * (index + 2))? as usize
        };
        if start < self.header_size || end < start || end > self.metadata_start {
            return Err(corrupt("invalid compression group bounds"));
        }
        Ok(&self.data[start..end])
    }
}
