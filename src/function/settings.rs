use super::*;

#[derive(Debug)]
struct CurrentSetting;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    registry
        .register_scalar(Arc::new(CurrentSetting))
        .expect("unique current_setting function");
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for CurrentSetting {
    fn name(&self) -> &str {
        "current_setting"
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        if arguments.len() != 1 || arguments.data_type(0)? != DataType::Varchar {
            return Err(Error::Bind(
                "current_setting requires one constant VARCHAR name".into(),
            ));
        }
        let Value::Varchar(name) = arguments.constant(0)? else {
            return Err(Error::Bind(
                "current_setting requires a non-NULL name".into(),
            ));
        };
        if name.is_empty() {
            return Err(Error::Parse(
                "Key name for current_setting must not be empty".into(),
            ));
        }
        let value = query.settings().get(&name, query)?.clone();
        let data_type = query
            .settings()
            .registry()
            .definition(&name)?
            .data_type
            .clone();
        Ok(Some(Arc::new(BoundSetting { value, data_type })))
    }
    fn return_type(
        &self,
        _: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        Err(Error::Bind(
            "current_setting requires contextual function binding".into(),
        ))
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        Err(Error::Internal("unbound current_setting function".into()))
    }
}

#[derive(Debug)]
struct BoundSetting {
    value: Value,
    data_type: DataType,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for BoundSetting {
    fn name(&self) -> &str {
        "current_setting"
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        if arguments == [DataType::Varchar] {
            Ok(self.data_type.clone())
        } else {
            Err(Error::Bind(
                "current_setting requires one VARCHAR argument".into(),
            ))
        }
    }
    fn evaluate(&self, _: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        Ok(self.value.clone())
    }
}
