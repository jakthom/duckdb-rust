use super::{
    AggregateFunction, AggregateState, FunctionRegistry, ScalarBindArguments, ScalarFunction,
};
use crate::{
    common::{
        DataType, Error, NestedPayload, NestedType, NestedValue, Result, Value,
        type_registry::{BoundType, TypeRegistry},
    },
    parallel::QueryContext,
};
use std::{collections::BTreeSet, sync::Arc};
mod concat;
mod map;
mod variant;
pub(super) use concat::bind_concat;

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
            NestedType::Struct(_) | NestedType::Tuple(_) => {
                NestedPayload::Struct(arguments.to_vec())
            }
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
    key: Option<BoundType>,
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
        if matches!(
            self.name,
            "struct_extract" | "struct_extract_at" | "array_extract"
        ) && let [DataType::Nested(metadata), index_type] = types.as_slice()
            && (matches!(metadata.as_ref(), NestedType::Tuple(_))
                || self.name == "struct_extract_at")
        {
            if !index_type.is_integer() {
                return Err(Error::Bind(
                    "name extraction cannot be used on an unnamed struct".into(),
                ));
            }
            let fields = match metadata.as_ref() {
                NestedType::Tuple(fields) => fields.iter().collect::<Vec<_>>(),
                NestedType::Struct(fields) if self.name == "struct_extract_at" => {
                    fields.iter().map(|(_, ty)| ty).collect()
                }
                _ => {
                    return Err(Error::Bind(
                        "positional extraction requires STRUCT or TUPLE".into(),
                    ));
                }
            };
            let index = match arguments.constant(1)? {
                Value::Integer(value) => usize::try_from(value).ok(),
                Value::Unsigned(value) => usize::try_from(value).ok(),
                _ => None,
            }
            .and_then(|value| value.checked_sub(1))
            .filter(|index| *index < fields.len())
            .ok_or_else(|| Error::Bind("TUPLE index out of range".into()))?;
            return Ok(Some(Arc::new(Self {
                name: self.name,
                result: Some(fields[index].clone()),
                field: Some(index),
                key: None,
            })));
        }
        if self.name == "struct_extract" || self.name == "union_extract" {
            let [DataType::Nested(metadata), DataType::Varchar] = types.as_slice() else {
                return Err(Error::Bind(
                    "struct_extract requires STRUCT and a constant field name".into(),
                ));
            };
            let fields = match (self.name, metadata.as_ref()) {
                ("struct_extract", NestedType::Struct(fields))
                | ("union_extract", NestedType::Union(fields)) => fields,
                _ => return Err(Error::Bind("nested extraction has wrong family".into())),
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
                key: None,
            })));
        }
        let arguments = self.argument_types(&types, query.types())?;
        let result = self.return_type(&arguments, query.types())?;
        let key = if self.name == "map" {
            let DataType::Nested(metadata) = &result else {
                return Err(Error::Internal("MAP constructor metadata".into()));
            };
            let NestedType::Map { key, .. } = metadata.as_ref() else {
                return Err(Error::Internal("MAP constructor type".into()));
            };
            Some(query.types().bind(key)?)
        } else {
            None
        };
        Ok(Some(Arc::new(Self {
            name: self.name,
            result: Some(result),
            field: None,
            key,
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
            "row" => Ok(NestedType::Tuple(arguments.to_vec()).data_type()),
            "struct_values" | "struct_keys" if arguments.len() == 1 => match &arguments[0] {
                DataType::Nested(metadata) => match metadata.as_ref() {
                    NestedType::Struct(fields) => Ok(if self.name == "struct_values" {
                        NestedType::Tuple(fields.iter().map(|(_, ty)| ty.clone()).collect())
                            .data_type()
                    } else {
                        NestedType::List(DataType::Varchar).data_type()
                    }),
                    NestedType::Tuple(fields) if self.name == "struct_values" => {
                        Ok(NestedType::Tuple(fields.clone()).data_type())
                    }
                    _ => Err(Error::Bind(format!(
                        "{} expects a STRUCT argument",
                        self.name
                    ))),
                },
                _ => Err(Error::Bind(format!(
                    "{} expects a STRUCT argument",
                    self.name
                ))),
            },
            "union_tag" if arguments.len() == 1 => match &arguments[0] {
                DataType::Nested(metadata) => match metadata.as_ref() {
                    NestedType::Union(fields) => {
                        DataType::enumeration(fields.iter().map(|(name, _)| name.clone()).collect())
                    }
                    _ => Err(Error::Bind("union_tag requires UNION".into())),
                },
                _ => Err(Error::Bind("union_tag requires UNION".into())),
            },
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
        if matches!(self.name, "list_value" | "array_value" | "row") {
            return Constructor(result.clone()).evaluate(arguments, query);
        }
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if matches!(self.name, "struct_extract_at" | "array_extract")
            && let Some(field) = self.field
        {
            let Value::Nested(value) = &arguments[0] else {
                return Err(Error::Internal("TUPLE argument".into()));
            };
            let NestedPayload::Struct(values) = &value.payload else {
                return Err(Error::Internal("TUPLE payload".into()));
            };
            return values
                .get(field)
                .cloned()
                .ok_or_else(|| Error::Internal("TUPLE extraction index".into()));
        }
        match self.name {
            "struct_values" | "struct_keys" => {
                let Value::Nested(value) = &arguments[0] else {
                    return Err(Error::Internal("STRUCT argument".into()));
                };
                let NestedPayload::Struct(values) = &value.payload else {
                    return Err(Error::Internal("STRUCT payload".into()));
                };
                if self.name == "struct_values" {
                    return NestedValue::value(
                        result.clone(),
                        NestedPayload::Struct(values.clone()),
                    );
                }
                let DataType::Nested(metadata) = &value.data_type else {
                    return Err(Error::Internal("STRUCT metadata".into()));
                };
                let NestedType::Struct(fields) = metadata.as_ref() else {
                    return Err(Error::Internal("STRUCT key metadata".into()));
                };
                NestedValue::value(
                    result.clone(),
                    NestedPayload::Sequence(
                        fields
                            .iter()
                            .map(|(name, _)| Value::Varchar(name.clone()))
                            .collect(),
                    ),
                )
            }
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
            "union_extract" | "union_tag" => {
                let Value::Nested(value) = &arguments[0] else {
                    return Err(Error::Internal("UNION argument".into()));
                };
                let NestedPayload::Union { tag, value } = &value.payload else {
                    return Err(Error::Internal("UNION payload".into()));
                };
                if self.name == "union_tag" {
                    return Value::enumeration(result, *tag as u32);
                }
                Ok(if Some(*tag) == self.field {
                    value.clone()
                } else {
                    Value::Null
                })
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
                    return Err(Error::InvalidInput(
                        "MAP key and value list lengths differ".into(),
                    ));
                }
                // Constructor domain failures are InvalidInput. Keep this
                // separate from converted-key validation in NestedCast: its
                // explicit rejection provenance must remain recoverable by TRY.
                let key = self
                    .key
                    .as_ref()
                    .ok_or_else(|| Error::Internal("MAP constructor key was not bound".into()))?;
                query.check_rows(values[0].len())?;
                let mut keys = BTreeSet::new();
                for value in values[0] {
                    if value.is_null() {
                        return Err(Error::InvalidInput("Map keys can not be NULL.".into()));
                    }
                    let mut bytes = Vec::new();
                    key.append_key(value, &mut bytes, query)?;
                    if !keys.insert(bytes) {
                        return Err(Error::InvalidInput("Map keys must be unique.".into()));
                    }
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
    concat::register(registry);
    map::register(registry);
    variant::register(registry);
    for name in [
        "list_value",
        "array_value",
        "list_extract",
        "array_extract",
        "struct_extract",
        "struct_extract_at",
        "struct_values",
        "struct_keys",
        "row",
        "union_extract",
        "union_tag",
        "map",
    ] {
        registry
            .register_scalar(Arc::new(NestedFunction {
                name,
                result: None,
                field: None,
                key: None,
            }))
            .expect("unique nested function");
    }
    for name in ["list", "array_agg"] {
        registry
            .register_aggregate(Arc::new(CollectList(name)))
            .expect("unique list aggregate");
    }
}

#[derive(Debug)]
struct CollectList(&'static str);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateFunction for CollectList {
    fn name(&self) -> &str {
        self.0
    }
    fn return_type(&self, arguments: &[DataType], types: &TypeRegistry) -> Result<DataType> {
        let [child] = arguments else {
            return Err(Error::Bind("list aggregate requires one argument".into()));
        };
        types.bind(child)?;
        Ok(NestedType::List(child.clone()).data_type())
    }
    fn create_state(
        &self,
        arguments: &[DataType],
        types: &TypeRegistry,
    ) -> Result<Box<dyn AggregateState>> {
        Ok(Box::new(CollectedList {
            data_type: self.return_type(arguments, types)?,
            values: Vec::new(),
        }))
    }
}

struct CollectedList {
    data_type: DataType,
    values: Vec<Value>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateState for CollectedList {
    fn update(&mut self, arguments: &[Value], query: &QueryContext) -> Result<()> {
        query.check_rows(
            self.values
                .len()
                .checked_add(1)
                .ok_or_else(|| Error::Resource("list aggregate cardinality overflow".into()))?,
        )?;
        let [value] = arguments else {
            return Err(Error::Internal("list aggregate argument count".into()));
        };
        self.values
            .try_reserve(1)
            .map_err(|_| Error::Resource("list aggregate allocation failed".into()))?;
        self.values.push(value.clone());
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<Value> {
        if self.values.is_empty() {
            return Ok(Value::Null);
        }
        NestedValue::value(self.data_type, NestedPayload::Sequence(self.values))
    }
}
