use super::*;
use crate::common::variant::Node;

#[derive(Debug, Clone)]
enum Path {
    Root,
    Field(String),
    Index(usize),
    Many(Vec<String>),
}

#[derive(Debug)]
struct VariantFunction {
    name: &'static str,
    arguments: Option<Vec<DataType>>,
    result: Option<DataType>,
    path: Path,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl VariantFunction {
    fn resolve<'a>(&self, node: Node<'a>, path: &Path) -> Result<Option<Node<'a>>> {
        let node = node.resolved()?;
        match path {
            Path::Root => Ok(Some(node)),
            Path::Field(name) if node.rank()? == 15 => Ok(node
                .object()?
                .into_iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value)),
            Path::Index(index) if node.rank()? == 14 => Ok(node.array()?.get(*index).copied()),
            _ => Ok(None),
        }
    }
    fn evaluate_node(&self, node: Option<Node<'_>>, query: &QueryContext) -> Result<Value> {
        query.check()?;
        if self.name == "variant_exists" {
            return Ok(Value::Boolean(node.is_some()));
        }
        let Some(node) = node else {
            return Ok(Value::Null);
        };
        match self.name {
            "variant_extract" => node.owned(),
            "variant_typeof" => Ok(Value::Varchar(node.type_name()?)),
            "variant_type" => Ok(Value::Varchar(
                node.type_name()?
                    .split('(')
                    .next()
                    .unwrap_or("VARIANT_NULL")
                    .to_owned(),
            )),
            "variant_array_length" => Ok(Value::Unsigned(if node.rank()? == 14 {
                node.array()?.len() as u128
            } else {
                0
            })),
            "variant_keys" => {
                let mut keys = if node.rank()? == 15 {
                    node.object()?
                        .into_iter()
                        .map(|(key, _)| key.to_owned())
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                query.check_rows(keys.len())?;
                keys.sort();
                NestedValue::value(
                    NestedType::List(DataType::Varchar).data_type(),
                    NestedPayload::Sequence(keys.into_iter().map(Value::Varchar).collect()),
                )
            }
            _ => Err(Error::Internal("VARIANT function name".into())),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for VariantFunction {
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
        if let Some(first) = actual.first()
            && first.family() != "builtin.variant"
            && *first != DataType::Null
            && !arguments.is_string_literal(0)?
        {
            return Err(Error::Bind(format!(
                "{} requires a VARIANT argument",
                self.name
            )));
        }
        let required = if matches!(self.name, "variant_extract" | "variant_exists") {
            2
        } else {
            1
        };
        if actual.len() < required
            || actual.len() > if self.name == "variant_typeof" { 1 } else { 2 }
        {
            return Err(Error::Bind(format!("invalid {} arity", self.name)));
        }
        let mut targets = actual.clone();
        targets[0] = NestedType::Variant.data_type();
        let path = if actual.len() == 1 {
            Path::Root
        } else {
            let constant = arguments.constant(1)?;
            match constant {
                Value::Varchar(path) => {
                    if path.is_empty() && self.name != "variant_extract" {
                        Path::Root
                    } else {
                        Path::Field(path)
                    }
                }
                Value::Integer(index) if self.name == "variant_extract" => {
                    let index = u32::try_from(index)
                        .map_err(|_| Error::Bind("VARIANT index must fit UINTEGER".into()))?;
                    if index == 0 {
                        return Err(Error::Bind("VARIANT ARRAY indexes are 1-based".into()));
                    }
                    Path::Index(index as usize - 1)
                }
                Value::Unsigned(index) if self.name == "variant_extract" => {
                    let index = u32::try_from(index)
                        .map_err(|_| Error::Bind("VARIANT index must fit UINTEGER".into()))?;
                    if index == 0 {
                        return Err(Error::Bind("VARIANT ARRAY indexes are 1-based".into()));
                    }
                    Path::Index(index as usize - 1)
                }
                Value::Nested(value) if self.name != "variant_extract" => {
                    let NestedPayload::Sequence(paths) = &value.payload else {
                        return Err(Error::Bind("VARIANT paths require VARCHAR[]".into()));
                    };
                    let paths = paths
                        .iter()
                        .map(|value| match value {
                            Value::Varchar(value) => Ok(value.clone()),
                            _ => Err(Error::Bind("VARIANT paths require non-NULL strings".into())),
                        })
                        .collect::<Result<Vec<_>>>()?;
                    targets[1] = NestedType::List(DataType::Varchar).data_type();
                    Path::Many(paths)
                }
                _ => {
                    return Err(Error::Bind(
                        "VARIANT path requires a non-NULL constant of the declared path type"
                            .into(),
                    ));
                }
            }
        };
        let base = match self.name {
            "variant_extract" => NestedType::Variant.data_type(),
            "variant_type" | "variant_typeof" => DataType::Varchar,
            "variant_keys" => NestedType::List(DataType::Varchar).data_type(),
            "variant_array_length" => DataType::UBigInt,
            "variant_exists" => DataType::Boolean,
            _ => return Err(Error::Internal("VARIANT function binding".into())),
        };
        let result = if matches!(&path, Path::Many(_)) {
            NestedType::List(base).data_type()
        } else {
            base
        };
        query.types().bind(&result)?;
        Ok(Some(Arc::new(Self {
            name: self.name,
            arguments: Some(targets),
            result: Some(result),
            path,
        })))
    }
    fn argument_types(&self, _: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        self.arguments
            .clone()
            .ok_or_else(|| Error::Internal("unbound VARIANT function".into()))
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        self.result
            .clone()
            .ok_or_else(|| Error::Internal("unbound VARIANT function".into()))
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if arguments[0].is_null() {
            return Ok(if self.name == "variant_typeof" {
                Value::Varchar("VARIANT_NULL".into())
            } else {
                Value::Null
            });
        }
        let ty = NestedType::Variant.data_type();
        let node = Node::Typed(&ty, &arguments[0]);
        if let Path::Many(paths) = &self.path {
            let values = paths
                .iter()
                .map(|path| {
                    let path = if path.is_empty() {
                        Path::Root
                    } else {
                        Path::Field(path.clone())
                    };
                    self.evaluate_node(self.resolve(node, &path)?, query)
                })
                .collect::<Result<_>>()?;
            return NestedValue::value(
                self.result
                    .clone()
                    .ok_or_else(|| Error::Internal("VARIANT result metadata".into()))?,
                NestedPayload::Sequence(values),
            );
        }
        self.evaluate_node(self.resolve(node, &self.path)?, query)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in [
        "variant_typeof",
        "variant_extract",
        "variant_type",
        "variant_keys",
        "variant_array_length",
        "variant_exists",
    ] {
        registry
            .register_scalar(Arc::new(VariantFunction {
                name,
                arguments: None,
                result: None,
                path: Path::Root,
            }))
            .expect("unique VARIANT scalar function");
    }
}
