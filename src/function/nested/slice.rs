//! Selected LIST/ARRAY slicing with retained omitted-bound metadata.
use super::*;
use crate::common::type_registry::BoundType;

#[derive(Debug)]
struct SequenceSlice(&'static str);

#[derive(Debug)]
struct BoundSequenceSlice {
    name: &'static str,
    result: BoundType,
    arguments: Vec<DataType>,
    begin_empty: bool,
    end_empty: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in ["list_slice", "array_slice"] {
        registry
            .register_scalar(Arc::new(SequenceSlice(name)))
            .expect("unique sequence slice function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for SequenceSlice {
    fn name(&self) -> &str {
        self.0
    }

    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        if !matches!(arguments.len(), 3 | 4) {
            return Err(Error::Bind(format!(
                "{} requires a value, begin, end and optional step",
                self.0
            )));
        }
        let source = arguments.data_type(0)?;
        let result = match &source {
            DataType::Nested(metadata) => match metadata.as_ref() {
                NestedType::List(_) => source.clone(),
                NestedType::Array { element, .. } => NestedType::List(element.clone()).data_type(),
                _ => {
                    return Err(Error::Bind(format!("{} requires a LIST or ARRAY", self.0)));
                }
            },
            DataType::Null => DataType::Null,
            _ => {
                return Err(Error::Bind(format!("{} requires a LIST or ARRAY", self.0)));
            }
        };
        let begin_empty = omitted_bound(arguments, 1)?;
        let end_empty = omitted_bound(arguments, 2)?;
        let mut required = Vec::with_capacity(arguments.len());
        required.push(result.clone());
        required.push(if begin_empty {
            arguments.data_type(1)?
        } else {
            DataType::BigInt
        });
        required.push(if end_empty {
            arguments.data_type(2)?
        } else {
            DataType::BigInt
        });
        if arguments.len() == 4 {
            required.push(DataType::BigInt);
        }
        Ok(Some(Arc::new(BoundSequenceSlice {
            name: self.0,
            result: query.types().bind(&result)?,
            arguments: required,
            begin_empty,
            end_empty,
        })))
    }

    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Err(Error::Internal(
            "sequence slice requires contextual binding".into(),
        ))
    }

    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        Err(Error::Internal(
            "sequence slice was not specialized before execution".into(),
        ))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn omitted_bound(arguments: &dyn ScalarBindArguments, index: usize) -> Result<bool> {
    if arguments.is_omitted_slice_bound(index)? {
        return Ok(true);
    }
    let data_type = arguments.data_type(index)?;
    if matches!(data_type, DataType::Nested(_)) {
        return Err(Error::Bind("slice bounds must be BIGINT".into()));
    }
    Ok(false)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for BoundSequenceSlice {
    fn name(&self) -> &str {
        self.name
    }

    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        if arguments.len() != self.arguments.len() {
            return Err(Error::Bind(
                "sequence slice argument count changed after binding".into(),
            ));
        }
        Ok(self.arguments.clone())
    }

    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        if arguments != self.arguments {
            return Err(Error::Bind(
                "sequence slice argument types differ from binding".into(),
            ));
        }
        Ok(self.result.data_type().clone())
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if arguments.len() != self.arguments.len() {
            return Err(Error::Internal(
                "sequence slice argument count differs from binding".into(),
            ));
        }
        if arguments[0].is_null()
            || (!self.begin_empty && arguments[1].is_null())
            || (!self.end_empty && arguments[2].is_null())
            || (arguments.len() == 4 && arguments[3].is_null())
        {
            return Ok(Value::Null);
        }
        if self.result.data_type() == &DataType::Null {
            return Ok(Value::Null);
        }
        let Value::Nested(input) = &arguments[0] else {
            return Err(Error::Internal("sequence slice input".into()));
        };
        let NestedPayload::Sequence(values) = &input.payload else {
            return Err(Error::Internal("sequence slice payload".into()));
        };
        let begin = (!self.begin_empty)
            .then(|| slice_index(&arguments[1]))
            .transpose()?;
        let end = (!self.end_empty)
            .then(|| slice_index(&arguments[2]))
            .transpose()?;
        let step = if arguments.len() == 4 {
            slice_index(&arguments[3])?
        } else {
            1
        };
        let indices = slice_indices(values.len(), begin, end, step, query)?;
        query.check_rows(indices.len())?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(indices.len())
            .map_err(|_| Error::Resource("cannot allocate sliced child values".into()))?;
        for (offset, index) in indices.into_iter().enumerate() {
            if offset % 1024 == 0 {
                query.check()?;
            }
            output.push(values[index].clone());
        }
        let result = NestedValue::value(
            self.result.data_type().clone(),
            NestedPayload::Sequence(output),
        )?;
        self.result.validate(&result, query)?;
        Ok(result)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn slice_index(value: &Value) -> Result<i64> {
    i64::try_from(value.as_i128()?)
        .map_err(|_| Error::Conversion("slice bound is outside BIGINT range".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn slice_indices(
    length: usize,
    begin: Option<i64>,
    end: Option<i64>,
    step: i64,
    query: &QueryContext,
) -> Result<Vec<usize>> {
    if step == 0 {
        return Err(Error::InvalidInput("Slice step cannot be zero".into()));
    }
    let value_count = length;
    let length = i128::try_from(value_count)
        .map_err(|_| Error::Resource("sequence slice length exceeds integer range".into()))?;
    let begin_empty = begin.is_none();
    let end_empty = end.is_none();
    let mut begin = begin.map(i128::from).unwrap_or(0);
    let mut end = end.map(i128::from).unwrap_or(length);
    let step = i128::from(step);
    if step < 0 {
        std::mem::swap(&mut begin, &mut end);
        if end_empty {
            begin = 0;
        }
        if begin_empty {
            end = length;
        }
    }
    let minimum = i128::from(i64::MIN);
    if begin != 0 && begin != minimum {
        begin -= 1;
    }
    let begin_was_minimum = begin == minimum;
    if begin_was_minimum {
        begin += 1;
    }
    if begin < 0 && -begin > length && end < 0 && end < -length {
        begin = 0;
        end = 0;
    } else {
        if begin < 0 && -begin > length {
            begin = 0;
        }
        begin = clamp_index(begin, length, begin_was_minimum);
        end = clamp_index(end, length, false);
        end = end.max(begin);
    }
    let mut result = Vec::new();
    let span = end - begin;
    let capacity = if span == 0 {
        0
    } else {
        usize::try_from((span - 1) / step.abs() + 1)
            .unwrap_or(value_count)
            .min(value_count)
    };
    query.check_rows(capacity)?;
    result
        .try_reserve_exact(capacity)
        .map_err(|_| Error::Resource("cannot allocate slice selection".into()))?;
    if step > 0 {
        let mut index = begin;
        while index < end {
            if result.len() % 1024 == 0 {
                query.check_rows(result.len() + 1)?;
            }
            result.push(
                usize::try_from(index).map_err(|_| {
                    Error::Internal("positive slice produced a negative index".into())
                })?,
            );
            index = index.saturating_add(step);
        }
    } else {
        let mut index = end - 1;
        while index >= begin && end > begin {
            if result.len() % 1024 == 0 {
                query.check_rows(result.len() + 1)?;
            }
            result.push(
                usize::try_from(index).map_err(|_| {
                    Error::Internal("negative slice produced a negative index".into())
                })?,
            );
            index = index.saturating_add(step);
        }
    }
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn clamp_index(mut index: i128, length: i128, is_minimum: bool) -> i128 {
    if index < 0 {
        if !is_minimum {
            index += 1;
        }
        length + index
    } else {
        index.min(length)
    }
}
