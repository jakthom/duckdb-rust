//! Sequence concat owns its selected child metadata; scalar concat delegates
//! only when LIST/ARRAY arguments select this overload.
use super::*;
use crate::common::{cast::CastMode, type_registry::BoundType};

#[derive(Debug)]
struct ListConcat {
    name: &'static str,
    result: BoundType,
    arity: usize,
}

#[derive(Debug)]
struct ConcatAlias(&'static str);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for ConcatAlias {
    fn name(&self) -> &str {
        self.0
    }
    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        bind_concat_family(self.0, arguments, query, true)
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Err(Error::Internal(format!(
            "{} requires selected sequence binding",
            self.0
        )))
    }
    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        Err(Error::Internal(format!(
            "{} was not specialized before execution",
            self.0
        )))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in ["list_concat", "list_cat", "array_concat", "array_cat"] {
        registry
            .register_scalar(Arc::new(ConcatAlias(name)))
            .expect("unique sequence concat alias");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn child(ty: &DataType) -> Option<&DataType> {
    match ty {
        DataType::Nested(metadata) => match metadata.as_ref() {
            NestedType::List(child) | NestedType::Array { element: child, .. } => Some(child),
            _ => None,
        },
        _ => None,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(in crate::function) fn bind_concat(
    arguments: &dyn ScalarBindArguments,
    query: &QueryContext,
) -> Result<Option<Arc<dyn ScalarFunction>>> {
    bind_concat_family("concat", arguments, query, false)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bind_concat_family(
    name: &'static str,
    arguments: &dyn ScalarBindArguments,
    query: &QueryContext,
    sequence_only: bool,
) -> Result<Option<Arc<dyn ScalarFunction>>> {
    query.check()?;
    let types = (0..arguments.len())
        .map(|index| arguments.data_type(index))
        .collect::<Result<Vec<_>>>()?;
    if !sequence_only && !types.iter().any(|ty| child(ty).is_some()) {
        return Ok(None);
    }
    let mut common = DataType::Null;
    for ty in &types {
        query.check()?;
        if *ty == DataType::Null {
            continue;
        }
        let next = child(ty).ok_or_else(|| {
            Error::Bind(format!(
                "Cannot concatenate types {types:?} - an explicit cast is required"
            ))
        })?;
        common = query.types().common_type(&common, next)?;
    }
    // The LIST-only catalog signatures differ from concat's ANY signature:
    // all untyped NULL arguments yield SQL NULL. A zero-argument alias follows
    // the development variadic binder's empty VARCHAR result; a typed NULL
    // list still selects LIST and yields an empty, non-NULL sequence.
    let result = if types.is_empty() {
        DataType::Varchar
    } else if types.iter().all(|ty| *ty == DataType::Null) {
        DataType::Null
    } else {
        NestedType::List(common).data_type()
    };
    let result = query.types().bind(&result)?;
    Ok(Some(Arc::new(ListConcat {
        name,
        result,
        arity: types.len(),
    })))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for ListConcat {
    fn name(&self) -> &str {
        self.name
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        if arguments.len() != self.arity {
            return Err(Error::Bind(format!(
                "{} argument count changed after binding",
                self.name
            )));
        }
        Ok(vec![self.result.data_type().clone(); self.arity])
    }
    fn argument_cast_mode(&self, _: usize) -> CastMode {
        // The selected common child type is fixed first; combination casts
        // (including BOOLEAN to integral children) are then explicit plan casts.
        CastMode::Explicit
    }
    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        if arguments.len() != self.arity || arguments.iter().any(|ty| ty != self.result.data_type())
        {
            return Err(Error::Bind(
                "concat arguments differ from selected list type".into(),
            ));
        }
        Ok(self.result.data_type().clone())
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if arguments.len() != self.arity {
            return Err(Error::Internal(
                "concat argument count differs from binding".into(),
            ));
        }
        let mut count = 0usize;
        for value in arguments {
            self.result.validate(value, query)?;
            if let Value::Nested(value) = value {
                let NestedPayload::Sequence(values) = &value.payload else {
                    return Err(Error::Internal("concat expected a sequence payload".into()));
                };
                count = count
                    .checked_add(values.len())
                    .filter(|n| *n < 16_777_216)
                    .ok_or_else(|| {
                        Error::Resource("concat exceeds 16 million child values".into())
                    })?;
            }
        }
        query.check_rows(count)?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(count)
            .map_err(|_| Error::Resource("cannot allocate concat child values".into()))?;
        for value in arguments {
            if let Value::Nested(value) = value {
                let NestedPayload::Sequence(values) = &value.payload else {
                    return Err(Error::Internal("concat expected a sequence payload".into()));
                };
                for (index, value) in values.iter().enumerate() {
                    if index % 1024 == 0 {
                        query.check()?;
                    }
                    result.push(value.clone());
                }
            }
        }
        let result = match self.result.data_type() {
            DataType::Null => Value::Null,
            DataType::Varchar if self.arity == 0 => Value::Varchar(String::new()),
            ty => NestedValue::value(ty.clone(), NestedPayload::Sequence(result))?,
        };
        self.result.validate(&result, query)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::type_registry::{KeyWriter, PrimitiveTypes, TypeAdapter};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct SelectedType(Arc<AtomicUsize>);
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl TypeAdapter for SelectedType {
        fn name(&self) -> &'static str {
            "selected-concat-child"
        }
        fn validate_type(&self, ty: &DataType) -> Result<()> {
            PrimitiveTypes.validate_type(ty)
        }
        fn validate_value(&self, _: &DataType, _: &Value, query: &QueryContext) -> Result<()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            query.check()
        }
        fn common_type(&self, _: &DataType, _: &DataType) -> Result<Option<DataType>> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(Some(DataType::BigInt))
        }
        fn compare(
            &self,
            ty: &DataType,
            left: &Value,
            right: &Value,
            query: &QueryContext,
        ) -> Result<std::cmp::Ordering> {
            PrimitiveTypes.compare(ty, left, right, query)
        }
        fn write_key(
            &self,
            ty: &DataType,
            value: &Value,
            output: &mut KeyWriter<'_>,
            query: &QueryContext,
        ) -> Result<()> {
            PrimitiveTypes.write_key(ty, value, output, query)
        }
    }
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
            Err(Error::Internal(
                "concat binding must not evaluate arguments".into(),
            ))
        }
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn concat_retains_selected_child_inference_and_validation() -> Result<()> {
        let calls = Arc::new(AtomicUsize::new(0));
        let unused = Arc::new(AtomicUsize::new(0));
        let mut types = TypeRegistry::builtins();
        for family in ["builtin.tinyint", "builtin.smallint", "builtin.bigint"] {
            types.replace(family, Arc::new(SelectedType(calls.clone())))?;
        }
        let args = Arguments(vec![
            NestedType::List(DataType::TinyInt).data_type(),
            NestedType::Array {
                element: DataType::SmallInt,
                length: 1,
            }
            .data_type(),
        ]);
        let query = QueryContext::background().with_types(Arc::new(types.clone()));
        let function = bind_concat(&args, &query)?.unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        let result = NestedType::List(DataType::BigInt).data_type();
        assert_eq!(
            function.argument_types(&args.0, &types)?,
            vec![result.clone(); 2]
        );
        assert_eq!(
            function.return_type(&[result.clone(), result.clone()], &types)?,
            result
        );
        types.replace("builtin.bigint", Arc::new(SelectedType(unused.clone())))?;
        let query = QueryContext::background().with_types(Arc::new(TypeRegistry::default()));
        let input = NestedValue::value(
            result.clone(),
            NestedPayload::Sequence(vec![Value::Integer(1), Value::Null]),
        )?;
        assert_eq!(
            function.evaluate(&[input.clone(), Value::Null], &query)?,
            input
        );
        assert!(calls.load(Ordering::Relaxed) > 2);
        assert_eq!(unused.load(Ordering::Relaxed), 0);
        assert!(
            function
                .evaluate(&[Value::Integer(1), Value::Null], &query)
                .is_err()
        );
        assert_eq!(
            function.evaluate(&[Value::Null, Value::Null], &query)?,
            NestedValue::value(result, NestedPayload::Sequence(vec![]))?
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn concat_aliases_retain_the_selected_catalog_identity_and_signature() -> Result<()> {
        let registry = FunctionRegistry::builtins();
        let query = QueryContext::background();
        for name in ["list_concat", "list_cat", "array_concat", "array_cat"] {
            let template = registry.scalar(name)?;
            for (types, result, value) in [
                (vec![], DataType::Varchar, Value::Varchar(String::new())),
                (vec![DataType::Null; 2], DataType::Null, Value::Null),
                (
                    vec![NestedType::List(DataType::Integer).data_type()],
                    NestedType::List(DataType::Integer).data_type(),
                    NestedValue::value(
                        NestedType::List(DataType::Integer).data_type(),
                        NestedPayload::Sequence(vec![]),
                    )?,
                ),
            ] {
                let function = template.bind(&Arguments(types.clone()), &query)?.unwrap();
                assert_eq!(function.name(), name);
                let arguments = function.argument_types(&types, query.types())?;
                assert_eq!(arguments, vec![result.clone(); types.len()]);
                assert_eq!(function.return_type(&arguments, query.types())?, result);
                let empty_registry =
                    QueryContext::background().with_types(Arc::new(TypeRegistry::default()));
                assert_eq!(
                    function.evaluate(&vec![Value::Null; types.len()], &empty_registry)?,
                    value
                );
                assert!(
                    function
                        .evaluate(&[Value::Integer(1)], &empty_registry)
                        .is_err()
                );
            }
        }
        Ok(())
    }
}
