use super::*;

#[derive(Debug)]
pub struct NumericArithmetic;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl OperatorFunction for NumericArithmetic {
    fn name(&self) -> &'static str {
        "checked-numeric-arithmetic"
    }
    #[allow(private_interfaces)]
    fn batch_kind(&self, _: OperatorBatchAccess) -> Option<OperatorBatchKind> {
        Some(OperatorBatchKind::builtin(
            OperatorBatchIdentity::NumericArithmetic,
        ))
    }
    fn supports(&self, signature: &OperatorSignature) -> bool {
        use Operator::*;
        let t = &signature.result;
        (t.is_integer() || t.is_floating())
            && signature.arguments.len() == signature.operator.arity()
            && signature.arguments.iter().all(|a| a == t)
            && match signature.operator {
                Plus | Negate | Add | Subtract | Multiply => !signature.nullable,
                Divide => t.is_floating() && !signature.nullable,
                IntegerDivide | Modulo => signature.nullable,
                _ => false,
            }
    }
    fn is_total(&self, signature: &OperatorSignature, constants: &[Option<&Value>]) -> bool {
        use Operator::*;
        if constants
            .iter()
            .any(|value| value.is_some_and(Value::is_null))
        {
            return true;
        }
        signature.result.is_floating()
            || signature.operator == Plus
            || (matches!(signature.operator, IntegerDivide | Modulo)
                && match constants.get(1) {
                    Some(Some(Value::Integer(value))) => *value != -1,
                    Some(Some(Value::Unsigned(value))) => *value != 0,
                    _ => false,
                })
    }
    fn evaluate_batch(
        &self,
        signature: &OperatorSignature,
        arguments: &crate::common::vector::DataChunk,
        query: &QueryContext,
    ) -> Result<crate::common::vector::Vector> {
        use crate::common::vector::Vector;
        use Operator::*;
        if signature.result == DataType::Double
            && signature.operator == Divide
            && arguments.columns().len() == 2
            && let Some(Value::Double(divisor)) = arguments.columns()[1].constant_value()
            && arguments.columns()[0].all_valid()
            && let Some(values) = arguments.columns()[0].flat_doubles()
        {
            // Floating division is total, including zero and IEEE special
            // values.  The scalar evaluator's only work here is extracting
            // Values and redispatching this exact operation.
            let mut output = Vec::new();
            output
                .try_reserve_exact(values.len())
                .map_err(|_| Error::Resource("cannot allocate DOUBLE column".into()))?;
            for (index, &value) in values.iter().enumerate() {
                if index % 1024 == 0 {
                    query.check()?;
                }
                output.push(value / *divisor);
            }
            query.check()?;
            return Vector::try_doubles(output);
        }
        if signature.result.is_unsigned_integer()
            && matches!(signature.operator, IntegerDivide | Modulo)
            && let Some(Value::Unsigned(divisor)) = arguments.columns()[1].constant_value()
            && *divisor != 0
        {
            let mask =
                (signature.operator == Modulo && divisor.is_power_of_two()).then_some(divisor - 1);
            let column = &arguments.columns()[0];
            if let Some(mask) = mask
                && *divisor <= 256
                && arguments.len() / 4 > *divisor as usize
            {
                // A small remainder domain admits a compact physical
                // dictionary. NULL has its own entry; this does not merge
                // distinct payloads according to configurable SQL equality.
                let mut values = (0..*divisor).map(Value::Unsigned).collect::<Vec<_>>();
                let null = values.len();
                if !column.all_valid() {
                    values.push(Value::Null);
                }
                let parent = Arc::new(Vector::flat(signature.result.clone(), values)?);
                let mut selected = Vec::with_capacity(arguments.len());
                let mut select = |(index, value): (usize, Value)| -> Result<()> {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    selected.push(match value {
                        Value::Unsigned(value) => (value & mask) as usize,
                        Value::Null => null,
                        _ => unreachable!("validated unsigned column"),
                    });
                    Ok(())
                };
                if let Some(values) = column.flat_values() {
                    values
                        .iter()
                        .cloned()
                        .enumerate()
                        .try_for_each(&mut select)?;
                } else {
                    column.values().enumerate().try_for_each(&mut select)?;
                }
                query.check()?;
                return parent.select(selected);
            }
            let apply = |(index, value): (usize, Value)| {
                if index % 1024 == 0 {
                    query.check()?;
                }
                Ok(match value {
                    Value::Null => None,
                    Value::Unsigned(value) => Some(if let Some(mask) = mask {
                        value & mask
                    } else if signature.operator == Modulo {
                        value % divisor
                    } else {
                        value / divisor
                    }),
                    _ => unreachable!("validated unsigned column"),
                })
            };
            return if let Some(values) = column.flat_values() {
                Vector::try_unsigned(
                    signature.result.clone(),
                    values.iter().cloned().enumerate().map(apply),
                )
            } else {
                Vector::try_unsigned(
                    signature.result.clone(),
                    column.values().enumerate().map(apply),
                )
            };
        }
        if let Some(bits) = signature.result.integer_bits().filter(|bits| *bits <= 64)
            && matches!(signature.operator, Add | Subtract | Multiply)
            && let Some(Value::Integer(right)) = arguments.columns()[1].constant_value()
            && let Ok(right) = i64::try_from(*right)
        {
            let column = &arguments.columns()[0];
            if signature.result == DataType::BigInt {
                return match signature.operator {
                    Add => map_checked_bigint_constant(
                        column,
                        right,
                        query,
                        i64::checked_add,
                        i64::wrapping_add,
                        BigIntOrder::Preserves,
                    ),
                    Subtract => map_checked_bigint_constant(
                        column,
                        right,
                        query,
                        i64::checked_sub,
                        i64::wrapping_sub,
                        BigIntOrder::Preserves,
                    ),
                    Multiply if right == 0 => map_checked_bigint_constant(
                        column,
                        right,
                        query,
                        i64::checked_mul,
                        i64::wrapping_mul,
                        BigIntOrder::AlwaysAscending,
                    ),
                    Multiply if right > 0 => map_checked_bigint_constant(
                        column,
                        right,
                        query,
                        i64::checked_mul,
                        i64::wrapping_mul,
                        BigIntOrder::Preserves,
                    ),
                    Multiply => map_checked_bigint_constant(
                        column,
                        right,
                        query,
                        i64::checked_mul,
                        i64::wrapping_mul,
                        BigIntOrder::Unknown,
                    ),
                    _ => unreachable!("checked arithmetic operation"),
                };
            }
            let minimum = -(1_i128 << (bits - 1));
            let maximum = (1_i128 << (bits - 1)) - 1;
            let operation = |left: i64| {
                let result = match signature.operator {
                    Add => left.checked_add(right),
                    Subtract => left.checked_sub(right),
                    Multiply => left.checked_mul(right),
                    _ => unreachable!("checked arithmetic operation"),
                }
                .ok_or_else(overflow)?;
                if (result as i128) < minimum || (result as i128) > maximum {
                    return Err(overflow());
                }
                Ok(result)
            };
            return map_checked_integer_column(column, &signature.result, query, operation);
        }
        // Fixed-width division uses the declared physical range. The -1 case
        // keeps scalar overflow checks, including narrower integer minima.
        if signature
            .result
            .integer_bits()
            .is_some_and(|bits| bits <= 64)
            && matches!(signature.operator, IntegerDivide | Modulo)
            && let Some(Value::Integer(divisor)) = arguments.columns()[1].constant_value()
            && let Ok(divisor) = i64::try_from(*divisor)
            && divisor != -1
        {
            if divisor == 0 {
                return Vector::constant(signature.result.clone(), Value::Null, arguments.len());
            }
            let magnitude = divisor.unsigned_abs();
            let remainder_mask = (signature.operator == Modulo && magnitude.is_power_of_two())
                .then_some(magnitude - 1);
            let column = &arguments.columns()[0];
            if signature.operator == Modulo
                && magnitude <= 128
                && arguments.len() / 4 >= (magnitude as usize * 2 - 1)
            {
                return dictionary_signed_remainder(
                    column,
                    &signature.result,
                    divisor,
                    magnitude as usize,
                    query,
                );
            }
            if let Some(mask) = remainder_mask {
                return map_integer_column(column, &signature.result, query, |value| {
                    // Remainder keeps the numerator's sign. Unsigned
                    // magnitude also handles the signed minimum exactly.
                    let remainder = (value.unsigned_abs() & mask) as i64;
                    if value < 0 { -remainder } else { remainder }
                });
            } else if signature.operator == Modulo {
                return map_integer_column(column, &signature.result, query, |value| {
                    value % divisor
                });
            } else {
                return map_integer_column(column, &signature.result, query, |value| {
                    value / divisor
                });
            }
        }
        evaluate_operator_rows(self, signature, arguments, query)
    }
    fn evaluate(
        &self,
        signature: &OperatorSignature,
        arguments: &[Value],
        query: &QueryContext,
    ) -> Result<Value> {
        query.check()?;
        use Operator::*;
        let op = signature.operator;
        let value = if signature.result.is_unsigned_integer() {
            let Value::Unsigned(a) = arguments[0] else {
                return Err(Error::Internal("unsigned arithmetic input".into()));
            };
            let b = match arguments.get(1) {
                Some(Value::Unsigned(b)) => *b,
                None => 0,
                _ => return Err(Error::Internal("unsigned arithmetic input".into())),
            };
            let result = match op {
                Plus => Some(a),
                Negate => (a == 0).then_some(0),
                Add => a.checked_add(b),
                Subtract => a.checked_sub(b),
                Multiply => a.checked_mul(b),
                IntegerDivide | Modulo if b == 0 => {
                    return Err(Error::InvalidInput("Division by zero".into()));
                }
                IntegerDivide => a.checked_div(b),
                Modulo => a.checked_rem(b),
                _ => {
                    return Err(Error::Internal(
                        "invalid unsigned arithmetic binding".into(),
                    ));
                }
            };
            let value = Value::Unsigned(result.ok_or_else(|| {
                Error::OutOfRange(format!("Overflow in {} arithmetic", signature.result))
            })?);
            if !value.fits_type(&signature.result) {
                return Err(Error::OutOfRange(format!(
                    "Overflow in {} arithmetic",
                    signature.result
                )));
            }
            value
        } else if signature.result.is_signed_integer() {
            let a = arguments[0].as_i128()?;
            let b = if arguments.len() == 2 {
                arguments[1].as_i128()?
            } else {
                0
            };
            if matches!(op, IntegerDivide | Modulo)
                && b == -1
                && a.checked_neg()
                    .is_none_or(|n| !Value::Integer(n).fits_type(&signature.result))
            {
                return Err(overflow());
            }
            let result = match op {
                Plus => Some(a),
                Negate => a.checked_neg(),
                Add => a.checked_add(b),
                Subtract => a.checked_sub(b),
                Multiply => a.checked_mul(b),
                IntegerDivide | Modulo if b == 0 => return Ok(Value::Null),
                IntegerDivide => a.checked_div(b),
                Modulo => a.checked_rem(b),
                _ => return Err(Error::Internal("invalid integer arithmetic binding".into())),
            };
            Value::Integer(result.ok_or_else(overflow)?)
        } else if signature.result == DataType::Float {
            let a = arguments[0].as_f32()?;
            let b = if arguments.len() == 2 {
                arguments[1].as_f32()?
            } else {
                0.0
            };
            Value::Float(match op {
                Plus => a,
                Negate => -a,
                Add => a + b,
                Subtract => a - b,
                Multiply => a * b,
                IntegerDivide | Modulo if b == 0.0 => return Ok(Value::Null),
                Divide | IntegerDivide => a / b,
                Modulo => a % b,
                _ => return Err(Error::Internal("invalid FLOAT arithmetic binding".into())),
            })
        } else {
            let a = arguments[0].as_f64()?;
            let b = if arguments.len() == 2 {
                arguments[1].as_f64()?
            } else {
                0.0
            };
            Value::Double(match op {
                Plus => a,
                Negate => -a,
                Add => a + b,
                Subtract => a - b,
                Multiply => a * b,
                IntegerDivide | Modulo if b == 0.0 => return Ok(Value::Null),
                Divide | IntegerDivide => a / b,
                Modulo => a % b,
                _ => return Err(Error::Internal("invalid DOUBLE arithmetic binding".into())),
            })
        };
        if !value.fits_type(&signature.result) {
            return Err(overflow());
        }
        Ok(value)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline]
