use super::*;

#[derive(Debug)]
pub struct NumericArithmetic;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl OperatorFunction for NumericArithmetic {
    fn name(&self) -> &'static str {
        "checked-numeric-arithmetic"
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
                && matches!(constants.get(1), Some(Some(Value::Integer(value))) if *value != -1))
    }
    fn evaluate_batch(
        &self,
        signature: &OperatorSignature,
        arguments: &crate::common::vector::DataChunk,
        query: &QueryContext,
    ) -> Result<crate::common::vector::Vector> {
        use crate::common::vector::Vector;
        use Operator::*;
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
/// Choose the arithmetic kernel once per column, retaining scalar NULL and
/// signed remainder rules without redispatching the operator for every value.
#[inline]
fn map_integer_column(
    column: &crate::common::vector::Vector,
    data_type: &DataType,
    query: &QueryContext,
    operation: impl Fn(i64) -> i64,
) -> Result<crate::common::vector::Vector> {
    let apply = |value: &Value| match value {
        Value::Null => None,
        Value::Integer(value) => Some(operation(*value as i64)),
        _ => unreachable!("validated integer vector"),
    };
    if let Some(values) = column.flat_values() {
        narrow_column(data_type, values.iter(), apply, query)
    } else {
        narrow_column(data_type, column.values(), apply, query)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline]
fn narrow_column<'a>(
    data_type: &DataType,
    values: impl Iterator<Item = &'a Value>,
    apply: impl Fn(&Value) -> Option<i64>,
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
