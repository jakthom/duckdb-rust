//! Contiguous exact aggregate states, selected by the function adapter.
use super::*;
use crate::{
    common::vector::DataChunk,
    function::grouped::{GroupSelection, GroupedAggregateState},
    parallel::QueryContext,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn create(name: &str, arguments: &[DataType]) -> Option<Box<dyn GroupedAggregateState>> {
    if name == "product" && arguments == [DataType::Double] {
        return Some(Box::new(ProductGroups::default()));
    }
    let sum = name == "sum";
    let kernel = arguments
        .first()
        .and_then(SumKernel::bind)
        .filter(|kernel| kernel.supports_count(usize::MAX));
    if usize::BITS > 64 || (name != "count" && !(sum && kernel.is_some())) {
        return None;
    }
    Some(Box::new(IntegerGroups {
        sum,
        kernel,
        arguments: arguments.to_vec(),
        values: Vec::new(),
        counts: Vec::new(),
    }))
}

#[derive(Default)]
struct ProductGroups {
    values: Vec<f64>,
    counts: Vec<usize>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl GroupedAggregateState for ProductGroups {
    fn group_count(&self) -> usize {
        self.counts.len()
    }
    fn resize(&mut self, groups: usize, query: &QueryContext) -> Result<()> {
        query.check_rows(groups)?;
        if groups < self.group_count() {
            return Err(Error::Internal("cannot shrink aggregate groups".into()));
        }
        self.values.resize(groups, 1.0);
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
        let [column] = arguments.columns() else {
            return Err(Error::Internal("product requires one argument".into()));
        };
        if column.data_type() != &DataType::Double {
            return Err(Error::Internal(
                "grouped product argument differs from binding".into(),
            ));
        }
        if self.update_sign_dictionary(groups, column, query)? {
            return Ok(());
        }
        if let Some(values) = column.flat_values() {
            self.update_values(groups, values.iter().cloned(), query)
        } else {
            self.update_values(groups, column.values(), query)
        }
    }
    fn finish(self: Box<Self>, query: &QueryContext) -> Result<Vec<Value>> {
        query.check()?;
        self.counts
            .into_iter()
            .zip(self.values)
            .enumerate()
            .map(|(index, (count, value))| {
                if index % 1024 == 0 {
                    query.check()?;
                }
                Ok(if count == 0 {
                    Value::Null
                } else {
                    Value::Double(value)
                })
            })
            .collect()
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ProductGroups {
    fn update_sign_dictionary(
        &mut self,
        groups: &GroupSelection<'_>,
        column: &crate::common::vector::Vector,
        query: &QueryContext,
    ) -> Result<bool> {
        if !column.all_valid()
            || self
                .values
                .iter()
                .any(|value| *value != 1.0 && *value != -1.0)
        {
            return Ok(false);
        }
        let Some(counts) = groups.counts() else {
            return Ok(false);
        };
        let Some((parent, selection)) = column.dictionary() else {
            return Ok(false);
        };
        let Some(signs) = parent
            .values()
            .map(|value| match value {
                Value::Double(value) if value == 1.0 => Some(false),
                Value::Double(value) if value == -1.0 => Some(true),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
        else {
            return Ok(false);
        };
        let mut negative = vec![false; self.group_count()];
        for (index, (&group, &source)) in groups.indices().iter().zip(selection).enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            if signs[source] {
                negative[group] = !negative[group];
            }
        }
        for (group, (&count, negative)) in counts.iter().zip(negative).enumerate() {
            self.counts[group] = self.counts[group]
                .checked_add(count)
                .ok_or_else(|| Error::Resource("aggregate update count exceeds usize".into()))?;
            if negative {
                self.values[group] = -self.values[group];
            }
        }
        query.check()?;
        Ok(true)
    }

    fn update_values(
        &mut self,
        groups: &GroupSelection<'_>,
        values: impl Iterator<Item = Value>,
        query: &QueryContext,
    ) -> Result<()> {
        for (index, (&group, value)) in groups.indices().iter().zip(values).enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            match value {
                Value::Double(value) => {
                    self.values[group] *= value;
                    self.counts[group] = self.counts[group].checked_add(1).ok_or_else(|| {
                        Error::Resource("aggregate update count exceeds usize".into())
                    })?;
                }
                Value::Null => {}
                _ => {
                    return Err(Error::Internal(
                        "grouped product argument differs from binding".into(),
                    ));
                }
            }
        }
        query.check()
    }
}

struct IntegerGroups {
    sum: bool,
    kernel: Option<SumKernel>,
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
                let kernel = self.kernel.expect("bound SUM kernel");
                // Reuse the ordinary single-group column kernel, including
                // its narrow partial sums and checked wide fallback.
                let mut state = State {
                    name: "sum",
                    data_type: kernel.result_type(),
                    count: 0,
                    value: if previous_count == 0 {
                        Value::Null
                    } else {
                        kernel.value(self.values[group])
                    },
                    seen: false,
                };
                state.update_batch(arguments, query)?;
                if let Value::Integer(value) | Value::Decimal { value, .. } = state.value {
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
            let kernel = self.kernel.expect("bound SUM kernel");
            let column = column.expect("bound SUM argument");
            if counted
                && let Some(value) = column
                    .constant_value()
                    .and_then(|value| kernel.coefficient(value))
            {
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
                self.update_sum(groups, values.iter().cloned(), counted, query)?;
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
                    self.kernel.expect("bound SUM kernel").value(value)
                })
            })
            .collect()
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl IntegerGroups {
    fn update_sum(
        &mut self,
        groups: &GroupSelection<'_>,
        values: impl Iterator<Item = Value>,
        counted: bool,
        query: &QueryContext,
    ) -> Result<()> {
        match self.kernel.expect("bound SUM kernel") {
            SumKernel::Signed(_) => self.update_signed(groups, values, counted, query),
            SumKernel::Unsigned(_) => self.update_coefficients(
                groups,
                values.map(|value| match value {
                    Value::Unsigned(value) => Some(value as i128),
                    Value::Null => None,
                    _ => unreachable!("validated unsigned SUM input"),
                }),
                counted,
                query,
            ),
            SumKernel::Decimal { .. } => self.update_coefficients(
                groups,
                values.map(|value| match value {
                    Value::Decimal { value, .. } => Some(value),
                    Value::Null => None,
                    _ => unreachable!("validated decimal SUM input"),
                }),
                counted,
                query,
            ),
        }
    }
    fn update_signed(
        &mut self,
        groups: &GroupSelection<'_>,
        values: impl Iterator<Item = Value>,
        counted: bool,
        query: &QueryContext,
    ) -> Result<()> {
        // Preserve the direct signed path: lifetime counts already prove each
        // i128 prefix safe, without constructing nullable coefficient tuples.
        for (index, (&group, value)) in groups.indices().iter().zip(values).enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            if let Value::Integer(value) = value {
                if !counted {
                    self.increment_by(group, 1)?;
                }
                self.values[group] += value;
            }
        }
        Ok(())
    }
    fn update_coefficients(
        &mut self,
        groups: &GroupSelection<'_>,
        values: impl Iterator<Item = Option<i128>>,
        counted: bool,
        query: &QueryContext,
    ) -> Result<()> {
        if counted
            && self.values.len() <= groups.indices().len() / 4
            && self
                .kernel
                .expect("bound SUM kernel")
                .maximum_magnitude()
                .checked_mul(groups.indices().len() as i128)
                .is_some_and(|bound| bound <= i64::MAX as i128)
        {
            // The entire batch's absolute bound fits i64, so every group's
            // partial prefix does too. Merge once per group into the wide
            // state; the already checked lifetime counts prove that safe.
            let mut partials = vec![0_i64; self.values.len()];
            for (index, (&group, value)) in groups.indices().iter().zip(values).enumerate() {
                if index % 1024 == 0 {
                    query.check()?;
                }
                partials[group] += value.expect("counted non-NULL SUM input") as i64;
            }
            for (index, (sum, partial)) in self.values.iter_mut().zip(partials).enumerate() {
                if index % 1024 == 0 {
                    query.check()?;
                }
                *sum += i128::from(partial);
            }
            return Ok(());
        }
        for (index, (&group, value)) in groups.indices().iter().zip(values).enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            if let Some(value) = value {
                if !counted {
                    self.increment_by(group, 1)?;
                }
                // Kernel selection proves every prefix of at most usize::MAX
                // inputs fits the result, including DECIMAL(38, scale).
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