fn dictionary_signed_remainder(
    column: &crate::common::vector::Vector,
    data_type: &DataType,
    divisor: i64,
    magnitude: usize,
    query: &QueryContext,
) -> Result<crate::common::vector::Vector> {
    use crate::common::vector::Vector;
    let minimum = -(magnitude as i64 - 1);
    let mut values = (minimum..=magnitude as i64 - 1)
        .map(|value| Value::Integer(value as i128))
        .collect::<Vec<_>>();
    let null = values.len();
    if !column.all_valid() {
        values.push(Value::Null);
    }
    let parent = Arc::new(Vector::flat(data_type.clone(), values)?);
    let mask = magnitude
        .is_power_of_two()
        .then_some((magnitude - 1) as u64);
    let select = |(index, value): (usize, Value)| {
        if index % 1024 == 0 {
            query.check()?;
        }
        Ok(match value {
            Value::Integer(value) => {
                let value = value as i64;
                let remainder = if let Some(mask) = mask {
                    let remainder = (value.unsigned_abs() & mask) as i64;
                    if value < 0 { -remainder } else { remainder }
                } else {
                    value % divisor
                };
                (remainder - minimum) as usize
            }
            Value::Null => null,
            _ => unreachable!("validated integer vector"),
        })
    };
    let selection = if let Some(values) = column.flat_bigints() {
        let mut selected = Vec::with_capacity(values.len());
        for (index, &value) in values.iter().enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            let remainder = if let Some(mask) = mask {
                let remainder = (value.unsigned_abs() & mask) as i64;
                if value < 0 { -remainder } else { remainder }
            } else {
                value % divisor
            };
            selected.push((remainder - minimum) as usize);
        }
        selected
    } else if let Some(values) = column.flat_values() {
        values
            .iter()
            .cloned()
            .enumerate()
            .map(select)
            .collect::<Result<_>>()?
    } else {
        column
            .values()
            .enumerate()
            .map(select)
            .collect::<Result<_>>()?
    };
    query.check()?;
    parent.select(selection)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Choose the arithmetic kernel once per column, retaining scalar NULL and
