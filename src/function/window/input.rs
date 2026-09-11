use crate::common::{DataType, Error, Result, RowCollection, Value};
use std::ops::{Index, Range};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NullTreatment {
    Respect,
    Ignore,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WindowOptions {
    pub distinct: bool,
    pub null_treatment: Option<NullTreatment>,
    pub filtered: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl WindowOptions {
    pub fn ignores_nulls(self) -> bool {
        self.null_treatment == Some(NullTreatment::Ignore)
    }
}

/// A checked selection of borrowed, evaluated argument rows. Reordering and
/// duplicates never copy payloads. The source and selection outlive this view.
pub struct WindowRows<'a> {
    rows: &'a RowCollection,
    indices: &'a [usize],
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> WindowRows<'a> {
    pub fn new(rows: &'a RowCollection, indices: &'a [usize]) -> Result<Self> {
        if indices.iter().any(|&index| index >= rows.len()) {
            return Err(Error::Internal("window argument row outside input".into()));
        }
        Ok(Self { rows, indices })
    }
    pub fn len(&self) -> usize {
        self.indices.len()
    }
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
    pub fn get(&self, index: usize) -> Option<&[Value]> {
        self.indices.get(index).map(|&index| &self.rows[index])
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &[Value]> {
        self.indices.iter().map(|&index| &self.rows[index])
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Index<usize> for WindowRows<'_> {
    type Output = [Value];
    fn index(&self, index: usize) -> &Self::Output {
        &self.rows[self.indices[index]]
    }
}

/// Half-open frame or peer bounds, in partition order. Uniform bounds retain
/// one range; varying bounds retain one range per row. Constructors reject
/// reversed and out-of-partition bounds, including for an empty partition.
pub struct WindowBounds {
    ranges: Ranges,
    count: usize,
}
enum Ranges {
    Uniform(Range<usize>),
    Rows(Vec<Range<usize>>),
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl WindowBounds {
    pub fn uniform(range: Range<usize>, count: usize) -> Result<Self> {
        validate(&range, count)?;
        Ok(Self {
            ranges: Ranges::Uniform(range),
            count,
        })
    }
    pub fn rows(ranges: Vec<Range<usize>>) -> Result<Self> {
        let count = ranges.len();
        for range in &ranges {
            validate(range, count)?;
        }
        Ok(Self {
            ranges: Ranges::Rows(ranges),
            count,
        })
    }
    pub fn len(&self) -> usize {
        self.count
    }
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Range<usize>> {
        (0..self.count).map(|index| &self[index])
    }
    pub fn uniform_range(&self) -> Option<&Range<usize>> {
        match &self.ranges {
            Ranges::Uniform(range) => Some(range),
            Ranges::Rows(_) => None,
        }
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Index<usize> for WindowBounds {
    type Output = Range<usize>;
    fn index(&self, index: usize) -> &Self::Output {
        assert!(index < self.count, "window bounds row outside partition");
        match &self.ranges {
            Ranges::Uniform(range) => range,
            Ranges::Rows(ranges) => &ranges[index],
        }
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate(range: &Range<usize>, count: usize) -> Result<()> {
    if range.start > range.end || range.end > count {
        Err(Error::Internal("window bounds outside partition".into()))
    } else {
        Ok(())
    }
}

#[cfg(kani)]
mod verification {
    use super::*;

    #[kani::proof]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn kani_uniform_window_bounds_validate_and_retain_range() {
        let start: usize = kani::any();
        let end: usize = kani::any();
        let count: usize = kani::any();
        let bounds = WindowBounds::uniform(start..end, count);
        assert_eq!(bounds.is_ok(), start <= end && end <= count);
        if let Ok(bounds) = bounds {
            assert_eq!(bounds.len(), count);
            assert_eq!(bounds.is_empty(), count == 0);
            assert_eq!(bounds.uniform_range(), Some(&(start..end)));
            let row: usize = kani::any();
            if row < count {
                assert_eq!(bounds[row], start..end);
            }
        }
    }
}

/// Rows and bounds have equal cardinality. Arguments have already been
/// evaluated once per input row and validated against argument_types. Peers
/// contain their row; frames may be empty. Functions borrow this input only
/// for evaluation and return owned results.
pub struct WindowInput<'a> {
    pub arguments: WindowRows<'a>,
    pub argument_types: &'a [DataType],
    pub frames: &'a WindowBounds,
    pub peers: &'a WindowBounds,
    pub filter: &'a [bool],
    pub options: WindowOptions,
}
