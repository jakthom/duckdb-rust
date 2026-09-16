//! Selected, metadata-only constructors shared by SQL syntax and stored calls.
use super::*;
use crate::common::{
    cast::CastMode,
    vector::{DataChunk, Vector, append_physical_identity},
};
use std::collections::HashMap;

#[derive(Debug)]
struct Constructor(&'static str);

#[derive(Debug)]
struct BoundConstructor {
    name: &'static str,
    result: BoundType,
    arguments: Vec<DataType>,
    modes: Vec<CastMode>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in [
        "list_value",
        "array_value",
        "row",
        "struct_pack",
        "union_value",
    ] {
        registry
            .register_scalar(Arc::new(Constructor(name)))
            .expect("unique nested constructor");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Constructor {
    fn name(&self) -> &str {
        self.0
    }
    fn accepts_named_arguments(&self) -> bool {
        matches!(self.0, "struct_pack" | "union_value")
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        if arguments.len() > 16_777_216 {
            return Err(Error::Resource("nested constructor argument limit".into()));
        }
        let mut types = (0..arguments.len())
            .map(|index| {
                query.check()?;
                arguments.data_type(index)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut modes = vec![CastMode::Implicit; types.len()];
        let metadata = match self.0 {
            "list_value" | "array_value" => {
                if self.0 == "array_value" && types.is_empty() {
                    return Err(Error::Bind(
                        "array_value requires at least one argument".into(),
                    ));
                }
                let child = if types.is_empty() {
                    DataType::Null
                } else {
                    let indices = (0..types.len()).collect::<Vec<_>>();
                    let combined = arguments.collection_combination(&indices)?;
                    combined.validate(types.len(), query.types())?;
                    modes = combined.cast_modes;
                    combined.data_type
                };
                types.fill(child.clone());
                if self.0 == "list_value" {
                    NestedType::List(child)
                } else {
                    NestedType::Array {
                        element: child,
                        length: types.len(),
                    }
                }
            }
            "row" => NestedType::Tuple(types.clone()),
            "struct_pack" | "union_value" => {
                if self.0 == "union_value" && types.len() != 1 {
                    return Err(Error::Bind(
                        "union_value requires one named argument".into(),
                    ));
                }
                let fields = types
                    .iter()
                    .enumerate()
                    .map(|(index, data_type)| {
                        query.check()?;
                        let name = match arguments.argument_name(index)? {
                            Some(name) => Some(name),
                            None => arguments.argument_alias(index)?,
                        }
                        .filter(|name| !name.is_empty())
                        .ok_or_else(|| {
                            Error::Bind(format!("{} requires named arguments", self.0))
                        })?;
                        Ok((name.to_owned(), data_type.clone()))
                    })
                    .collect::<Result<Vec<_>>>()?;
                if self.0 == "struct_pack" {
                    NestedType::Struct(fields)
                } else {
                    NestedType::Union(fields)
                }
            }
            _ => return Err(Error::Internal("nested constructor selection".into())),
        };
        let result = query.types().bind(&metadata.data_type())?;
        Ok(Some(Arc::new(BoundConstructor {
            name: self.0,
            result,
            arguments: types,
            modes,
        })))
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Err(Error::Unsupported(
            "nested constructor requires contextual binding".into(),
        ))
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        Err(Error::Internal("nested constructor was not bound".into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for BoundConstructor {
    fn name(&self) -> &str {
        self.name
    }
    fn accepts_named_arguments(&self) -> bool {
        matches!(self.name, "struct_pack" | "union_value")
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        if arguments.len() != self.arguments.len() {
            return Err(Error::Bind(
                "nested constructor arity differs from binding".into(),
            ));
        }
        Ok(self.arguments.clone())
    }
    fn argument_cast_mode(&self, index: usize) -> CastMode {
        self.modes.get(index).copied().unwrap_or(CastMode::Implicit)
    }
    fn argument_literal_coercion(&self, _: usize) -> bool {
        false
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        if arguments != self.arguments {
            return Err(Error::Bind(
                "nested constructor types differ from binding".into(),
            ));
        }
        Ok(self.result.data_type().clone())
    }
    fn is_total(&self, _: &[Option<&Value>]) -> bool {
        true
    }
    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        const MAX_DICTIONARY_VALUES: usize = 256;
        if arguments.columns().len() != self.arguments.len()
            || !arguments
                .columns()
                .iter()
                .map(Vector::data_type)
                .eq(&self.arguments)
        {
            return Err(Error::Internal(
                "nested constructor batch differs from binding".into(),
            ));
        }
        if arguments.len() < 8 {
            return Ok(None);
        }
        let limit = MAX_DICTIONARY_VALUES.min(arguments.len() / 4);
        if let Some(output) = dictionary_argument_constructor(self, arguments, limit, query)? {
            return Ok(Some(output));
        }
        let mut dictionary = HashMap::<Vec<u8>, usize>::new();
        let mut unique = Vec::new();
        let mut selection = Vec::with_capacity(arguments.len());
        let mut key = Vec::new();
        let mut row = Vec::with_capacity(arguments.columns().len());
        for index in 0..arguments.len() {
            if index % 1024 == 0 {
                query.check()?;
            }
            key.clear();
            for column in arguments.columns() {
                if !append_physical_identity(
                    column.get(index).expect("validated constructor column"),
                    &mut key,
                ) {
                    return Ok(None);
                }
            }
            if let Some(&entry) = dictionary.get(key.as_slice()) {
                selection.push(entry);
                continue;
            }
            if unique.len() == limit {
                return Ok(None);
            }
            arguments.read_row(index, &mut row)?;
            let value = self.evaluate(&row, query)?;
            let entry = unique.len();
            dictionary.insert(key.clone(), entry);
            unique.push(value);
            selection.push(entry);
        }
        query.check()?;
        if unique.len() == 1 {
            return Vector::constant(
                self.result.data_type().clone(),
                unique.pop().expect("one constructor value"),
                arguments.len(),
            )
            .map(Some);
        }
        Arc::new(Vector::flat(self.result.data_type().clone(), unique)?)
            .select(selection)
            .map(Some)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if arguments.len() != self.arguments.len() {
            return Err(Error::Internal("nested constructor argument count".into()));
        }
        query.check_rows(arguments.len())?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(arguments.len())
            .map_err(|_| Error::Resource("nested constructor allocation failed".into()))?;
        values.extend_from_slice(arguments);
        let payload = match self.name {
            "list_value" | "array_value" => NestedPayload::Sequence(values),
            "row" | "struct_pack" => NestedPayload::Struct(values),
            "union_value" => NestedPayload::Union {
                tag: 0,
                value: values.remove(0),
            },
            _ => return Err(Error::Internal("nested constructor dispatch".into())),
        };
        let result = NestedValue::value(self.result.data_type().clone(), payload)?;
        self.result.validate(&result, query)?;
        Ok(result)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Constructor arguments that already carry exact dictionary identities can
/// be combined by those identities. Payload serialization remains the fallback
/// for flat or opaque inputs.
fn dictionary_argument_constructor(
    function: &BoundConstructor,
    arguments: &DataChunk,
    limit: usize,
    query: &QueryContext,
) -> Result<Option<Vector>> {
    let mut selections = Vec::with_capacity(arguments.columns().len());
    for column in arguments.columns() {
        if column.constant_value().is_some() {
            selections.push(None);
        } else if let Some((_, selection)) = column.dictionary() {
            selections.push(Some(selection));
        } else {
            return Ok(None);
        }
    }
    let mut dictionary = HashMap::<Vec<usize>, usize>::new();
    let mut unique = Vec::new();
    let mut selection = Vec::with_capacity(arguments.len());
    let mut key = Vec::with_capacity(selections.iter().flatten().count());
    let mut row = Vec::with_capacity(arguments.columns().len());
    for index in 0..arguments.len() {
        if index % 1024 == 0 {
            query.check()?;
        }
        key.clear();
        key.extend(
            selections
                .iter()
                .filter_map(|selected| selected.map(|s| s[index])),
        );
        if let Some(&entry) = dictionary.get(key.as_slice()) {
            selection.push(entry);
            continue;
        }
        if unique.len() == limit {
            return Ok(None);
        }
        arguments.read_row(index, &mut row)?;
        let value = function.evaluate(&row, query)?;
        let entry = unique.len();
        dictionary.insert(key.clone(), entry);
        unique.push(value);
        selection.push(entry);
    }
    query.check()?;
    if unique.len() == 1 {
        return Vector::constant(
            function.result.data_type().clone(),
            unique.pop().expect("one constructor value"),
            arguments.len(),
        )
        .map(Some);
    }
    Arc::new(Vector::flat(function.result.data_type().clone(), unique)?)
        .select(selection)
        .map(Some)
}