/// signed remainder rules without redispatching the operator for every value.
#[inline]
fn map_integer_column(
    column: &crate::common::vector::Vector,
    data_type: &DataType,
    query: &QueryContext,
    operation: impl Fn(i64) -> i64,
) -> Result<crate::common::vector::Vector> {
    if data_type == &DataType::BigInt
        && let Some(values) = column.flat_bigints()
    {
        let mut output = Vec::with_capacity(values.len());
        let mut numeric_ascending = true;
        let mut previous = None;
        for (index, &value) in values.iter().enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            let value = operation(value);
            numeric_ascending &= previous.is_none_or(|previous| previous <= value);
            previous = Some(value);
            output.push(value);
        }
        query.check()?;
        return Ok(
            crate::common::vector::Vector::bigints_prevalidated_with_order(
                output,
                numeric_ascending,
            ),
        );
    }
    let apply = |value: Value| match value {
        Value::Null => None,
        Value::Integer(value) => Some(operation(value as i64)),
        _ => unreachable!("validated integer vector"),
    };
    if let Some(values) = column.flat_values() {
        narrow_column(data_type, values.iter().cloned(), apply, query)
    } else {
        narrow_column(data_type, column.values(), apply, query)
    }
}

#[derive(Clone, Copy)]
enum BigIntOrder {
    Preserves,
    AlwaysAscending,
    Unknown,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline]
