//! Physical content equality, distinct from SQL comparison and grouping keys.
//! In particular, nested NaNs preserve their bits and signed zeros stay distinct.
use crate::{
    common::{Error, NestedPayload, Result, Value},
    parallel::QueryContext,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn equal<'a>(
    mut left: impl Iterator<Item = &'a Value>,
    mut right: impl Iterator<Item = &'a Value>,
    query: &QueryContext,
) -> Result<bool> {
    query.check()?;
    let mut exact = Exact {
        remaining: 16_777_216,
    };
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ok(true),
            (Some(a), Some(b)) if exact.value(a, b, 0, query)? => (),
            _ => return Ok(false),
        }
    }
}

struct Exact {
    remaining: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Exact {
    fn sequence(
        &mut self,
        left: &[Value],
        right: &[Value],
        depth: usize,
        query: &QueryContext,
    ) -> Result<bool> {
        query.check()?;
        if left.len() != right.len() {
            return Ok(false);
        }
        for (a, b) in left.iter().zip(right) {
            if !self.value(a, b, depth, query)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn value(
        &mut self,
        left: &Value,
        right: &Value,
        depth: usize,
        query: &QueryContext,
    ) -> Result<bool> {
        query.check()?;
        if depth > 64 {
            return Err(Error::Resource("checkpoint value depth exceeds 64".into()));
        }
        self.remaining = self.remaining.checked_sub(1).ok_or_else(|| {
            Error::Resource("checkpoint row comparison exceeds 16 million values".into())
        })?;
        Ok(match (left, right) {
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::Double(a), Value::Double(b)) => a.to_bits() == b.to_bits(),
            (Value::Nested(a), Value::Nested(b)) => {
                if a.data_type != b.data_type {
                    return Ok(false);
                }
                match (&a.payload, &b.payload) {
                    (NestedPayload::Sequence(a), NestedPayload::Sequence(b))
                    | (NestedPayload::Struct(a), NestedPayload::Struct(b)) => {
                        self.sequence(a, b, depth + 1, query)?
                    }
                    (NestedPayload::Map(a), NestedPayload::Map(b)) => {
                        if a.len() != b.len() {
                            return Ok(false);
                        }
                        for ((ak, av), (bk, bv)) in a.iter().zip(b) {
                            if !self.value(ak, bk, depth + 1, query)?
                                || !self.value(av, bv, depth + 1, query)?
                            {
                                return Ok(false);
                            }
                        }
                        true
                    }
                    (
                        NestedPayload::Union { tag: a, value: av },
                        NestedPayload::Union { tag: b, value: bv },
                    ) => a == b && self.value(av, bv, depth + 1, query)?,
                    (
                        NestedPayload::Variant {
                            data_type: a,
                            value: av,
                        },
                        NestedPayload::Variant {
                            data_type: b,
                            value: bv,
                        },
                    ) => a == b && self.value(av, bv, depth + 1, query)?,
                    _ => false,
                }
            }
            // All remaining scalar payloads have exact derived equality. A
            // nested value against another variant returns false without
            // descending through Value's floating-point PartialEq.
            _ => left == right,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        common::{NestedType, NestedValue},
        parallel::InterruptHandle,
    };

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn exact_comparison_checks_limits_and_cancellation_without_pointer_shortcuts() -> Result<()> {
        let query = QueryContext::background();
        let value = NestedValue::value(
            NestedType::List(crate::DataType::Double).data_type(),
            NestedPayload::Sequence(vec![Value::Double(f64::NAN); 2]),
        )?;
        // Identical Arc ownership does not waive logical visit accounting.
        assert!(matches!(
            Exact { remaining: 2 }.value(&value, &value, 0, &query),
            Err(Error::Resource(_))
        ));
        assert!(matches!(
            Exact { remaining: 100 }.value(&value, &value, 65, &query),
            Err(Error::Resource(_))
        ));
        let handle = InterruptHandle::default();
        let query = QueryContext::new(handle.clone(), None, 2, 10)?;
        handle.interrupt();
        assert!(matches!(
            equal([].iter(), [].iter(), &query),
            Err(Error::Interrupted)
        ));
        Ok(())
    }
}
