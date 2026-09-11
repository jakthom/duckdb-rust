//! BIGNUM's exact SQL operations; other numeric overloads retain their selected
//! casts (for example, multiplication is a DOUBLE operation in development).
use std::sync::Arc;

use super::{
    AggregateState,
    operator::{Operator, OperatorFunction, OperatorRegistry, OperatorSignature},
};
use crate::{
    common::{BignumValue, DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
pub struct BignumArithmetic;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl OperatorFunction for BignumArithmetic {
    fn name(&self) -> &'static str {
        "magnitude-limb-bignum-arithmetic"
    }
    fn supports(&self, signature: &OperatorSignature) -> bool {
        matches!(
            signature.operator,
            Operator::Negate | Operator::Add | Operator::Subtract
        ) && signature.arguments.len() == signature.operator.arity()
            && signature.arguments.iter().all(|t| *t == DataType::Bignum)
            && signature.result == DataType::Bignum
            && !signature.nullable
    }
    fn evaluate(
        &self,
        signature: &OperatorSignature,
        arguments: &[Value],
        query: &QueryContext,
    ) -> Result<Value> {
        query.check()?;
        let Value::Bignum(left) = &arguments[0] else {
            return Err(Error::Internal("BIGNUM operator left input".into()));
        };
        if signature.operator == Operator::Negate {
            return Ok(left.negated().value());
        }
        let Value::Bignum(right) = &arguments[1] else {
            return Err(Error::Internal("BIGNUM operator right input".into()));
        };
        match signature.operator {
            Operator::Add => left.add(right, || query.check()),
            Operator::Subtract => left.add(&right.negated(), || query.check()),
            _ => return Err(Error::Internal("invalid BIGNUM operator".into())),
        }
        .map(BignumValue::value)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register_operators(registry: &mut OperatorRegistry) {
    for operator in [Operator::Negate, Operator::Add, Operator::Subtract] {
        registry
            .register(
                OperatorSignature {
                    operator,
                    arguments: vec![DataType::Bignum; operator.arity()],
                    result: DataType::Bignum,
                    nullable: false,
                },
                Arc::new(BignumArithmetic),
            )
            .expect("unique BIGNUM operator");
    }
}

#[derive(Default)]
pub struct BignumSum {
    value: Option<Arc<BignumValue>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateState for BignumSum {
    fn update(&mut self, arguments: &[Value], query: &QueryContext) -> Result<()> {
        query.check()?;
        let [argument] = arguments else {
            return Err(Error::Internal("BIGNUM SUM argument count".into()));
        };
        let value = match argument {
            Value::Null => return Ok(()),
            Value::Bignum(value) => value,
            _ => return Err(Error::Internal("BIGNUM SUM argument type".into())),
        };
        let zero = BignumValue::from_u128(0);
        // Initial positive zero intentionally normalizes a sole negative-zero
        // input, matching the reference aggregate rather than scalar addition.
        self.value = Some(Arc::new(
            self.value
                .as_deref()
                .unwrap_or(&zero)
                .add(value, || query.check())?,
        ));
        Ok(())
    }
    fn update_column(
        &mut self,
        column: &crate::common::vector::Vector,
        query: &QueryContext,
    ) -> Result<()> {
        for value in column.values() {
            self.update(std::slice::from_ref(value), query)?;
        }
        query.check()
    }
    fn finish(self: Box<Self>) -> Result<Value> {
        Ok(self.value.map(Value::Bignum).unwrap_or(Value::Null))
    }
}
