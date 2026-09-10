//! MAP access retains key semantics selected at binding, including nested keys.
use super::*;
use crate::common::type_registry::BoundType;

#[derive(Debug)]
struct MapFunction {
    name: &'static str,
    arguments: Option<Vec<DataType>>,
    result: Option<DataType>,
    key: Option<BoundType>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn metadata(arguments: &[DataType]) -> Result<(&DataType, &DataType)> {
    match arguments.first() {
        Some(DataType::Nested(metadata)) => match metadata.as_ref() {
            NestedType::Map { key, value } => Ok((key, value)),
            _ => Err(Error::Bind("MAP function requires MAP".into())),
        },
        _ => Err(Error::Bind("MAP function requires MAP".into())),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for MapFunction {
    fn name(&self) -> &str {
        self.name
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        let actual = (0..arguments.len())
            .map(|i| arguments.data_type(i))
            .collect::<Result<Vec<_>>>()?;
        let mut inferred = actual.clone();
        if arguments.len() == 2 && arguments.is_string_literal(1)? {
            // DuckDB's string literal can adopt the inferred template key;
            // an ordinary VARCHAR column may not take this conversion path.
            inferred[1] = metadata(&actual)?.0.clone();
        }
        let targets = self.argument_types(&inferred, query.types())?;
        let (key, _) = metadata(&targets)?;
        Ok(Some(Arc::new(Self {
            name: self.name,
            result: Some(self.return_type(&targets, query.types())?),
            key: Some(query.types().bind(key)?),
            arguments: Some(targets),
        })))
    }
    fn argument_types(
        &self,
        arguments: &[DataType],
        types: &TypeRegistry,
    ) -> Result<Vec<DataType>> {
        if let Some(targets) = &self.arguments {
            return Ok(targets.clone());
        }
        let (key, value) = metadata(arguments)?;
        let arity = if matches!(
            self.name,
            "map_extract" | "element_at" | "map_extract_value" | "map_contains"
        ) {
            2
        } else {
            1
        };
        if arguments.len() != arity {
            return Err(Error::Bind(format!(
                "{} requires {arity} arguments",
                self.name
            )));
        }
        if arity == 2 {
            let key = types.common_type(key, &arguments[1])?;
            Ok(vec![
                NestedType::Map {
                    key: key.clone(),
                    value: value.clone(),
                }
                .data_type(),
                key,
            ])
        } else {
            Ok(arguments.to_vec())
        }
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        if let Some(result) = &self.result {
            return Ok(result.clone());
        }
        let (key, value) = metadata(arguments)?;
        Ok(match self.name {
            "map_extract" | "element_at" | "map_values" => {
                NestedType::List(value.clone()).data_type()
            }
            "map_extract_value" => value.clone(),
            "map_contains" => DataType::Boolean,
            "map_keys" => NestedType::List(key.clone()).data_type(),
            "cardinality" => DataType::UBigInt,
            "map_entries" => NestedType::List(
                NestedType::Struct(vec![
                    ("key".into(), key.clone()),
                    ("value".into(), value.clone()),
                ])
                .data_type(),
            )
            .data_type(),
            _ => return Err(Error::Internal("MAP function name".into())),
        })
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let result = self
            .result
            .as_ref()
            .ok_or_else(|| Error::Internal("unbound MAP function".into()))?;
        if arguments[0].is_null() {
            return Ok(Value::Null);
        }
        let Value::Nested(map) = &arguments[0] else {
            return Err(Error::Internal("MAP function payload".into()));
        };
        let NestedPayload::Map(entries) = &map.payload else {
            return Err(Error::Internal("MAP entries payload".into()));
        };
        query.check_rows(entries.len())?;
        if self.name == "cardinality" {
            return Ok(Value::Unsigned(entries.len() as u128));
        }
        if matches!(self.name, "map_keys" | "map_values" | "map_entries") {
            let mut values = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                query.check()?;
                values.push(match self.name {
                    "map_keys" => key.clone(),
                    "map_values" => value.clone(),
                    _ => {
                        let DataType::Nested(list) = result else {
                            return Err(Error::Internal("MAP entries type".into()));
                        };
                        let NestedType::List(child) = list.as_ref() else {
                            return Err(Error::Internal("MAP entries LIST".into()));
                        };
                        NestedValue::value(
                            child.clone(),
                            NestedPayload::Struct(vec![key.clone(), value.clone()]),
                        )?
                    }
                });
            }
            return NestedValue::value(result.clone(), NestedPayload::Sequence(values));
        }
        let list = matches!(self.name, "map_extract" | "element_at");
        let mut found = None;
        if !arguments[1].is_null() {
            let key = self
                .key
                .as_ref()
                .ok_or_else(|| Error::Internal("MAP key binding".into()))?;
            for (entry, value) in entries {
                query.check()?;
                if key.compare(entry, &arguments[1], query)? == std::cmp::Ordering::Equal {
                    found = Some(value.clone());
                    break;
                }
            }
        } else if !list {
            return Ok(Value::Null);
        }
        if list {
            NestedValue::value(
                result.clone(),
                NestedPayload::Sequence(found.into_iter().collect()),
            )
        } else if self.name == "map_contains" {
            Ok(Value::Boolean(found.is_some()))
        } else {
            Ok(found.unwrap_or(Value::Null))
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in [
        "map_extract",
        "element_at",
        "map_extract_value",
        "map_contains",
        "map_keys",
        "map_values",
        "map_entries",
        "cardinality",
    ] {
        registry
            .register_scalar(Arc::new(MapFunction {
                name,
                arguments: None,
                result: None,
                key: None,
            }))
            .expect("unique MAP function");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::type_registry::ascii::{self, MaterializedAscii};

    struct Arguments(Vec<DataType>);
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl ScalarBindArguments for Arguments {
        fn len(&self) -> usize {
            self.0.len()
        }
        fn data_type(&self, index: usize) -> Result<DataType> {
            Ok(self.0[index].clone())
        }
        fn constant(&self, _: usize) -> Result<Value> {
            Err(Error::Bind("not a constant".into()))
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn map_lookup_retains_selected_extension_key_without_ambient_registry_fallback() -> Result<()> {
        let mut types = TypeRegistry::builtins();
        types.register(ascii::FAMILY, Arc::new(MaterializedAscii))?;
        let ty = ascii::data_type(16)?;
        let map = NestedType::Map {
            key: ty.clone(),
            value: DataType::Integer,
        }
        .data_type();
        let query = QueryContext::background().with_types(Arc::new(types));
        let function = MapFunction {
            name: "map_extract_value",
            arguments: None,
            result: None,
            key: None,
        };
        let bound = function
            .bind(&Arguments(vec![map.clone(), ty.clone()]), &query)?
            .expect("specialized MAP lookup");
        let entries = NestedValue::value(
            map,
            NestedPayload::Map(vec![(
                Value::extension(ty.clone(), b"A".to_vec()),
                Value::Integer(4),
            )]),
        )?;
        // The execution registry has no ASCII family at all. Retained selected
        // key semantics, not a fresh built-in lookup, must perform comparison.
        assert_eq!(
            bound.evaluate(
                &[entries, Value::extension(ty, b"a".to_vec())],
                &QueryContext::background()
            )?,
            Value::Integer(4)
        );
        Ok(())
    }
}
