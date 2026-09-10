//! Contiguous integer aggregate states, selected by the function adapter.
use super::*;
use crate::{
    common::vector::DataChunk,
    function::grouped::{GroupSelection, GroupedAggregateState},
    parallel::QueryContext,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn create(name: &str, arguments: &[DataType]) -> Option<Box<dyn GroupedAggregateState>> {
    let sum = name == "sum";
    if usize::BITS > 64
        || (name != "count" && !(sum && arguments[0].integer_bits().is_some_and(|bits| bits <= 64)))
    {
        return None;
    }
    Some(Box::new(IntegerGroups {
        sum,
        arguments: arguments.to_vec(),
        values: Vec::new(),
        counts: Vec::new(),
    }))
}

struct IntegerGroups {
    sum: bool,
    arguments: Vec<DataType>,
    values: Vec<i128>,
    counts: Vec<usize>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl GroupedAggregateState for IntegerGroups {
    fn group_count(&self) -> usize {
        self.counts.len()
    }
    fn resize(&mut self, groups: usize, query: &QueryContext) -> Result<()> {
        query.check_rows(groups)?;
        if groups < self.group_count() {
            return Err(Error::Internal("cannot shrink aggregate groups".into()));
        }
        self.values.resize(groups, 0);
        self.counts.resize(groups, 0);
        Ok(())
    }
    fn update_batch(
        &mut self,
        groups: &GroupSelection<'_>,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<()> {
        groups.validate(arguments, self.group_count())?;
        if !arguments
            .columns()
            .iter()
            .map(|c| c.data_type())
            .eq(&self.arguments)
        {
            return Err(Error::Internal(
                "grouped aggregate argument types differ".into(),
            ));
        }
        query.check()?;
        let column = arguments.columns().first();
        if let Some(group) = groups.constant_group() {
            let count = column.map_or(Ok(arguments.len()), |c| nonnull_count(c, query))?;
            let previous_count = self.counts[group];
            self.increment_by(group, count)?;
            if self.sum {
                // Reuse the ordinary single-group column kernel, including
                // its narrow partial sums and checked wide fallback.
                let mut state = State {
                    name: "sum",
                    data_type: DataType::HugeInt,
                    count: 0,
                    value: if previous_count == 0 {
                        Value::Null
                    } else {
                        Value::Integer(self.values[group])
                    },
                    seen: false,
                };
                state.update_batch(arguments, query)?;
                if let Value::Integer(value) = state.value {
                    self.values[group] = value;
                }
            }
            return query.check();
        }
        if column.is_some_and(|c| c.constant_value().is_some_and(Value::is_null)) {
            return query.check();
        }
        let counted = if column.is_none_or(|c| c.all_valid())
            && let Some(counts) = groups.counts()
        {
            for (group, &count) in counts.iter().enumerate() {
                if group % 1024 == 0 {
                    query.check()?;
                }
                self.increment_by(group, count)?;
            }
            true
        } else {
            false
        };
        if self.sum {
            let column = column.expect("bound SUM argument");
            if counted && let Some(Value::Integer(value)) = column.constant_value() {
                for (group, &count) in groups
                    .counts()
                    .expect("counted group destinations")
                    .iter()
                    .enumerate()
                {
                    if group % 1024 == 0 {
                        query.check()?;
                    }
                    self.values[group] += value * count as i128;
                }
                return query.check();
            }
            // Read Value storage sequentially. Counting non-NULL updates once
            // per group proves wide sums safe without permuting the values.
            if let Some(values) = column.flat_values() {
                self.update_sum(groups, values.iter(), counted, query)?;
            } else {
                self.update_sum(groups, column.values(), counted, query)?;
            }
        } else if !counted {
            if let Some(column) = column {
                for (index, (&group, value)) in
                    groups.indices().iter().zip(column.values()).enumerate()
                {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    if !value.is_null() {
                        self.increment_by(group, 1)?;
                    }
                }
            } else {
                for (index, &group) in groups.indices().iter().enumerate() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    self.increment_by(group, 1)?;
                }
            }
        }
        query.check()
    }
    fn finish(self: Box<Self>, query: &QueryContext) -> Result<Vec<Value>> {
        query.check()?;
        self.counts
            .iter()
            .zip(&self.values)
            .map(|(&count, &value)| {
                query.check()?;
                Ok(if !self.sum {
                    Value::Integer(count as i128)
                } else if count == 0 {
                    Value::Null
                } else {
                    Value::Integer(value)
                })
            })
            .collect()
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl IntegerGroups {
    fn update_sum<'a>(
        &mut self,
        groups: &GroupSelection<'_>,
        values: impl Iterator<Item = &'a Value>,
        counted: bool,
        query: &QueryContext,
    ) -> Result<()> {
        for (index, (&group, value)) in groups.indices().iter().zip(values).enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            if let Value::Integer(value) = value {
                if !counted {
                    self.increment_by(group, 1)?;
                }
                // At most usize::MAX non-NULL values, each signed <=64 bits,
                // fit in i128 for every input prefix on <=64-bit hosts.
                self.values[group] += value;
            }
        }
        Ok(())
    }
    fn increment_by(&mut self, group: usize, count: usize) -> Result<()> {
        self.counts[group] = self.counts[group]
            .checked_add(count)
            .ok_or_else(|| Error::Resource("aggregate update count exceeds usize".into()))?;
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn nonnull_count(column: &crate::common::vector::Vector, query: &QueryContext) -> Result<usize> {
    if column.all_valid() {
        return Ok(column.len());
    }
    if column.constant_value().is_some_and(Value::is_null) {
        return Ok(0);
    }
    let mut count = 0;
    for (index, value) in column.values().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        count += usize::from(!value.is_null());
    }
    query.check()?;
    Ok(count)
}
