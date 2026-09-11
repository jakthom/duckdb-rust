use super::*;
use crate::common::Date;

#[derive(Debug)]
pub struct DateArithmetic;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl OperatorFunction for DateArithmetic {
    fn name(&self) -> &'static str {
        "gregorian-date-arithmetic"
    }
    fn supports(&self, signature: &OperatorSignature) -> bool {
        use DataType::*;
        !signature.nullable
            && matches!(
                (
                    signature.operator,
                    signature.arguments.as_slice(),
                    &signature.result
                ),
                (Operator::Add, [Date, Integer] | [Integer, Date], Date)
                    | (Operator::Subtract, [Date, Integer], Date)
                    | (Operator::Subtract, [Date, Date], BigInt)
            )
    }

    fn evaluate(
        &self,
        signature: &OperatorSignature,
        arguments: &[Value],
        query: &QueryContext,
    ) -> Result<Value> {
        query.check()?;
        if signature.result == DataType::BigInt {
            return Ok(Value::Integer(
                i128::from(arguments[0].as_date()?.days())
                    - i128::from(arguments[1].as_date()?.days()),
            ));
        }
        let (date, offset) = if signature.arguments[0] == DataType::Date {
            (arguments[0].as_date()?, arguments[1].as_i128()?)
        } else {
            (arguments[1].as_date()?, arguments[0].as_i128()?)
        };
        if !date.is_finite() {
            return Ok(Value::Date(date));
        }
        let days = if signature.operator == Operator::Subtract {
            i128::from(date.days()) - offset
        } else {
            i128::from(date.days()) + offset
        };
        if !(-2147483646..=2147483646).contains(&days) {
            return Err(Error::Execution("DATE outside finite range".into()));
        }
        Ok(Value::Date(Date::from_days(days as i32)?))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut OperatorRegistry) {
    use DataType::*;
    for (op, arguments, result) in [
        (Operator::Add, vec![Date, Integer], Date),
        (Operator::Add, vec![Integer, Date], Date),
        (Operator::Subtract, vec![Date, Integer], Date),
        (Operator::Subtract, vec![Date, Date], BigInt),
    ] {
        registry
            .register(
                OperatorSignature {
                    operator: op,
                    arguments,
                    result,
                    nullable: false,
                },
                Arc::new(DateArithmetic),
            )
            .expect("unique date operator");
    }
}
