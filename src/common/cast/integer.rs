use super::*;

/// Decimal integer parser using checked negative accumulation, including the
/// asymmetric i128 minimum. An alternative to the standard library parser used
/// by PrimitiveCast. Both currently accept trimmed, optionally signed digits.
#[derive(Debug)]
pub struct DigitIntegerCast;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for DigitIntegerCast {
    fn name(&self) -> &'static str {
        "digit-integer-cast"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar
            && spec.target.is_integer()
            && spec.mode != CastMode::Implicit
    }
    fn cast(&self, value: &Value, spec: &CastSpec, context: &QueryContext) -> Result<Value> {
        let Value::Varchar(text) = value else {
            return Err(Error::Internal("integer cast requires VARCHAR".into()));
        };
        let fail = || Error::Conversion(format!("cannot cast {text:?} to {}", spec.target));
        let text = text.trim().as_bytes();
        let negative = text.first() == Some(&b'-');
        let digits = if matches!(text.first(), Some(b'+' | b'-')) {
            &text[1..]
        } else {
            text
        };
        if digits.is_empty() {
            return Err(fail());
        }
        let mut result = 0_i128;
        for (index, &byte) in digits.iter().enumerate() {
            if index % 1024 == 0 {
                context.check()?;
            }
            if !byte.is_ascii_digit() {
                return Err(fail());
            }
            result = result
                .checked_mul(10)
                .and_then(|v| v.checked_sub(i128::from(byte - b'0')))
                .ok_or_else(&fail)?;
        }
        let result = Value::Integer(if negative {
            result
        } else {
            result.checked_neg().ok_or_else(&fail)?
        });
        if !result.fits_type(&spec.target) {
            return Err(fail());
        }
        Ok(result)
    }
}
