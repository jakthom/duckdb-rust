use super::*;

#[derive(Debug)]
struct CurrentTimestamp(&'static str);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in ["get_current_timestamp", "now", "transaction_timestamp"] {
        registry
            .register_scalar(Arc::new(CurrentTimestamp(name)))
            .expect("unique current timestamp function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for CurrentTimestamp {
    fn name(&self) -> &str {
        self.0
    }

    fn argument_types(
        &self,
        arguments: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Vec<DataType>> {
        if arguments.is_empty() {
            Ok(Vec::new())
        } else {
            Err(Error::Bind(format!("{} accepts no arguments", self.0)))
        }
    }

    fn return_type(
        &self,
        arguments: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if arguments.is_empty() {
            Ok(DataType::TimestampTz)
        } else {
            Err(Error::Bind(format!("{} accepts no arguments", self.0)))
        }
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        if !arguments.is_empty() {
            return Err(Error::Internal(format!("bound {} argument count", self.0)));
        }
        Ok(Value::Temporal(TemporalValue::TimestampTz(
            query.transaction_timestamp_micros()?,
        )))
    }
}
