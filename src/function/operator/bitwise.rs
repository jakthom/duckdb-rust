//! Fixed-width numeric and length-preserving BIT operations share SQL dispatch,
//! but BIT shifts operate on logical positions rather than integer arithmetic.
use super::*;

#[derive(Debug)]
pub struct Bitwise;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl OperatorFunction for Bitwise {
    fn name(&self) -> &'static str {
        "checked-bitwise-operations"
    }
    fn supports(&self, signature: &OperatorSignature) -> bool {
        use Operator::*;
        if !matches!(
            signature.operator,
            BitAnd | BitOr | BitXor | BitNot | ShiftLeft | ShiftRight
        ) || signature.nullable
            || signature.arguments.len() != signature.operator.arity()
        {
            return false;
        }
        if signature.result == DataType::Bit {
            signature.arguments[0] == DataType::Bit
                && (signature.arguments.len() == 1
                    || signature.arguments[1]
                        == if matches!(signature.operator, ShiftLeft | ShiftRight) {
                            DataType::Integer
                        } else {
                            DataType::Bit
                        })
        } else {
            signature.result.is_integer()
                && signature.arguments.iter().all(|t| *t == signature.result)
        }
    }
    fn evaluate(
        &self,
        signature: &OperatorSignature,
        arguments: &[Value],
        query: &QueryContext,
    ) -> Result<Value> {
        evaluate(signature.operator, &signature.result, arguments, query)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn evaluate(
    operator: Operator,
    data_type: &DataType,
    arguments: &[Value],
    query: &QueryContext,
) -> Result<Value> {
    query.check()?;
    use Operator::*;
    if let Value::Bit(left) = &arguments[0] {
        let value = match operator {
            BitNot => left.invert(|| query.check())?,
            ShiftLeft | ShiftRight => {
                let shift = arguments[1].as_i128()?;
                if shift < 0 && operator == ShiftLeft {
                    return Err(Error::OutOfRange(
                        "Cannot left-shift by negative number".into(),
                    ));
                }
                let shift = usize::try_from(shift).unwrap_or(usize::MAX);
                left.shift(shift, operator == ShiftLeft, || query.check())?
            }
            BitAnd | BitOr | BitXor => {
                let Value::Bit(right) = &arguments[1] else {
                    return Err(Error::Internal("BIT right operand".into()));
                };
                left.bitwise(
                    right,
                    match operator {
                        BitAnd => |a, b| a & b,
                        BitOr => |a, b| a | b,
                        _ => |a, b| a ^ b,
                    },
                    || query.check(),
                )?
            }
            _ => return Err(Error::Internal("invalid BIT operation".into())),
        };
        return Ok(value.value());
    }
    let signed = data_type.is_signed_integer();
    let width = data_type
        .integer_bits()
        .or_else(|| data_type.unsigned_bits())
        .ok_or_else(|| Error::Internal("bitwise integer width".into()))?;
    let mask = if width == 128 {
        u128::MAX
    } else {
        (1_u128 << width) - 1
    };
    let word = |value: &Value| match value {
        Value::Integer(value) => Ok(*value as u128 & mask),
        Value::Unsigned(value) => Ok(*value),
        _ => Err(Error::Internal("bitwise numeric operand".into())),
    };
    let left = word(&arguments[0])?;
    let result = match operator {
        BitNot => !left & mask,
        BitAnd => left & word(&arguments[1])?,
        BitOr => left | word(&arguments[1])?,
        BitXor => left ^ word(&arguments[1])?,
        ShiftLeft | ShiftRight => {
            let negative = matches!(arguments[1],Value::Integer(value) if value<0);
            let count = word(&arguments[1])?;
            if operator == ShiftRight {
                if negative || count >= u128::from(width) {
                    0
                } else if signed {
                    (arguments[0].as_i128()? >> count as u32) as u128 & mask
                } else {
                    left >> count as u32
                }
            } else {
                if negative || matches!(arguments[0],Value::Integer(value) if value<0) {
                    return Err(Error::OutOfRange(
                        "Cannot left-shift a negative number or count".into(),
                    ));
                }
                let maximum = if signed { mask >> 1 } else { mask };
                if left == 0 {
                    0
                } else if count >= u128::from(width) || left > (maximum >> count as u32) {
                    return Err(Error::OutOfRange("Overflow in left shift".into()));
                } else {
                    left << count as u32
                }
            }
        }
        _ => return Err(Error::Internal("invalid numeric bitwise operation".into())),
    };
    Ok(if signed {
        Value::Integer(((result as i128) << (128 - width)) >> (128 - width))
    } else {
        Value::Unsigned(result)
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut OperatorRegistry) {
    use Operator::*;
    for data_type in [
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
        DataType::Bit,
    ] {
        for operator in [BitAnd, BitOr, BitXor, BitNot, ShiftLeft, ShiftRight] {
            let mut arguments = vec![data_type.clone(); operator.arity()];
            if data_type == DataType::Bit && matches!(operator, ShiftLeft | ShiftRight) {
                arguments[1] = DataType::Integer;
            }
            registry
                .register(
                    OperatorSignature {
                        operator,
                        arguments,
                        result: data_type.clone(),
                        nullable: false,
                    },
                    Arc::new(Bitwise),
                )
                .expect("unique bitwise signature");
        }
    }
}
