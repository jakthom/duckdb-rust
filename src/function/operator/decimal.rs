use super::*;
use crate::common::numeric::{decimal, rescale};

#[derive(Debug)]
pub struct DecimalArithmetic;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl OperatorFunction for DecimalArithmetic {
    fn name(&self) -> &'static str {
        "checked-decimal-arithmetic"
    }
    fn supports(&self, signature: &OperatorSignature) -> bool {
        signature.result.is_decimal()
            && signature.arguments.iter().all(DataType::is_decimal)
            && signature.arguments.len() == signature.operator.arity()
            && matches!(
                signature.operator,
                Operator::Plus
                    | Operator::Negate
                    | Operator::Add
                    | Operator::Subtract
                    | Operator::Multiply
                    | Operator::Modulo
            )
            && signature.nullable == (signature.operator == Operator::Modulo)
    }
    fn specialize(
        &self,
        operator: Operator,
        arguments: &[OperatorArgument<'_>],
    ) -> Result<Option<OperatorSignature>> {
        use DataType::Decimal;
        use Operator::*;
        if !matches!(operator, Plus | Negate | Add | Subtract | Multiply | Modulo)
            || !arguments.iter().any(|a| a.data_type.is_decimal())
            || arguments.iter().any(|a| {
                !(a.data_type.is_integer()
                    || a.data_type.is_decimal()
                    || *a.data_type == DataType::Null)
            })
        {
            return Ok(None);
        }
        let properties: Vec<_> = arguments
            .iter()
            .map(|a| a.data_type.decimal_properties().unwrap())
            .collect();
        let width = properties.iter().map(|p| p.0).max().unwrap_or(1);
        let scale = properties.iter().map(|p| p.1).max().unwrap_or(0);
        let (result_width, result_scale) = if operator == Multiply {
            let s: u8 = properties.iter().map(|p| p.1).sum();
            if s > 38 {
                return Err(Error::Bind(
                    "decimal multiplication requires scale greater than 38".into(),
                ));
            }
            let mut w: u8 = properties.iter().map(|p| p.0).sum();
            if w > 18 && width <= 18 && s < 18 {
                w = 18;
            }
            (w.min(38), s)
        } else if matches!(operator, Plus | Negate) {
            (width, scale)
        } else {
            let integral = properties.iter().map(|p| p.0 - p.1).max().unwrap_or(0);
            (
                (integral + scale + u8::from(operator != Modulo)).min(38),
                scale,
            )
        };
        let result = Decimal {
            width: result_width,
            scale: result_scale,
        };
        let arguments = properties
            .iter()
            .map(|p| {
                if operator == Multiply {
                    Decimal {
                        width: result_width.max(p.0),
                        scale: p.1,
                    }
                } else {
                    result.clone()
                }
            })
            .collect();
        Ok(Some(OperatorSignature {
            operator,
            arguments,
            result,
            nullable: operator == Modulo,
        }))
    }
    fn evaluate(
        &self,
        signature: &OperatorSignature,
        arguments: &[Value],
        query: &QueryContext,
    ) -> Result<Value> {
        query.check()?;
        let DataType::Decimal { width, scale } = signature.result else {
            return Err(Error::Internal("decimal result metadata".into()));
        };
        let Value::Decimal {
            value: a,
            scale: sa,
            ..
        } = arguments[0]
        else {
            return Err(Error::Internal("decimal input".into()));
        };
        let (b, sb) = match arguments.get(1) {
            Some(Value::Decimal { value, scale, .. }) => (*value, *scale),
            None => (0, 0),
            _ => return Err(Error::Internal("decimal input".into())),
        };
        use Operator::*;
        let n = match signature.operator {
            Plus => Some(a),
            Negate => a.checked_neg(),
            Add => a.checked_add(b),
            Subtract => a.checked_sub(b),
            Multiply => a.checked_mul(b),
            Modulo if b == 0 => return Err(Error::InvalidInput("Division by zero".into())),
            Modulo => a.checked_rem(b),
            _ => return Err(Error::Internal("invalid decimal operator".into())),
        }
        .ok_or_else(|| Error::OutOfRange("Overflow in decimal arithmetic".into()))?;
        let n = if signature.operator == Multiply {
            rescale(n, sa + sb, scale)?
        } else {
            n
        };
        decimal(n, width, scale)
            .map_err(|_| Error::OutOfRange("Overflow in decimal arithmetic".into()))
    }
}
