use std::sync::Arc;

use super::{FunctionRegistry, ScalarBindArguments, ScalarFunction};
use crate::{
    common::{DataType, EnumType, Error, NestedPayload, NestedType, NestedValue, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug)]
struct EnumFunction {
    name: &'static str,
    metadata: Option<Arc<EnumType>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl EnumFunction {
    fn metadata(arguments: &[DataType], boundary: bool) -> Result<Arc<EnumType>> {
        if arguments.len() != if boundary { 2 } else { 1 } {
            return Err(Error::Bind("incorrect ENUM function arity".into()));
        }
        let mut metadata = None;
        for argument in arguments {
            match argument {
                DataType::Enum(current) => {
                    if metadata
                        .as_ref()
                        .is_some_and(|previous| previous != current)
                    {
                        return Err(Error::Bind("ENUM range requires one dictionary".into()));
                    }
                    metadata = Some(current.clone());
                }
                DataType::Null if boundary => (),
                _ => return Err(Error::Bind("this function needs an ENUM argument".into())),
            }
        }
        metadata.ok_or_else(|| Error::Bind("this function needs an ENUM argument".into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for EnumFunction {
    fn name(&self) -> &str {
        self.name
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        let arguments = (0..arguments.len())
            .map(|index| arguments.data_type(index))
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(Arc::new(Self {
            name: self.name,
            metadata: Some(Self::metadata(
                &arguments,
                self.name == "enum_range_boundary",
            )?),
        })))
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        _: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType> {
        let metadata = Self::metadata(arguments, self.name == "enum_range_boundary")?;
        Ok(match self.name {
            "enum_code" => match metadata.physical_width() {
                1 => DataType::UTinyInt,
                2 => DataType::USmallInt,
                _ => DataType::UInteger,
            },
            "enum_range" | "enum_range_boundary" => NestedType::List(DataType::Varchar).data_type(),
            _ => DataType::Varchar,
        })
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let metadata = self
            .metadata
            .as_ref()
            .ok_or_else(|| Error::Internal("ENUM function was not bound".into()))?;
        let labels = &metadata.labels;
        match self.name {
            "enum_first" => Ok(labels
                .first()
                .cloned()
                .map(Value::Varchar)
                .unwrap_or(Value::Null)),
            "enum_last" => Ok(labels
                .last()
                .cloned()
                .map(Value::Varchar)
                .unwrap_or(Value::Null)),
            "enum_code" => match arguments {
                [Value::Null] => Ok(Value::Null),
                [Value::Enum(value)] => Ok(Value::Unsigned(u128::from(value.ordinal))),
                _ => Err(Error::Internal(
                    "ENUM code input differs from binding".into(),
                )),
            },
            "enum_range" | "enum_range_boundary" => {
                let (start, end) = if self.name == "enum_range" {
                    (0, labels.len())
                } else {
                    let [start, end] = arguments else {
                        return Err(Error::Internal("ENUM boundary arity".into()));
                    };
                    let ordinal = |value: &Value, default: usize, inclusive: usize| match value {
                        Value::Null => Ok(default),
                        Value::Enum(value) => Ok(value.ordinal as usize + inclusive),
                        _ => Err(Error::Internal(
                            "ENUM boundary input differs from binding".into(),
                        )),
                    };
                    (ordinal(start, 0, 0)?, ordinal(end, labels.len(), 1)?)
                };
                let mut values = Vec::with_capacity(end.saturating_sub(start));
                for (index, label) in labels.iter().enumerate().take(end).skip(start) {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    values.push(Value::Varchar(label.clone()));
                }
                NestedValue::value(
                    NestedType::List(DataType::Varchar).data_type(),
                    NestedPayload::Sequence(values),
                )
            }
            _ => Err(Error::Internal("unknown ENUM function".into())),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in [
        "enum_first",
        "enum_last",
        "enum_code",
        "enum_range",
        "enum_range_boundary",
    ] {
        registry
            .register_scalar(Arc::new(EnumFunction {
                name,
                metadata: None,
            }))
            .expect("unique ENUM function");
    }
}
