use std::sync::Arc;

use super::{AggregateFunction, AggregateState, FunctionRegistry};
use crate::common::{DataType, Error, Result, Value};

#[derive(Debug)]
struct Builtin(&'static str);

pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in [
        "count", "sum", "avg", "min", "max", "first", "last", "bool_and", "bool_or",
    ] {
        registry
            .register_aggregate(Arc::new(Builtin(name)))
            .expect("unique builtin name");
    }
}

impl AggregateFunction for Builtin {
    fn name(&self) -> &str {
        self.0
    }
    fn return_type(
        &self,
        args: &[DataType],
        _types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if self.0 == "count" && args.len() <= 1 {
            return Ok(DataType::BigInt);
        }
        if args.len() != 1 {
            return Err(Error::Bind(format!("{} requires one argument", self.0)));
        }
        match self.0 {
            "sum" | "avg" if args[0].is_numeric() || args[0] == DataType::Null => {
                Ok(if self.0 == "avg" || args[0].is_floating() {
                    DataType::Double
                } else {
                    DataType::HugeInt
                })
            }
            "min" | "max" | "first" | "last" => Ok(args[0].clone()),
            "bool_and" | "bool_or" if matches!(args[0], DataType::Boolean | DataType::Null) => {
                Ok(DataType::Boolean)
            }
            _ => Err(Error::Bind(format!("no overload for {}({args:?})", self.0))),
        }
    }
    fn create_state(
        &self,
        args: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Box<dyn AggregateState>> {
        Ok(Box::new(State {
            name: self.0,
            data_type: self.return_type(args, types)?,
            count: 0,
            value: Value::Null,
            seen: false,
        }))
    }
}

struct State {
    name: &'static str,
    data_type: DataType,
    count: i128,
    value: Value,
    seen: bool,
}

impl AggregateState for State {
    fn update(&mut self, args: &[Value], context: &crate::parallel::QueryContext) -> Result<()> {
        let value = args.first().cloned().unwrap_or(Value::Integer(1));
        if self.name == "first" {
            if !self.seen {
                self.value = value;
                self.seen = true;
            }
            return Ok(());
        }
        if self.name == "last" {
            self.value = value;
            return Ok(());
        }
        if value.is_null() {
            return Ok(());
        }
        self.count = self
            .count
            .checked_add(1)
            .ok_or_else(|| Error::Execution("aggregate count overflow".into()))?;
        match self.name {
            "count" => {}
            "sum" | "avg" => {
                let value = if self.data_type == DataType::Double {
                    Value::Double(value.as_f64()?)
                } else {
                    Value::Integer(value.as_i128()?)
                };
                self.value = match (&self.value, value) {
                    (Value::Null, v) => v,
                    (Value::Integer(a), Value::Integer(b)) => Value::Integer(
                        a.checked_add(b)
                            .ok_or_else(|| Error::Execution("sum overflow".into()))?,
                    ),
                    (Value::Double(a), Value::Double(b)) => Value::Double(a + b),
                    _ => return Err(Error::Internal("aggregate type mismatch".into())),
                };
            }
            "min" | "max" => {
                if self.value.is_null()
                    || (self.name == "min"
                        && context
                            .types()
                            .bind(&self.data_type)?
                            .compare(&value, &self.value, context)?
                            .is_lt())
                    || (self.name == "max"
                        && context
                            .types()
                            .bind(&self.data_type)?
                            .compare(&value, &self.value, context)?
                            .is_gt())
                {
                    self.value = value;
                }
            }
            "bool_and" | "bool_or" => {
                let right = value.as_bool()?.unwrap_or(false);
                self.value = Value::Boolean(match self.value.as_bool()? {
                    None => right,
                    Some(left) if self.name == "bool_and" => left && right,
                    Some(left) => left || right,
                });
            }
            _ => return Err(Error::Internal("unknown aggregate".into())),
        }
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<Value> {
        match self.name {
            "count" => Ok(Value::Integer(self.count)),
            "avg" if self.count > 0 => Ok(Value::Double(self.value.as_f64()? / self.count as f64)),
            _ => Ok(self.value),
        }
    }
}