fn map_checked_bigint_constant<F, W>(
    column: &crate::common::vector::Vector,
    right: i64,
    query: &QueryContext,
    operation: F,
    wrapping: W,
    order: BigIntOrder,
) -> Result<crate::common::vector::Vector>
where
    F: Fn(i64, i64) -> Option<i64>,
    W: Fn(i64, i64) -> i64,
{
    use crate::common::vector::{SignedI64At, Vector};

    let ordered = match order {
        BigIntOrder::Preserves => column.numeric_ascending(),
        BigIntOrder::AlwaysAscending => true,
        BigIntOrder::Unknown => false,
    };
    let apply = |value| operation(value, right).ok_or_else(overflow);
    if column.all_valid()
        && let Some(values) = column.flat_bigints()
    {
        let mut output = Vec::new();
        output
            .try_reserve_exact(values.len())
            .map_err(|_| Error::Resource("cannot allocate BIGINT column".into()))?;
        let proven = prove_ordered_bigint_range(
            values.first().copied(),
            values.last().copied(),
            right,
            &operation,
        );
        if proven && column.numeric_ascending() {
            output.resize(values.len(), 0);
            for (input, output) in values.chunks(1024).zip(output.chunks_mut(1024)) {
                query.check()?;
                for (&value, output) in input.iter().zip(output) {
                    *output = wrapping(value, right);
                }
            }
        } else {
            for (index, &value) in values.iter().enumerate() {
                if index % 1024 == 0 {
                    query.check()?;
                }
                output.push(apply(value)?);
            }
        }
        query.check()?;
        return Ok(Vector::bigints_prevalidated_with_order(output, ordered));
    }
    if let Some(value) = column.constant_value() {
        let value = match value {
            Value::Null => Value::Null,
            Value::Integer(value) => Value::Integer(apply(*value as i64)? as i128),
            _ => unreachable!("validated integer vector"),
        };
        return Vector::constant(DataType::BigInt, value, column.len());
    }
    if let Some((parent, _)) = column.dictionary()
        && parent.len() <= column.len() / 4
    {
        return map_checked_integer_column(column, &DataType::BigInt, query, apply);
    }
    if let Some((parent, selection)) = column.dictionary()
        && parent.all_valid()
        && let Some(values) = parent.flat_bigints()
    {
        let first = selection
            .first()
            .and_then(|&index| values.get(index))
            .copied();
        let last = selection
            .last()
            .and_then(|&index| values.get(index))
            .copied();
        let proven = column.numeric_ascending()
            && prove_ordered_bigint_range(first, last, right, &operation);
        let mut output = Vec::new();
        output
            .try_reserve_exact(selection.len())
            .map_err(|_| Error::Resource("cannot allocate BIGINT column".into()))?;
        if proven {
            output.resize(selection.len(), 0);
            for (indices, output) in selection.chunks(1024).zip(output.chunks_mut(1024)) {
                query.check()?;
                for (&index, output) in indices.iter().zip(output) {
                    let value = *values.get(index).expect("validated dictionary selection");
                    *output = wrapping(value, right);
                }
            }
        } else {
            for (offset, &index) in selection.iter().enumerate() {
                if offset % 1024 == 0 {
                    query.check()?;
                }
                let value = *values.get(index).expect("validated dictionary selection");
                output.push(apply(value)?);
            }
        }
        query.check()?;
        return Ok(Vector::bigints_prevalidated_with_order(output, ordered));
    }
    if !column.all_valid() {
        return map_checked_integer_column(column, &DataType::BigInt, query, apply);
    }
    if column.flat_values().is_some() {
        return map_checked_integer_column(column, &DataType::BigInt, query, apply);
    }
    // Preflight the exact logical order before doing arithmetic. A selected or
    // chunked physical BIGINT view can then avoid Value reconstruction; any
    // unsupported view retains the ordinary mapper without partial output.
    let proven = if column.numeric_ascending() {
        let first = match column.signed_i64_at(0) {
            SignedI64At::Value(value) => Some(value),
            _ => None,
        };
        let last = match column.signed_i64_at(column.len().saturating_sub(1)) {
            SignedI64At::Value(value) => Some(value),
            _ => None,
        };
        prove_ordered_bigint_range(first, last, right, &operation)
    } else {
        false
    };
    for index in 0..column.len() {
        if index % 1024 == 0 {
            query.check()?;
        }
        if !matches!(column.signed_i64_at(index), SignedI64At::Value(_)) {
            return map_checked_integer_column(column, &DataType::BigInt, query, apply);
        }
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(column.len())
        .map_err(|_| Error::Resource("cannot allocate BIGINT column".into()))?;
    for index in 0..column.len() {
        if index % 1024 == 0 {
            query.check()?;
        }
        let SignedI64At::Value(value) = column.signed_i64_at(index) else {
            unreachable!("preflighted BIGINT physical view");
        };
        output.push(if proven {
            wrapping(value, right)
        } else {
            apply(value)?
        });
    }
    query.check()?;
    Ok(Vector::bigints_prevalidated_with_order(output, ordered))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline]
fn prove_ordered_bigint_range(
    first: Option<i64>,
    last: Option<i64>,
    right: i64,
    operation: &impl Fn(i64, i64) -> Option<i64>,
) -> bool {
    first.zip(last).is_some_and(|(first, last)| {
        operation(first, right).is_some() && operation(last, right).is_some()
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline]
fn map_checked_integer_column(
    column: &crate::common::vector::Vector,
    data_type: &DataType,
    query: &QueryContext,
    operation: impl Fn(i64) -> Result<i64>,
) -> Result<crate::common::vector::Vector> {
    use crate::common::vector::Vector;
    if data_type == &DataType::BigInt
        && column.all_valid()
        && let Some(values) = column.flat_bigints()
    {
        let mut output = Vec::new();
        output
            .try_reserve_exact(values.len())
            .map_err(|_| Error::Resource("cannot allocate BIGINT column".into()))?;
        let mut numeric_ascending = true;
        let mut previous = None;
        for (index, &value) in values.iter().enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            // Retain the selected checked operation and its first lane error.
            let value = operation(value)?;
            numeric_ascending &= previous.is_none_or(|previous| previous <= value);
            previous = Some(value);
            output.push(value);
        }
        query.check()?;
        return Ok(Vector::bigints_prevalidated_with_order(
            output,
            numeric_ascending,
        ));
    }
    if let Some(value) = column.constant_value() {
        let value = match value {
            Value::Null => Value::Null,
            Value::Integer(value) => Value::Integer(operation(*value as i64)? as i128),
            _ => unreachable!("validated integer vector"),
        };
        return Vector::constant(data_type.clone(), value, column.len());
    }
    if let Some((parent, selection)) = column.dictionary()
        && parent.len() <= column.len() / 4
    {
        let mut entries = vec![usize::MAX; parent.len()];
        let mut values = Vec::with_capacity(parent.len());
        let mut mapped = Vec::with_capacity(selection.len());
        for (offset, &index) in selection.iter().enumerate() {
            if offset % 1024 == 0 {
                query.check()?;
            }
            let entry = if entries[index] == usize::MAX {
                let value = match parent.get(index).expect("validated dictionary index") {
                    Value::Null => Value::Null,
                    Value::Integer(value) => Value::Integer(operation(value as i64)? as i128),
                    _ => unreachable!("validated integer vector"),
                };
                let entry = values.len();
                entries[index] = entry;
                values.push(value);
                entry
            } else {
                entries[index]
            };
            mapped.push(entry);
        }
        query.check()?;
        return Arc::new(Vector::flat(data_type.clone(), values)?).select(mapped);
    }
    let apply = |(index, value): (usize, Value)| {
        if index % 1024 == 0 {
            query.check()?;
        }
        match value {
            Value::Null => Ok(None),
            Value::Integer(value) => operation(value as i64).map(Some),
            _ => unreachable!("validated integer vector"),
        }
    };
    if let Some(values) = column.flat_values() {
        return checked_integer_vector(values.iter().cloned().enumerate().map(apply), data_type);
    }
    let values = column.values().enumerate().map(apply);
    if data_type == &DataType::BigInt {
        crate::common::vector::Vector::try_bigints(values)
    } else {
        crate::common::vector::Vector::flat(
            data_type.clone(),
            values
                .map(|value| {
                    value.map(|value| value.map_or(Value::Null, |v| Value::Integer(v as i128)))
                })
                .collect::<Result<_>>()?,
        )
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline]
fn checked_integer_vector(
    values: impl Iterator<Item = Result<Option<i64>>>,
    data_type: &DataType,
) -> Result<crate::common::vector::Vector> {
    if data_type == &DataType::BigInt {
        crate::common::vector::Vector::try_bigints(values)
    } else {
        crate::common::vector::Vector::flat(
            data_type.clone(),
            values
                .map(|value| {
                    value.map(|value| value.map_or(Value::Null, |v| Value::Integer(v as i128)))
                })
                .collect::<Result<_>>()?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn checked_bigint_constant_uses_selected_and_chunked_physical_lanes() -> Result<()> {
        use crate::common::vector::Vector;
        let query = QueryContext::background();
        let parent = Vector::try_bigints([Ok(Some(10)), Ok(Some(20)), Ok(Some(30))])?;
        let selected = Arc::new(parent).select(vec![2, 0, 1, 2])?;
        let output = map_checked_bigint_constant(
            &selected,
            1,
            &query,
            i64::checked_add,
            i64::wrapping_add,
            BigIntOrder::Preserves,
        )?;
        assert_eq!(
            output.values().collect::<Vec<_>>(),
            vec![
                Value::Integer(31),
                Value::Integer(11),
                Value::Integer(21),
                Value::Integer(31)
            ]
        );
        assert!(!output.numeric_ascending());

        let sorted = Vector::try_bigints([Ok(Some(i64::MIN + 1)), Ok(Some(0)), Ok(Some(4))])?;
        assert!(sorted.numeric_ascending());
        let output = map_checked_bigint_constant(
            &sorted,
            1,
            &query,
            i64::checked_add,
            i64::wrapping_add,
            BigIntOrder::Preserves,
        )?;
        assert_eq!(
            output.values().collect::<Vec<_>>(),
            vec![
                Value::Integer((i64::MIN + 2).into()),
                Value::Integer(1),
                Value::Integer(5)
            ]
        );
        assert!(output.numeric_ascending());
        let small_sorted = Vector::try_bigints([Ok(Some(-2)), Ok(Some(0)), Ok(Some(4))])?;
        let output = map_checked_bigint_constant(
            &small_sorted,
            -2,
            &query,
            i64::checked_mul,
            i64::wrapping_mul,
            BigIntOrder::Unknown,
        )?;
        assert!(!output.numeric_ascending());
        let output = map_checked_bigint_constant(
            &small_sorted,
            0,
            &query,
            i64::checked_mul,
            i64::wrapping_mul,
            BigIntOrder::AlwaysAscending,
        )?;
        assert!(output.numeric_ascending());

        let selected_sorted = Arc::new(small_sorted.clone()).select(vec![0, 1, 2])?;
        let output = map_checked_bigint_constant(
            &selected_sorted,
            1,
            &query,
            i64::checked_add,
            i64::wrapping_add,
            BigIntOrder::Preserves,
        )?;
        assert_eq!(
            output.values().collect::<Vec<_>>(),
            vec![Value::Integer(-1), Value::Integer(1), Value::Integer(5)]
        );
        assert!(output.numeric_ascending());

        let chunks = Vector::chunked(
            DataType::BigInt,
            vec![
                Vector::try_bigints([Ok(Some(-2)), Ok(Some(-1))])?,
                Vector::try_bigints([Ok(Some(0)), Ok(Some(1))])?,
            ],
        )?;
        let output = map_checked_bigint_constant(
            &chunks,
            2,
            &query,
            i64::checked_add,
            i64::wrapping_add,
            BigIntOrder::Preserves,
        )?;
        assert_eq!(
            output.values().collect::<Vec<_>>(),
            vec![
                Value::Integer(0),
                Value::Integer(1),
                Value::Integer(2),
                Value::Integer(3)
            ]
        );
        // BIGINT chunk collections carry unknown ordering even when their
        // values happen to ascend; addition must preserve that conservative
        // metadata rather than infer a stronger guarantee from this example.
        assert!(!chunks.numeric_ascending());
        assert!(!output.numeric_ascending());

        let overflow = Vector::chunked(
            DataType::BigInt,
            vec![
                Vector::try_bigints([Ok(Some(-1))])?,
                Vector::try_bigints([Ok(Some(i64::MAX))])?,
            ],
        )?;
        assert!(matches!(
            map_checked_bigint_constant(
                &overflow,
                1,
                &query,
                i64::checked_add,
                i64::wrapping_add,
                BigIntOrder::Preserves,
            ),
            Err(Error::Execution(message)) if message == "integer overflow"
        ));

        let output = map_checked_bigint_constant(
            &chunks,
            -2,
            &query,
            i64::checked_mul,
            i64::wrapping_mul,
            BigIntOrder::Unknown,
        )?;
        assert_eq!(
            output.values().collect::<Vec<_>>(),
            vec![
                Value::Integer(4),
                Value::Integer(2),
                Value::Integer(0),
                Value::Integer(-2)
            ]
        );
        assert!(!output.numeric_ascending());

        let long = Vector::chunked(
            DataType::BigInt,
            vec![
                Vector::try_bigints((0..1024).map(|value| Ok(Some(value))))?,
                Vector::try_bigints([Ok(Some(1024))])?,
            ],
        )?;
        let interrupt = crate::parallel::InterruptHandle::default();
        let cancelled = QueryContext::new(interrupt.clone(), None, 2048, usize::MAX)?;
        interrupt.interrupt();
        assert!(matches!(
            map_checked_bigint_constant(
                &long,
                1,
                &cancelled,
                i64::checked_add,
                i64::wrapping_add,
                BigIntOrder::Preserves,
            ),
            Err(Error::Interrupted)
        ));
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline]
fn narrow_column(
    data_type: &DataType,
    values: impl Iterator<Item = Value>,
    apply: impl Fn(Value) -> Option<i64>,
    query: &QueryContext,
) -> Result<crate::common::vector::Vector> {
    use crate::common::vector::Vector;
    let values = values.enumerate().map(|(index, value)| {
        if index % 1024 == 0 {
            query.check()?;
        }
        Ok(apply(value))
    });
    if data_type == &DataType::BigInt {
        Vector::try_bigints(values)
    } else {
        Vector::flat(
            data_type.clone(),
            values
                .map(|value| {
                    value.map(|value| {
                        value.map_or(Value::Null, |value| Value::Integer(value as i128))
                    })
                })
                .collect::<Result<_>>()?,
        )
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn overflow() -> Error {
    Error::Execution("integer overflow".into())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut OperatorRegistry) {
    use Operator::*;
    for t in [
        DataType::TinyInt,
        DataType::SmallInt,
        DataType::Integer,
        DataType::BigInt,
        DataType::HugeInt,
        DataType::UTinyInt,
        DataType::USmallInt,
        DataType::UInteger,
        DataType::UBigInt,
        DataType::UHugeInt,
        DataType::Float,
        DataType::Double,
    ] {
        for op in [
            Plus,
            Negate,
            Add,
            Subtract,
            Multiply,
            Divide,
            IntegerDivide,
            Modulo,
        ] {
            let signature = OperatorSignature {
                operator: op,
                arguments: vec![t.clone(); op.arity()],
                result: t.clone(),
                nullable: matches!(op, IntegerDivide | Modulo),
            };
            if NumericArithmetic.supports(&signature) {
                registry
                    .register(signature, Arc::new(NumericArithmetic))
                    .expect("unique numeric operator");
            }
        }
    }
}
