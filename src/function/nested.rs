use super::{FunctionRegistry, ScalarBindArguments, ScalarFunction};
use crate::{
    common::{
        DataType, Error, NestedPayload, NestedType, NestedValue, Result, Value,
        type_registry::TypeRegistry,
    },
    parallel::QueryContext,
};
use std::sync::Arc;

#[derive(Debug)]
pub struct Constructor(pub DataType);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Constructor {
    fn name(&self) -> &str {
        "nested_constructor"
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(self.0.clone())
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let DataType::Nested(metadata) = &self.0 else {
            return Err(Error::Internal("constructor metadata".into()));
        };
        let payload = match metadata.as_ref() {
            NestedType::List(_) | NestedType::Array { .. } => {
                NestedPayload::Sequence(arguments.to_vec())
            }
            NestedType::Struct(_) => NestedPayload::Struct(arguments.to_vec()),
            NestedType::Union(_) if arguments.len() == 1 => NestedPayload::Union {
                tag: 0,
                value: arguments[0].clone(),
            },
            _ => return Err(Error::Internal("constructor payload".into())),
        };
        NestedValue::value(self.0.clone(), payload)
    }
}

#[derive(Debug)]
struct NestedFunction {
    name: &'static str,
    result: Option<DataType>,
    field: Option<usize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn accessor(
    input: &DataType,
    field: Option<&str>,
) -> Result<(Arc<dyn ScalarFunction>, DataType)> {
    let DataType::Nested(metadata) = input else {
        return Err(Error::Bind(
            "nested accessor requires a nested value".into(),
        ));
    };
    let (name, result, index) = match (metadata.as_ref(), field) {
        (NestedType::Struct(fields), Some(name)) => {
            let index = fields
                .iter()
                .position(|(field, _)| field.eq_ignore_ascii_case(name))
                .ok_or_else(|| Error::Bind(format!("STRUCT has no field {name}")))?;
            ("struct_extract", fields[index].1.clone(), Some(index))
        }
        (NestedType::List(child) | NestedType::Array { element: child, .. }, None) => {
            ("list_extract", child.clone(), None)
        }
        _ => return Err(Error::Bind("invalid nested accessor".into())),
    };
    Ok((
        Arc::new(NestedFunction {
            name,
            result: Some(result.clone()),
            field: index,
        }),
        result,
    ))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for NestedFunction {
    fn name(&self) -> &str {
        self.name
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        let types = (0..arguments.len())
            .map(|index| arguments.data_type(index))
            .collect::<Result<Vec<_>>>()?;
        if self.name == "struct_extract" {
            let [DataType::Nested(metadata), DataType::Varchar] = types.as_slice() else {
                return Err(Error::Bind(
                    "struct_extract requires STRUCT and a constant field name".into(),
                ));
            };
            let NestedType::Struct(fields) = metadata.as_ref() else {
                return Err(Error::Bind("struct_extract requires STRUCT".into()));
            };
            let Value::Varchar(name) = arguments.constant(1)? else {
                return Err(Error::Bind("STRUCT field must be a string".into()));
            };
            let index = fields
                .iter()
                .position(|(field, _)| field.eq_ignore_ascii_case(&name))
                .ok_or_else(|| Error::Bind(format!("STRUCT has no field {name}")))?;
            return Ok(Some(Arc::new(Self {
                name: self.name,
                result: Some(fields[index].1.clone()),
                field: Some(index),
            })));
        }
        let arguments = self.argument_types(&types, query.types())?;
        Ok(Some(Arc::new(Self {
            name: self.name,
            result: Some(self.return_type(&arguments, query.types())?),
            field: None,
        })))
    }
    fn argument_types(
        &self,
        arguments: &[DataType],
        types: &TypeRegistry,
    ) -> Result<Vec<DataType>> {
        if matches!(self.name, "list_value" | "array_value") {
            let child = arguments
                .iter()
                .try_fold(DataType::Null, |a, b| types.common_type(&a, b))?;
            return Ok(vec![child; arguments.len()]);
        }
        if matches!(self.name, "list_extract" | "array_extract") && arguments.len() == 2 {
            return Ok(vec![arguments[0].clone(), DataType::BigInt]);
        }
        Ok(arguments.to_vec())
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        if let Some(result) = &self.result {
            return Ok(result.clone());
        }
        match self.name {
            "list_value" => Ok(NestedType::List(
                arguments.first().cloned().unwrap_or(DataType::Null),
            )
            .data_type()),
            "array_value" if !arguments.is_empty() => Ok(NestedType::Array {
                element: arguments[0].clone(),
                length: arguments.len(),
            }
            .data_type()),
            "list_extract" | "array_extract" if arguments.len() == 2 => match &arguments[0] {
                DataType::Nested(metadata) => match metadata.as_ref() {
                    NestedType::List(child) | NestedType::Array { element: child, .. } => {
                        Ok(child.clone())
                    }
                    _ => Err(Error::Bind("list_extract requires LIST or ARRAY".into())),
                },
                _ => Err(Error::Bind("list_extract requires LIST or ARRAY".into())),
            },
            "map" if arguments.len() == 2 => {
                let children = arguments
                    .iter()
                    .map(|ty| match ty {
                        DataType::Nested(metadata) => match metadata.as_ref() {
                            NestedType::List(child) | NestedType::Array { element: child, .. } => {
                                Ok(child.clone())
                            }
                            _ => Err(Error::Bind("map requires lists".into())),
                        },
                        _ => Err(Error::Bind("map requires lists".into())),
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(NestedType::Map {
                    key: children[0].clone(),
                    value: children[1].clone(),
                }
                .data_type())
            }
            _ => Err(Error::Bind(format!("invalid {} arguments", self.name))),
        }
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let result = self
            .result
            .as_ref()
            .ok_or_else(|| Error::Internal("nested function was not bound".into()))?;
        if matches!(self.name, "list_value" | "array_value") {
            return Constructor(result.clone()).evaluate(arguments, query);
        }
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        match self.name {
            "list_extract" | "array_extract" => {
                let Value::Nested(value) = &arguments[0] else {
                    return Err(Error::Internal("list argument".into()));
                };
                let NestedPayload::Sequence(values) = &value.payload else {
                    return Err(Error::Internal("list payload".into()));
                };
                let index = arguments[1].as_i128()?;
                let index = if index > 0 {
                    index - 1
                } else if index < 0 {
                    values.len() as i128 + index
                } else {
                    -1
                };
                Ok(usize::try_from(index)
                    .ok()
                    .and_then(|index| values.get(index))
                    .cloned()
                    .unwrap_or(Value::Null))
            }
            "struct_extract" => {
                let Value::Nested(value) = &arguments[0] else {
                    return Err(Error::Internal("struct argument".into()));
                };
                let NestedPayload::Struct(values) = &value.payload else {
                    return Err(Error::Internal("struct payload".into()));
                };
                values
                    .get(
                        self.field
                            .ok_or_else(|| Error::Internal("struct field".into()))?,
                    )
                    .cloned()
                    .ok_or_else(|| Error::Internal("struct field index".into()))
            }
            "map" => {
                let values = arguments
                    .iter()
                    .map(|value| match value {
                        Value::Nested(value) => match &value.payload {
                            NestedPayload::Sequence(values) => Ok(values),
                            _ => Err(Error::Internal("map list payload".into())),
                        },
                        _ => Err(Error::Internal("map list argument".into())),
                    })
                    .collect::<Result<Vec<_>>>()?;
                if values[0].len() != values[1].len() {
                    return Err(Error::Conversion(
                        "MAP key and value list lengths differ".into(),
                    ));
                }
                NestedValue::value(
                    result.clone(),
                    NestedPayload::Map(
                        values[0]
                            .iter()
                            .cloned()
                            .zip(values[1].iter().cloned())
                            .collect(),
                    ),
                )
            }
            _ => Err(Error::Internal("nested function dispatch".into())),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in [
        "list_value",
        "array_value",
        "list_extract",
        "array_extract",
        "struct_extract",
        "map",
    ] {
        registry
            .register_scalar(Arc::new(NestedFunction {
                name,
                result: None,
                field: None,
            }))
            .expect("unique nested function");
    }
}
