//! Typed SQL set and multiset operations over independently opened streams.
use crate::{
    common::{Error, Result, type_registry::BoundType, vector::DataChunk},
    execution::{
        ExecutionContext,
        physical_plan::PhysicalOperator,
        stream::{self, Stream},
    },
    planner::{Schema, logical::SetOperation},
};
use std::{
    collections::{BTreeMap, HashMap},
    fmt::Debug,
};

pub struct SetPlan<'a> {
    pub left: &'a dyn PhysicalOperator,
    pub right: &'a dyn PhysicalOperator,
    pub kind: SetOperation,
    pub all: bool,
    pub schema: &'a Schema,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// UNION preserves left-then-right demand. INTERSECT/EXCEPT build right-hand
/// multiplicities, then retain qualifying left rows in input order. DISTINCT
/// emits one representative, ALL preserves min/subtracted multiplicities.
/// Equality uses registered type keys, including NULL/NaN/signed-zero rules.
/// Cursors own all state and chunks; cancellation/errors terminate a cursor.
/// Retained distinct keys obey the query row budget. Spill is not implemented.
pub trait SetAlgorithm: Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn open<'a>(&self, plan: SetPlan<'a>, context: &'a ExecutionContext<'a>) -> Result<Stream<'a>>;
}

#[derive(Debug, Default)]
pub struct HashSetOperations;
#[derive(Debug, Default)]
pub struct OrderedSetOperations;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SetAlgorithm for HashSetOperations {
    fn name(&self) -> &'static str {
        "hash-set-operations"
    }
    fn open<'a>(&self, plan: SetPlan<'a>, context: &'a ExecutionContext<'a>) -> Result<Stream<'a>> {
        open::<HashCounts>(plan, context)
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SetAlgorithm for OrderedSetOperations {
    fn name(&self) -> &'static str {
        "ordered-set-operations"
    }
    fn open<'a>(&self, plan: SetPlan<'a>, context: &'a ExecutionContext<'a>) -> Result<Stream<'a>> {
        open::<BTreeMap<Key, usize>>(plan, context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
trait Counts: Default {
    fn entry(&mut self, key: Key) -> &mut usize;
    fn get_mut(&mut self, key: &Key) -> Option<&mut usize>;
    fn len(&self) -> usize;
    fn finish(&mut self) {}
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Key {
    Integer(Option<i128>),
    Bytes(Vec<u8>),
}

#[derive(Default)]
struct HashCounts {
    values: HashMap<Key, usize>,
    dense: Option<(i128, Vec<usize>)>,
    nulls: usize,
    size: usize,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Counts for HashCounts {
    fn entry(&mut self, key: Key) -> &mut usize {
        self.values.entry(key).or_default()
    }
    fn get_mut(&mut self, key: &Key) -> Option<&mut usize> {
        if let Some((minimum, counts)) = &mut self.dense {
            match key {
                Key::Integer(Some(value)) => value
                    .checked_sub(*minimum)
                    .and_then(|v| usize::try_from(v).ok())
                    .and_then(|index| counts.get_mut(index)),
                Key::Integer(None) => Some(&mut self.nulls),
                _ => None,
            }
        } else {
            self.values.get_mut(key)
        }
    }
    fn len(&self) -> usize {
        if self.dense.is_some() {
            self.size
        } else {
            self.values.len()
        }
    }
    fn finish(&mut self) {
        let mut minimum = i128::MAX;
        let mut maximum = i128::MIN;
        for key in self.values.keys() {
            match key {
                Key::Integer(Some(value)) => {
                    minimum = minimum.min(*value);
                    maximum = maximum.max(*value);
                }
                Key::Integer(None) => (),
                Key::Bytes(_) => return,
            }
        }
        let Some(width) = maximum
            .checked_sub(minimum)
            .and_then(|v| v.checked_add(1))
            .and_then(|v| usize::try_from(v).ok())
            .filter(|&width| width <= 16384 && width <= self.values.len().saturating_mul(8))
        else {
            return;
        };
        let mut counts = vec![0; width];
        self.size = self.values.len();
        for (key, count) in self.values.drain() {
            match key {
                Key::Integer(Some(value)) => counts[(value - minimum) as usize] = count,
                Key::Integer(None) => self.nulls = count,
                _ => unreachable!(),
            }
        }
        self.dense = Some((minimum, counts));
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Counts for BTreeMap<Key, usize> {
    fn entry(&mut self, key: Key) -> &mut usize {
        self.entry(key).or_default()
    }
    fn get_mut(&mut self, key: &Key) -> Option<&mut usize> {
        self.get_mut(key)
    }
    fn len(&self) -> usize {
        self.len()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn key(
    batch: &DataChunk,
    index: usize,
    types: &[BoundType],
    context: &ExecutionContext<'_>,
) -> Result<Key> {
    // Checked input streams establish logical validity before this capability is used.
    if let [data_type] = types
        && data_type.key_representation().has_integer_keys()
    {
        return data_type
            .key_representation()
            .integer_key(batch.columns()[0].get(index).expect("valid batch row"))
            .map(Key::Integer);
    }
    let mut key = Vec::new();
    for (column, data_type) in batch.columns().iter().zip(types) {
        data_type.append_key(
            column.get(index).expect("valid batch row"),
            &mut key,
            context.query,
        )?;
    }
    Ok(Key::Bytes(key))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn open<'a, C: Counts + 'a>(
    plan: SetPlan<'a>,
    context: &'a ExecutionContext<'a>,
) -> Result<Stream<'a>> {
    let types = plan
        .schema
        .iter()
        .map(|field| context.query.types().bind(&field.data_type))
        .collect::<Result<Vec<_>>>()?;
    let mut left = None;
    let mut right = Some(plan.right);
    let mut counts = C::default();
    let mut emitted = C::default();
    let mut remaining = 0usize;
    let mut initialized = false;
    Ok(stream::from_fn(move |max_rows| {
        context.query.check()?;
        if !initialized {
            initialized = true;
            left = Some(stream::open(plan.left, context)?);
            if plan.kind != SetOperation::Union {
                let mut build = stream::open(right.take().expect("unopened right input"), context)?;
                while let Some(batch) = build.next(context.query.batch_size())? {
                    for index in 0..batch.len() {
                        if index % 1024 == 0 {
                            context.query.check()?;
                        }
                        let count = counts.entry(key(&batch, index, &types, context)?);
                        if plan.all || *count == 0 {
                            remaining = remaining.checked_add(1).ok_or_else(|| {
                                Error::Resource("set multiplicity overflow".into())
                            })?;
                        }
                        *count = if plan.all {
                            count.checked_add(1).ok_or_else(|| {
                                Error::Resource("set multiplicity overflow".into())
                            })?
                        } else {
                            1
                        };
                        context.query.check_rows(counts.len())?;
                    }
                }
                counts.finish();
            }
        }
        loop {
            let Some(cursor) = &mut left else {
                return Ok(None);
            };
            let Some(batch) = cursor.next(max_rows)? else {
                left = right
                    .take()
                    .map(|right| stream::open(right, context))
                    .transpose()?;
                continue;
            };
            if plan.kind == SetOperation::Union && plan.all {
                return Ok(Some(batch));
            }
            // Once every build occurrence was emitted, intersection cannot
            // retain another row. Still drain the checked input so its errors
            // and effects remain observable.
            if plan.kind == SetOperation::Intersect && remaining == 0 {
                continue;
            }
            let mut selected = Vec::new();
            for index in 0..batch.len() {
                if index % 1024 == 0 {
                    context.query.check()?;
                }
                let key = key(&batch, index, &types, context)?;
                let matched = counts.get_mut(&key).is_some_and(|count| {
                    let matched = *count > 0;
                    if matched && (plan.all || plan.kind == SetOperation::Intersect) {
                        *count -= 1;
                        remaining -= 1;
                    }
                    matched
                });
                let keep = match plan.kind {
                    SetOperation::Union => true,
                    SetOperation::Intersect => matched,
                    SetOperation::Except => !matched,
                };
                if keep {
                    let first = if plan.all || plan.kind == SetOperation::Intersect {
                        true
                    } else {
                        let count = emitted.entry(key);
                        let first = *count == 0;
                        *count = 1;
                        context
                            .query
                            .check_rows(counts.len().saturating_add(emitted.len()))?;
                        first
                    };
                    if first {
                        selected.push(index);
                    }
                }
            }
            if !selected.is_empty() {
                return batch.select(&selected).map(Some);
            }
        }
    }))
}
