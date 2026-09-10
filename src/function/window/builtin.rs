use super::*;

struct StreamingRank {
    name: &'static str,
    position: i128,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl StreamingWindowState for StreamingRank {
    fn evaluate(
        &mut self,
        arguments: &crate::common::vector::DataChunk,
        query: &QueryContext,
    ) -> Result<crate::common::vector::Vector> {
        use crate::common::vector::Vector;
        query.check()?;
        if self.name == "row_number" {
            let end = self
                .position
                .checked_add(arguments.len() as i128)
                .ok_or_else(|| Error::Execution("row_number overflow".into()))?;
            let values = (self.position + 1..=end).map(Value::Integer).collect();
            self.position = end;
            Vector::flat(DataType::BigInt, values)
        } else {
            let value = match self.name {
                "percent_rank" => Value::Double(0.0),
                "cume_dist" => Value::Double(1.0),
                _ => Value::Integer(1),
            };
            let data_type = if matches!(self.name, "percent_rank" | "cume_dist") {
                DataType::Double
            } else {
                DataType::BigInt
            };
            Vector::constant(data_type, value, arguments.len())
        }
    }
}

#[derive(Debug)]
struct Builtin(&'static str);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::function) fn register(registry: &mut FunctionRegistry) {
    for name in [
        "row_number",
        "rank",
        "dense_rank",
        "percent_rank",
        "cume_dist",
        "ntile",
        "lead",
        "lag",
        "first_value",
        "last_value",
        "nth_value",
    ] {
        registry
            .register_window(Arc::new(Builtin(name)))
            .expect("unique window builtin");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl WindowFunction for Builtin {
    fn supports_streaming(&self) -> bool {
        matches!(
            self.0,
            "row_number" | "rank" | "dense_rank" | "percent_rank" | "cume_dist"
        )
    }
    fn start_stream(&self) -> Result<Box<dyn StreamingWindowState>> {
        if !self.supports_streaming() {
            return Err(Error::Unsupported("streaming value window".into()));
        }
        Ok(Box::new(StreamingRank {
            name: self.0,
            position: 0,
        }))
    }
    fn name(&self) -> &str {
        self.0
    }
    fn argument_types(&self, arguments: &[DataType]) -> Result<Vec<DataType>> {
        let mut types = arguments.to_vec();
        if matches!(self.0, "lead" | "lag") && types.len() == 3 {
            types[2] = types[0].clone();
        }
        Ok(types)
    }
    fn return_type(
        &self,
        args: &[DataType],
        options: WindowOptions,
        _: &TypeRegistry,
    ) -> Result<DataType> {
        if options.distinct || options.filtered {
            return Err(Error::Bind(
                "DISTINCT and FILTER require an aggregate window function".into(),
            ));
        }
        let value_function = matches!(
            self.0,
            "lead" | "lag" | "first_value" | "last_value" | "nth_value"
        );
        if options.ignores_nulls() && !value_function {
            return Err(Error::Bind(
                "IGNORE NULLS requires a value window function".into(),
            ));
        }
        let valid = match self.0 {
            "lead" | "lag" => (1..=3).contains(&args.len()),
            "first_value" | "last_value" | "ntile" => args.len() == 1,
            "nth_value" => args.len() == 2,
            _ => args.is_empty(),
        };
        if !valid {
            return Err(Error::Bind(format!(
                "invalid argument count for {}",
                self.0
            )));
        }
        if let Some(index) = match self.0 {
            "lead" | "lag" | "nth_value" if args.len() > 1 => Some(1),
            "ntile" => Some(0),
            _ => None,
        } && !args[index].is_integer()
            && args[index] != DataType::Null
        {
            return Err(Error::Bind(format!("{} offset must be an integer", self.0)));
        }
        Ok(if value_function {
            args[0].clone()
        } else if matches!(self.0, "percent_rank" | "cume_dist") {
            DataType::Double
        } else {
            DataType::BigInt
        })
    }
    fn evaluate(&self, input: &WindowInput<'_>, query: &QueryContext) -> Result<Vec<Value>> {
        let n = input.arguments.len();
        let mut result = Vec::with_capacity(n);
        let mut dense = 0i128;
        let nonnull: Vec<_> = if input.options.ignores_nulls() {
            input
                .arguments
                .iter()
                .enumerate()
                .filter_map(|(index, row)| (!row[0].is_null()).then_some(index))
                .collect()
        } else {
            Vec::new()
        };
        for (index, args) in input.arguments.iter().enumerate() {
            query.check()?;
            if input.peers[index].start == index {
                dense += 1;
            }
            let value = match self.0 {
                "row_number" => Value::Integer((index + 1) as i128),
                "rank" => Value::Integer((input.peers[index].start + 1) as i128),
                "dense_rank" => Value::Integer(dense),
                "percent_rank" => Value::Double(if n <= 1 {
                    0.0
                } else {
                    input.peers[index].start as f64 / (n - 1) as f64
                }),
                "cume_dist" => Value::Double(input.peers[index].end as f64 / n as f64),
                "ntile" => {
                    if args[0].is_null() {
                        result.push(Value::Null);
                        continue;
                    }
                    let buckets = position_argument(&args[0])?;
                    if buckets <= 0 {
                        return Err(Error::Execution(
                            "NTILE argument must be greater than zero".into(),
                        ));
                    }
                    let buckets = usize::try_from(buckets).unwrap_or(n).min(n);
                    let size = n / buckets;
                    let extra = n % buckets;
                    let cutoff = (size + 1) * extra;
                    let bucket = if index < cutoff {
                        index / (size + 1)
                    } else {
                        extra + (index - cutoff) / size
                    };
                    Value::Integer((bucket + 1) as i128)
                }
                "lead" | "lag" => {
                    let offset = args.get(1).unwrap_or(&Value::Integer(1));
                    if offset.is_null() {
                        result.push(Value::Null);
                        continue;
                    }
                    let offset = position_argument(offset)?;
                    let delta = if self.0 == "lag" {
                        offset.checked_neg().ok_or_else(|| {
                            Error::Execution("window offset subtraction overflow".into())
                        })?
                    } else {
                        offset
                    };
                    let target = (|| {
                        let delta = i128::from(delta);
                        if delta == 0 {
                            return Some(index);
                        }
                        if !input.options.ignores_nulls() {
                            return (index as i128)
                                .checked_add(delta)
                                .and_then(|v| usize::try_from(v).ok());
                        }
                        let position = if delta > 0 {
                            (nonnull.partition_point(|&i| i <= index) as i128)
                                .checked_add(delta)?
                                .checked_sub(1)?
                        } else {
                            (nonnull.partition_point(|&i| i < index) as i128).checked_add(delta)?
                        };
                        usize::try_from(position)
                            .ok()
                            .and_then(|p| nonnull.get(p).copied())
                    })();
                    target
                        .and_then(|i| input.arguments.get(i))
                        .map(|row| row[0].clone())
                        .unwrap_or_else(|| args.get(2).cloned().unwrap_or(Value::Null))
                }
                "first_value" | "last_value" | "nth_value" => {
                    let frame = &input.frames[index];
                    let nth = if self.0 == "nth_value" {
                        if args[1].is_null() {
                            result.push(Value::Null);
                            continue;
                        }
                        let nth = position_argument(&args[1])?;
                        if nth <= 0 {
                            result.push(Value::Null);
                            continue;
                        }
                        usize::try_from(nth - 1).ok()
                    } else {
                        Some(0)
                    };
                    let target = nth.and_then(|nth| {
                        if input.options.ignores_nulls() {
                            let start = nonnull.partition_point(|&i| i < frame.start);
                            let end = nonnull.partition_point(|&i| i < frame.end);
                            let position = if self.0 == "last_value" {
                                end.checked_sub(nth + 1)
                            } else {
                                start.checked_add(nth)
                            }?;
                            (position >= start && position < end).then(|| nonnull[position])
                        } else {
                            let position = if self.0 == "last_value" {
                                frame.end.checked_sub(nth + 1)
                            } else {
                                frame.start.checked_add(nth)
                            }?;
                            frame.contains(&position).then_some(position)
                        }
                    });
                    target
                        .map(|i| input.arguments[i][0].clone())
                        .unwrap_or(Value::Null)
                }
                _ => return Err(Error::Internal("unregistered window builtin".into())),
            };
            result.push(value);
        }
        Ok(result)
    }
}

/// SQL window positions have a signed BIGINT domain, even when the argument
/// arrives in a wider integer representation. Reject out-of-domain values
/// before doing row/peer arithmetic.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn position_argument(value: &Value) -> Result<i64> {
    let value = value.as_i128()?;
    i64::try_from(value).map_err(|_| Error::Conversion("window position is outside BIGINT".into()))
}
