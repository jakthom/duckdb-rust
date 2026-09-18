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

struct StringAggState {
    buffer: StringBuffer,
    separator: Option<String>,
}

const GROUPED_INLINE_BYTES: usize = 32;

#[derive(Default)]
enum GroupedStringBuffer {
    #[default]
    Unseen,
    Inline {
        len: u8,
        bytes: [u8; GROUPED_INLINE_BYTES],
    },
    Heap(String),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl GroupedStringBuffer {
    fn append(&mut self, input: &Value, separator: &str) -> Result<()> {
        let Some(input) = varchar_input(input)? else {
            return Ok(());
        };
        self.append_varchar(input, separator)
    }

    fn append_varchar(&mut self, input: &str, separator: &str) -> Result<()> {
        match self {
            Self::Unseen if input.len() <= GROUPED_INLINE_BYTES => {
                let mut bytes = [0; GROUPED_INLINE_BYTES];
                bytes[..input.len()].copy_from_slice(input.as_bytes());
                *self = Self::Inline {
                    len: input.len() as u8,
                    bytes,
                };
            }
            Self::Unseen => {
                let mut value = grouped_string(grouped_heap_capacity(input.len()))?;
                value.push_str(input);
                *self = Self::Heap(value);
            }
            Self::Inline { len, bytes } => {
                let current = usize::from(*len);
                let additional = additional_bytes(current, separator.len(), input.len())?;
                let required = current
                    .checked_add(additional)
                    .ok_or_else(|| Error::Resource("string_agg output size overflow".into()))?;
                if required <= GROUPED_INLINE_BYTES {
                    let separator_end = current + separator.len();
                    bytes[current..separator_end].copy_from_slice(separator.as_bytes());
                    bytes[separator_end..required].copy_from_slice(input.as_bytes());
                    *len = required as u8;
                } else {
                    let inline = std::str::from_utf8(&bytes[..current]).map_err(|_| {
                        Error::Internal("string_agg inline UTF-8 is invalid".into())
                    })?;
                    let mut value = grouped_string(grouped_heap_capacity(required))?;
                    value.push_str(inline);
                    value.push_str(separator);
                    value.push_str(input);
                    *self = Self::Heap(value);
                }
            }
            Self::Heap(value) => {
                let additional = additional_bytes(value.len(), separator.len(), input.len())?;
                let required = value
                    .len()
                    .checked_add(additional)
                    .ok_or_else(|| Error::Resource("string_agg output size overflow".into()))?;
                let target = grouped_growth_capacity(required, value.capacity());
                if target > value.capacity() {
                    value
                        .try_reserve_exact(target - value.len())
                        .map_err(|_| Error::Resource("string_agg allocation failed".into()))?;
                }
                value.push_str(separator);
                value.push_str(input);
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Value> {
        match self {
            Self::Unseen => Ok(Value::Null),
            Self::Inline { len, bytes } => {
                let text = std::str::from_utf8(&bytes[..usize::from(len)])
                    .map_err(|_| Error::Internal("string_agg inline UTF-8 is invalid".into()))?;
                let mut value = grouped_string(text.len())?;
                value.push_str(text);
                Ok(Value::Varchar(value))
            }
            Self::Heap(value) => Ok(Value::Varchar(value)),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn grouped_string(capacity: usize) -> Result<String> {
    let mut value = String::new();
    value
        .try_reserve_exact(capacity)
        .map_err(|_| Error::Resource("string_agg allocation failed".into()))?;
    Ok(value)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn grouped_heap_capacity(required: usize) -> usize {
    required.checked_next_power_of_two().unwrap_or(required)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn grouped_growth_capacity(required: usize, capacity: usize) -> usize {
    if required <= capacity {
        return capacity;
    }
    grouped_heap_capacity(required).max(required).max(capacity)
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
    buffers: Vec<GroupedStringBuffer>,
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
        self.buffers
            .resize_with(groups, GroupedStringBuffer::default);
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
            visit_grouped_column(&mut self.buffers, groups, column, separator, query)?;
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
            values.push(buffer.finish()?);
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn varchar_input(value: &Value) -> Result<Option<&str>> {
    match value {
        Value::Varchar(value) => Ok(Some(value)),
        Value::Null => Ok(None),
        _ => Err(Error::Internal(
            "string_agg input differs from binding".into(),
        )),
    }
}

/// Zip already validated destinations with borrowed physical VARCHAR values.
/// The generic visitor remains the authority for encodings without a direct
/// flat parent; this path changes delivery only, not buffer growth or results.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn visit_grouped_column(
    buffers: &mut [GroupedStringBuffer],
    groups: &GroupSelection<'_>,
    column: &Vector,
    separator: &str,
    query: &QueryContext,
) -> Result<()> {
    let destinations = groups.indices();
    if let Some(values) = column.flat_values() {
        if values.len() != destinations.len() {
            return Err(Error::Internal("string_agg flat column cardinality".into()));
        }
        for (destination_chunk, value_chunk) in destinations.chunks(1024).zip(values.chunks(1024)) {
            query.check()?;
            for (&destination, value) in destination_chunk.iter().zip(value_chunk) {
                if let Some(value) = varchar_input(value)? {
                    buffers[destination].append_varchar(value, separator)?;
                }
            }
        }
    } else if let Some(value) = column.constant_value() {
        let value = varchar_input(value)?;
        for destination_chunk in destinations.chunks(1024) {
            query.check()?;
            if let Some(value) = value {
                for &destination in destination_chunk {
                    buffers[destination].append_varchar(value, separator)?;
                }
            }
        }
    } else if let Some((parent, indices)) = column.dictionary()
        && let Some(values) = parent.flat_values()
    {
        if indices.len() != destinations.len() {
            return Err(Error::Internal("string_agg dictionary cardinality".into()));
        }
        for (destination_chunk, index_chunk) in destinations.chunks(1024).zip(indices.chunks(1024))
        {
            query.check()?;
            for (&destination, &index) in destination_chunk.iter().zip(index_chunk) {
                let value = values.get(index).ok_or_else(|| {
                    Error::Internal("string_agg dictionary index outside parent".into())
                })?;
                if let Some(value) = varchar_input(value)? {
                    buffers[destination].append_varchar(value, separator)?;
                }
            }
        }
    } else {
        visit_column(column, query, |row, value| {
            buffers[destinations[row]].append(value, separator)
        })?;
    }
    query.check()
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
