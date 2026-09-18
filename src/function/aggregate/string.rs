use crate::{
    common::{
        DataType, Error, Result, Value,
        vector::{DataChunk, Vector},
    },
    function::{
        AggregateBinding, AggregateFunction, AggregateModifierStrategy, AggregateState,
        grouped::{GroupSelection, GroupedAggregateState},
    },
    parallel::QueryContext,
};

/// STRING_AGG captures its constant separator in a bound adapter and retains
/// only the input row. This preserves generic ORDER BY/DISTINCT/FILTER and
/// window paths without a function-specific executor wrapper.
#[derive(Debug)]
pub(super) struct StringAgg(pub(super) &'static str, pub(super) Option<Option<String>>);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateFunction for StringAgg {
    fn name(&self) -> &str {
        self.0
    }

    fn argument_types(&self, arguments: &[DataType]) -> Result<Vec<DataType>> {
        match arguments {
            [DataType::Varchar] | [DataType::Null] => Ok(vec![DataType::Varchar]),
            [DataType::Varchar, DataType::Varchar]
            | [DataType::Varchar, DataType::Null]
            | [DataType::Null, DataType::Varchar]
            | [DataType::Null, DataType::Null] => Ok(vec![DataType::Varchar; 2]),
            _ => Err(Error::Bind(format!(
                "No function matches {}({arguments:?})",
                self.0
            ))),
        }
    }

    fn constant_arguments(&self, arity: usize) -> &[usize] {
        if arity == 2 { &[1] } else { &[] }
    }
    fn constant_argument_label(&self, index: usize) -> Option<&str> {
        (index == 1).then_some("Separator")
    }

    fn bind(&self, constants: &[Option<Value>]) -> Result<Option<AggregateBinding>> {
        let separator = match constants.len() {
            1 => Some(",".to_owned()),
            2 => match constants.get(1).and_then(Option::as_ref) {
                Some(Value::Varchar(separator)) => Some(separator.clone()),
                Some(Value::Null) => None,
                _ => return Err(Error::Internal("string_agg separator binding".into())),
            },
            _ => return Err(Error::Internal("string_agg argument binding".into())),
        };
        // Development retains the separator expression in its bound tree but
        // rewrites a NULL separator's leading input to typed NULL. The Rust
        // aggregate executor has no hidden constant-parameter lane, so the
        // adapter captures the constant and removes its inert child while
        // preserving the observable no-evaluation rule for the leading input.
        Ok(Some(AggregateBinding {
            function: std::sync::Arc::new(Self(self.0, Some(separator.clone()))),
            retain_arguments: vec![0],
            replacements: separator
                .is_none()
                .then_some((0, Value::Null))
                .into_iter()
                .collect(),
        }))
    }

    fn return_type(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        let _ = types;
        self.argument_types(arguments)?;
        Ok(DataType::Varchar)
    }

    fn create_state(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Box<dyn AggregateState>> {
        self.return_type(arguments, types)?;
        Ok(Box::new(StringAggState {
            separator: self.1.clone().unwrap_or_else(|| Some(",".to_owned())),
            buffer: StringBuffer::default(),
        }))
    }

    fn batch_update_is_total(&self, arguments: &[DataType]) -> bool {
        arguments == [DataType::Varchar]
    }

    fn modifier_strategy(&self, arguments: &[DataType]) -> AggregateModifierStrategy {
        if self.1.is_some() && arguments == [DataType::Varchar] {
            AggregateModifierStrategy::BufferedTotal
        } else {
            AggregateModifierStrategy::Generic
        }
    }

    fn create_grouped_state(
        &self,
        arguments: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Option<Box<dyn GroupedAggregateState>>> {
        if arguments != [DataType::Varchar] {
            return Ok(None);
        }
        Ok(Some(Box::new(StringAggGroups {
            separator: self.1.clone().unwrap_or_else(|| Some(",".to_owned())),
            buffers: Vec::new(),
            batch_bytes: Vec::new(),
            batch_values: Vec::new(),
            touched: Vec::new(),
        })))
    }
}

#[derive(Default)]
struct StringBuffer {
    value: String,
    seen: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl StringBuffer {
    fn append(&mut self, input: &Value, separator: &str) -> Result<()> {
        let Value::Varchar(input) = input else {
            if input.is_null() {
                return Ok(());
            }
            return Err(Error::Internal(
                "string_agg input differs from binding".into(),
            ));
        };
        let additional = additional_bytes(
            self.value.len(),
            if self.seen { separator.len() } else { 0 },
            input.len(),
        )?;
        self.value
            .try_reserve(additional)
            .map_err(|_| Error::Resource("string_agg allocation failed".into()))?;
        if self.seen {
            self.value.push_str(separator);
        }
        self.value.push_str(input);
        self.seen = true;
        Ok(())
    }

    fn finish(self) -> Value {
        if self.seen {
            Value::Varchar(self.value)
        } else {
            Value::Null
        }
    }

    fn reserve_grouped(&mut self, additional: usize) -> Result<()> {
        let target = grouped_reserve_target(
            self.value.len(),
            self.value.capacity(),
            additional,
            self.seen,
        )?;
        if target > self.value.capacity() {
            self.value
                .try_reserve_exact(target - self.value.len())
                .map_err(|_| Error::Resource("string_agg allocation failed".into()))?;
        }
        Ok(())
    }

    fn append_reserved(&mut self, input: &Value, separator: &str) -> Result<()> {
        let Value::Varchar(input) = input else {
            if input.is_null() {
                return Ok(());
            }
            return Err(Error::Internal(
                "string_agg input differs from binding".into(),
            ));
        };
        let additional = additional_bytes(
            self.value.len(),
            if self.seen { separator.len() } else { 0 },
            input.len(),
        )?;
        debug_assert!(self.value.capacity() - self.value.len() >= additional);
        if self.seen {
            self.value.push_str(separator);
        }
        self.value.push_str(input);
        self.seen = true;
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn additional_bytes(current: usize, separator: usize, input: usize) -> Result<usize> {
    let additional = separator
        .checked_add(input)
        .ok_or_else(|| Error::Resource("string_agg output size overflow".into()))?;
    current
        .checked_add(additional)
        .ok_or_else(|| Error::Resource("string_agg output size overflow".into()))?;
    Ok(additional)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn accumulate_grouped_bytes(current: usize, separator: usize, input: usize) -> Result<usize> {
    let additional = additional_bytes(current, separator, input)?;
    current
        .checked_add(additional)
        .ok_or_else(|| Error::Resource("string_agg output size overflow".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn grouped_reserve_target(
    current: usize,
    capacity: usize,
    additional: usize,
    reused: bool,
) -> Result<usize> {
    let required = current
        .checked_add(additional)
        .ok_or_else(|| Error::Resource("string_agg output size overflow".into()))?;
    if required <= capacity {
        return Ok(capacity);
    }
    Ok(if reused {
        required.checked_mul(2).unwrap_or(required)
    } else {
        required
    })
}

struct StringAggState {
    buffer: StringBuffer,
    separator: Option<String>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateState for StringAggState {
    fn update(&mut self, arguments: &[Value], query: &QueryContext) -> Result<()> {
        query.check()?;
        let [input] = arguments else {
            return Err(Error::Internal(
                "string_agg arguments differ from binding".into(),
            ));
        };
        if let Some(separator) = &self.separator {
            self.buffer.append(input, separator)?;
        }
        Ok(())
    }

    fn update_batch(&mut self, arguments: &DataChunk, query: &QueryContext) -> Result<()> {
        let [column] = arguments.columns() else {
            return Err(Error::Internal(
                "string_agg arguments differ from binding".into(),
            ));
        };
        self.update_column(column, query)
    }

    fn update_column(&mut self, column: &Vector, query: &QueryContext) -> Result<()> {
        validate_column(column, query)?;
        if let Some(separator) = &self.separator {
            visit_column(column, query, |_, value| {
                self.buffer.append(value, separator)
            })?;
        }
        query.check()
    }

    fn finish(self: Box<Self>) -> Result<Value> {
        Ok(self.buffer.finish())
    }
}

struct StringAggGroups {
    separator: Option<String>,
    buffers: Vec<StringBuffer>,
    batch_bytes: Vec<usize>,
    batch_values: Vec<usize>,
    touched: Vec<usize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl GroupedAggregateState for StringAggGroups {
    fn group_count(&self) -> usize {
        self.buffers.len()
    }

    fn resize(&mut self, groups: usize, query: &QueryContext) -> Result<()> {
        query.check_rows(groups)?;
        let additional = groups
            .checked_sub(self.buffers.len())
            .ok_or_else(|| Error::Internal("cannot shrink aggregate groups".into()))?;
        self.buffers
            .try_reserve(additional)
            .map_err(|_| Error::Resource("string_agg group allocation failed".into()))?;
        self.batch_bytes
            .try_reserve(additional)
            .map_err(|_| Error::Resource("string_agg group allocation failed".into()))?;
        self.batch_values
            .try_reserve(additional)
            .map_err(|_| Error::Resource("string_agg group allocation failed".into()))?;
        self.touched
            .try_reserve(groups.saturating_sub(self.touched.len()))
            .map_err(|_| Error::Resource("string_agg group allocation failed".into()))?;
        self.buffers.resize_with(groups, StringBuffer::default);
        self.batch_bytes.resize(groups, 0);
        self.batch_values.resize(groups, 0);
        query.check()
    }

    fn update_batch(
        &mut self,
        groups: &GroupSelection<'_>,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<()> {
        groups.validate(arguments, self.group_count())?;
        let [column] = arguments.columns() else {
            return Err(Error::Internal(
                "string_agg arguments differ from binding".into(),
            ));
        };
        validate_column(column, query)?;
        if let Some(separator) = &self.separator {
            let indices = groups.indices();
            let buffers = &mut self.buffers;
            let batch_bytes = &mut self.batch_bytes;
            let batch_values = &mut self.batch_values;
            let touched = &mut self.touched;

            visit_column(column, query, |row, value| {
                let Value::Varchar(input) = value else {
                    if value.is_null() {
                        return Ok(());
                    }
                    return Err(Error::Internal(
                        "string_agg input differs from binding".into(),
                    ));
                };
                let group = indices[row];
                if batch_values[group] == 0 {
                    touched.push(group);
                }
                let separator_bytes = if buffers[group].seen || batch_values[group] != 0 {
                    separator.len()
                } else {
                    0
                };
                batch_bytes[group] =
                    accumulate_grouped_bytes(batch_bytes[group], separator_bytes, input.len())?;
                batch_values[group] = batch_values[group]
                    .checked_add(1)
                    .ok_or_else(|| Error::Resource("string_agg value count overflow".into()))?;
                Ok(())
            })?;

            for (position, &group) in touched.iter().enumerate() {
                if position % 1024 == 0 {
                    query.check()?;
                }
                buffers[group].reserve_grouped(batch_bytes[group])?;
            }
            query.check()?;

            visit_column(column, query, |row, value| {
                buffers[indices[row]].append_reserved(value, separator)
            })?;

            for &group in touched.iter() {
                batch_bytes[group] = 0;
                batch_values[group] = 0;
            }
            touched.clear();
        }
        query.check()
    }

    fn finish(self: Box<Self>, query: &QueryContext) -> Result<Vec<Value>> {
        query.check()?;
        let mut values = Vec::new();
        values
            .try_reserve(self.buffers.len())
            .map_err(|_| Error::Resource("string_agg result allocation failed".into()))?;
        for (index, buffer) in self.buffers.into_iter().enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            values.push(buffer.finish());
        }
        query.check()?;
        Ok(values)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate_column(column: &Vector, query: &QueryContext) -> Result<()> {
    query.check()?;
    if column.data_type() != &DataType::Varchar {
        return Err(Error::Internal(
            "string_agg column differs from binding".into(),
        ));
    }
    Ok(())
}

/// Borrow validated string values whenever their physical encoding permits it.
/// Offsets and dictionary selections come from the vector's checked views;
/// other encodings retain the ordinary owned-value fallback.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn visit_column(
    column: &Vector,
    query: &QueryContext,
    mut visit: impl FnMut(usize, &Value) -> Result<()>,
) -> Result<()> {
    if let Some(values) = column.flat_values() {
        for (block, values) in values.chunks(1024).enumerate() {
            query.check()?;
            for (offset, value) in values.iter().enumerate() {
                visit(block * 1024 + offset, value)?;
            }
        }
    } else if let Some(value) = column.constant_value() {
        for start in (0..column.len()).step_by(1024) {
            query.check()?;
            for row in start..column.len().min(start.saturating_add(1024)) {
                visit(row, value)?;
            }
        }
    } else if let Some((parent, indices)) = column.dictionary()
        && let Some(values) = parent.flat_values()
    {
        for (block, indices) in indices.chunks(1024).enumerate() {
            query.check()?;
            for (offset, &index) in indices.iter().enumerate() {
                visit(block * 1024 + offset, &values[index])?;
            }
        }
    } else {
        for (row, value) in column.values().enumerate() {
            if row % 1024 == 0 {
                query.check()?;
            }
            visit(row, &value)?;
        }
    }
    query.check()
}

#[cfg(test)]
mod tests;
