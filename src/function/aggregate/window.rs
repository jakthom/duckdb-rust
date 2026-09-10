use super::*;
use crate::{function::window::WindowInput, parallel::QueryContext};
use std::collections::HashSet;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn evaluate(
    function: &Builtin,
    input: &WindowInput<'_>,
    query: &QueryContext,
) -> Result<Option<Vec<Value>>> {
    // Count and narrow integer sums admit exact prefix subtraction for arbitrary
    // frames. Restrict widths so no frame's intermediate sum can overflow i128.
    let count = function.0 == "count";
    let narrow_sum = function.0 == "sum"
        && input
            .argument_types
            .first()
            .and_then(DataType::integer_bits)
            .is_some_and(|bits| bits <= 64);
    if !input.options.distinct && (count || narrow_sum) {
        if input
            .frames
            .uniform_range()
            .is_some_and(|frame| frame.start == 0 && frame.end == input.arguments.len())
            || input
                .frames
                .iter()
                .all(|frame| frame.start == 0 && frame.end == input.arguments.len())
        {
            let mut sum = 0i128;
            let mut included = 0usize;
            for (index, row) in input.arguments.iter().enumerate() {
                if index % 1024 == 0 {
                    query.check()?;
                }
                if input.filter[index] && row.first().is_none_or(|value| !value.is_null()) {
                    included += 1;
                    if narrow_sum {
                        sum = sum
                            .checked_add(row[0].as_i128()?)
                            .ok_or_else(|| Error::Execution("window sum overflow".into()))?;
                    }
                }
            }
            let value = if count {
                Value::Integer(included as i128)
            } else if included == 0 {
                Value::Null
            } else {
                Value::Integer(sum)
            };
            query.check()?;
            return Ok(Some(vec![value; input.arguments.len()]));
        }
        let mut sums = vec![0i128];
        let mut counts = vec![0usize];
        for (index, row) in input.arguments.iter().enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            let included = input.filter[index] && row.first().is_none_or(|value| !value.is_null());
            let value = if narrow_sum && included {
                row[0].as_i128()?
            } else {
                0
            };
            sums.push(
                sums.last()
                    .unwrap()
                    .checked_add(value)
                    .ok_or_else(|| Error::Execution("window sum overflow".into()))?,
            );
            counts.push(counts.last().unwrap() + usize::from(included));
        }
        return input
            .frames
            .iter()
            .enumerate()
            .map(|(index, frame)| {
                if index % 1024 == 0 {
                    query.check()?;
                }
                let size = counts[frame.end] - counts[frame.start];
                Ok(if count {
                    Value::Integer(size as i128)
                } else if size == 0 {
                    Value::Null
                } else {
                    Value::Integer(sums[frame.end] - sums[frame.start])
                })
            })
            .collect::<Result<Vec<_>>>()
            .map(Some);
    }
    if input.frames.iter().all(|frame| frame.start == 0)
        && input
            .frames
            .iter()
            .zip(input.frames.iter().skip(1))
            .all(|(left, right)| left.end <= right.end)
    {
        let mut state = State {
            name: function.0,
            data_type: function.return_type(input.argument_types, query.types())?,
            count: 0,
            value: Value::Null,
            seen: false,
        };
        let types = input
            .argument_types
            .iter()
            .map(|t| query.types().bind(t))
            .collect::<Result<Vec<_>>>()?;
        let mut seen = HashSet::new();
        let mut consumed = 0;
        let mut result = Vec::with_capacity(input.frames.len());
        for frame in input.frames.iter() {
            query.check()?;
            while consumed < frame.end {
                let row = &input.arguments[consumed];
                if input.filter[consumed] {
                    let include = if input.options.distinct {
                        let mut key = Vec::new();
                        for (value, data_type) in row.iter().zip(&types) {
                            data_type.append_key(value, &mut key, query)?;
                        }
                        seen.insert(key)
                    } else {
                        true
                    };
                    if include {
                        state.update(row, query)?;
                    }
                }
                consumed += 1;
            }
            result.push(match function.0 {
                "count" => Value::Integer(state.count),
                "avg" if state.count > 0 => {
                    Value::Double(state.value.as_f64()? / state.count as f64)
                }
                _ => state.value.clone(),
            });
        }
        return Ok(Some(result));
    }
    Ok(None)
}
