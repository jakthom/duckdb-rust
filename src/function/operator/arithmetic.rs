use super::*;

#[derive(Debug)]
pub struct NumericArithmetic;

impl OperatorFunction for NumericArithmetic {
    fn name(&self) -> &'static str {
        "checked-numeric-arithmetic"
    }
    fn supports(&self, signature: &OperatorSignature) -> bool {
        use Operator::*;
        let t = &signature.result;
        t.is_numeric()
            && signature.arguments.len() == signature.operator.arity()
            && signature.arguments.iter().all(|a| a == t)
            && match signature.operator {
                Plus | Negate | Add | Subtract | Multiply => !signature.nullable,
                Divide => t.is_floating() && !signature.nullable,
                IntegerDivide | Modulo => signature.nullable,
                _ => false,
            }
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
        let value = if signature.result.is_integer() {
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

fn overflow() -> Error {
    Error::Execution("integer overflow".into())
}

pub(super) fn register(registry: &mut OperatorRegistry) {
    use Operator::*;
    for t in [
        DataType::TinyInt,
        DataType::SmallInt,
        DataType::Integer,
        DataType::BigInt,
        DataType::HugeInt,
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
