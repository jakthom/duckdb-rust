//! Non-lambda LIST/ARRAY search, selection, resize, and reversal mechanics.
use super::*;
use crate::common::{cast::CastMode, type_registry::BoundType};

const MAX_SEQUENCE_CHILDREN: usize = 16_777_216;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SequenceOperation {
    Contains,
    Position,
    Select,
    Resize,
    Reverse,
}

#[derive(Debug)]
struct SequenceFunction {
    name: &'static str,
    operation: SequenceOperation,
}

#[derive(Debug)]
struct BoundSequenceFunction {
    name: &'static str,
    operation: SequenceOperation,
    arguments: Vec<BoundType>,
    modes: Vec<CastMode>,
    result: BoundType,
    child: Option<BoundType>,
}

struct SequenceBinding {
    arguments: Vec<DataType>,
    modes: Vec<CastMode>,
    result: DataType,
    child: Option<DataType>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for (name, operation) in [
        ("list_contains", SequenceOperation::Contains),
        ("list_has", SequenceOperation::Contains),
        ("list_position", SequenceOperation::Position),
        ("list_indexof", SequenceOperation::Position),
        ("array_position", SequenceOperation::Position),
        ("array_indexof", SequenceOperation::Position),
        ("list_select", SequenceOperation::Select),
        ("array_select", SequenceOperation::Select),
        ("list_resize", SequenceOperation::Resize),
        ("list_reverse", SequenceOperation::Reverse),
        ("array_reverse", SequenceOperation::Reverse),
    ] {
        registry
            .register_scalar(Arc::new(SequenceFunction { name, operation }))
            .expect("unique sequence operation");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for SequenceFunction {
    fn name(&self) -> &str {
        self.name
    }

    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        let actual = (0..arguments.len())
            .map(|index| arguments.data_type(index))
            .collect::<Result<Vec<_>>>()?;
        let binding = match self.operation {
            SequenceOperation::Contains | SequenceOperation::Position => bind_search(
                arguments,
                &actual,
                self.operation == SequenceOperation::Position,
                query,
            )?,
            SequenceOperation::Select => bind_select(&actual, query)?,
            SequenceOperation::Resize => bind_resize(arguments, &actual, query)?,
            SequenceOperation::Reverse => bind_reverse(&actual, query)?,
        };
        let bound_arguments = binding
            .arguments
            .iter()
            .map(|data_type| query.types().bind(data_type))
            .collect::<Result<Vec<_>>>()?;
        Ok(Some(Arc::new(BoundSequenceFunction {
            name: self.name,
            operation: self.operation,
            arguments: bound_arguments,
            modes: binding.modes,
            result: query.types().bind(&binding.result)?,
            child: binding
                .child
                .as_ref()
                .map(|data_type| query.types().bind(data_type))
                .transpose()?,
        })))
    }

    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Err(Error::Internal(format!(
            "{} requires contextual sequence binding",
            self.name
        )))
    }

    fn evaluate(&self, _: &[Value], _: &QueryContext) -> Result<Value> {
        Err(Error::Internal(format!(
            "{} was not specialized before execution",
            self.name
        )))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for BoundSequenceFunction {
    fn name(&self) -> &str {
        self.name
    }

    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        if arguments.len() != self.arguments.len() {
            return Err(Error::Bind(format!(
                "{} argument count changed after binding",
                self.name
            )));
        }
        Ok(self
            .arguments
            .iter()
            .map(|argument| argument.data_type().clone())
            .collect())
    }

    fn argument_cast_mode(&self, index: usize) -> CastMode {
        self.modes.get(index).copied().unwrap_or(CastMode::Implicit)
    }

    fn argument_literal_coercion(&self, _: usize) -> bool {
        false
    }

    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        if arguments.len() != self.arguments.len()
            || arguments
                .iter()
                .zip(&self.arguments)
                .any(|(actual, expected)| actual != expected.data_type())
        {
            return Err(Error::Bind(format!(
                "{} argument types differ from binding",
                self.name
            )));
        }
        Ok(self.result.data_type().clone())
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if arguments.len() != self.arguments.len() {
            return Err(Error::Internal(format!(
                "{} argument count differs from binding",
                self.name
            )));
        }
        for (argument, value) in self.arguments.iter().zip(arguments) {
            argument.validate(value, query)?;
        }
        let result = match self.operation {
            SequenceOperation::Contains => self.search(arguments, false, query)?,
            SequenceOperation::Position => self.search(arguments, true, query)?,
            SequenceOperation::Select => self.select(arguments, query)?,
            SequenceOperation::Resize => self.resize(arguments, query)?,
            SequenceOperation::Reverse => self.reverse(arguments, query)?,
        };
        self.result.validate(&result, query)?;
        Ok(result)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BoundSequenceFunction {
    fn search(&self, arguments: &[Value], position: bool, query: &QueryContext) -> Result<Value> {
        if arguments[0].is_null() || (!position && arguments[1].is_null()) {
            return Ok(Value::Null);
        }
        let values = sequence_values(&arguments[0], self.name)?;
        let child = self
            .child
            .as_ref()
            .ok_or_else(|| Error::Internal("search has no selected child type".into()))?;
        for (index, value) in values.iter().enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            let equal = if arguments[1].is_null() {
                value.is_null()
            } else if value.is_null() {
                false
            } else {
                child.compare(value, &arguments[1], query)? == std::cmp::Ordering::Equal
            };
            if equal {
                return Ok(if position {
                    Value::Integer((index + 1) as i128)
                } else {
                    Value::Boolean(true)
                });
            }
        }
        Ok(if position {
            Value::Null
        } else {
            Value::Boolean(false)
        })
    }

    fn select(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let input = sequence_values(&arguments[0], self.name)?;
        let indices = sequence_values(&arguments[1], self.name)?;
        check_output_count(indices.len(), "list selection", query)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(indices.len())
            .map_err(|_| Error::Resource("cannot allocate selected list values".into()))?;
        for (offset, index) in indices.iter().enumerate() {
            if offset % 1024 == 0 {
                query.check()?;
            }
            if index.is_null() {
                return Err(Error::InvalidInput(
                    "NULLs are not allowed as list elements in the second input parameter.".into(),
                ));
            }
            let index = index.as_i128()?;
            let selected = index
                .checked_sub(1)
                .and_then(|index| usize::try_from(index).ok())
                .and_then(|index| input.get(index))
                .cloned()
                .unwrap_or(Value::Null);
            output.push(selected);
        }
        sequence_result(self.result.data_type(), output)
    }

    fn resize(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        if arguments[0].is_null() {
            return Ok(Value::Null);
        }
        let input = sequence_values(&arguments[0], self.name)?;
        let size = match &arguments[1] {
            Value::Null => 0,
            Value::Unsigned(size) => usize::try_from(*size)
                .map_err(|_| Error::Resource("resized list length exceeds address space".into()))?,
            _ => return Err(Error::Internal("list_resize size was not UBIGINT".into())),
        };
        check_output_count(size, "resized list", query)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(size)
            .map_err(|_| Error::Resource("cannot allocate resized list values".into()))?;
        for (index, value) in input.iter().take(size).enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            output.push(value.clone());
        }
        let fill = arguments.get(2).cloned().unwrap_or(Value::Null);
        while output.len() < size {
            if output.len() % 1024 == 0 {
                query.check()?;
            }
            output.push(fill.clone());
        }
        sequence_result(self.result.data_type(), output)
    }

    fn reverse(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        if arguments[0].is_null() {
            return Ok(Value::Null);
        }
        let input = sequence_values(&arguments[0], self.name)?;
        check_output_count(input.len(), "reversed list", query)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(input.len())
            .map_err(|_| Error::Resource("cannot allocate reversed list values".into()))?;
        for (index, value) in input.iter().rev().enumerate() {
            if index % 1024 == 0 {
                query.check()?;
            }
            output.push(value.clone());
        }
        sequence_result(self.result.data_type(), output)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bind_search(
    arguments: &dyn ScalarBindArguments,
    actual: &[DataType],
    position: bool,
    query: &QueryContext,
) -> Result<SequenceBinding> {
    let [source, needle] = actual else {
        return Err(Error::Bind(
            "list search requires a list and an element".into(),
        ));
    };
    let result = if position {
        DataType::Integer
    } else {
        DataType::Boolean
    };
    if *source == DataType::Null {
        return Ok(SequenceBinding {
            arguments: actual.to_vec(),
            modes: vec![CastMode::Implicit; 2],
            result,
            child: None,
        });
    }
    let source_child = sequence_child(source, "list search")?;
    let common = if *needle == DataType::Null || arguments.is_string_literal(1)? {
        source_child.clone()
    } else {
        query
            .types()
            .try_common_type_with_literals(
                &source_child,
                needle,
                None,
                arguments.full_integer_literal(1)?,
            )?
            .ok_or_else(|| Error::Bind("list and search element types are incompatible".into()))?
    };
    let targets = vec![NestedType::List(common.clone()).data_type(), common.clone()];
    let modes = selected_modes(arguments, actual, &targets)?;
    Ok(SequenceBinding {
        arguments: targets,
        modes,
        result,
        child: Some(common),
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bind_select(actual: &[DataType], _: &QueryContext) -> Result<SequenceBinding> {
    let [source, indices] = actual else {
        return Err(Error::Bind(
            "list_select requires a list and an index list".into(),
        ));
    };
    let child = (*source != DataType::Null)
        .then(|| sequence_child(source, "list_select"))
        .transpose()?;
    if *indices != DataType::Null {
        sequence_child(indices, "list_select index")?;
    }
    let result = match (&child, indices) {
        (Some(child), indices) if *indices != DataType::Null => {
            NestedType::List(child.clone()).data_type()
        }
        _ => DataType::Null,
    };
    let source_target = child
        .as_ref()
        .map(|child| NestedType::List(child.clone()).data_type())
        .unwrap_or(DataType::Null);
    let index_target = if *indices == DataType::Null {
        DataType::Null
    } else {
        NestedType::List(DataType::BigInt).data_type()
    };
    Ok(SequenceBinding {
        arguments: vec![source_target, index_target],
        modes: vec![CastMode::Implicit; 2],
        result,
        child,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bind_resize(
    arguments: &dyn ScalarBindArguments,
    actual: &[DataType],
    _: &QueryContext,
) -> Result<SequenceBinding> {
    if !matches!(actual.len(), 2 | 3) {
        return Err(Error::Bind(
            "list_resize requires a list, size, and optional value".into(),
        ));
    }
    let mut targets = actual.to_vec();
    targets[1] = DataType::UBigInt;
    let child = if actual[0] == DataType::Null {
        None
    } else {
        Some(sequence_child(&actual[0], "list_resize")?)
    };
    let result = if let Some(child) = &child {
        targets[0] = NestedType::List(child.clone()).data_type();
        if actual.len() == 3 && actual[2] != DataType::Null {
            targets[2] = child.clone();
        }
        NestedType::List(child.clone()).data_type()
    } else {
        DataType::Null
    };
    let modes = selected_modes(arguments, actual, &targets)?;
    Ok(SequenceBinding {
        arguments: targets,
        modes,
        result,
        child,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bind_reverse(actual: &[DataType], _: &QueryContext) -> Result<SequenceBinding> {
    let [source] = actual else {
        return Err(Error::Bind("list_reverse requires one list".into()));
    };
    if *source == DataType::Null {
        return Ok(SequenceBinding {
            arguments: vec![DataType::Null],
            modes: vec![CastMode::Implicit],
            result: DataType::Null,
            child: None,
        });
    }
    let child = sequence_child(source, "list_reverse")?;
    let result = NestedType::List(child.clone()).data_type();
    Ok(SequenceBinding {
        arguments: vec![result.clone()],
        modes: vec![CastMode::Implicit],
        result,
        child: Some(child),
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sequence_child(data_type: &DataType, operation: &str) -> Result<DataType> {
    match data_type {
        DataType::Nested(metadata) => match metadata.as_ref() {
            NestedType::List(child) | NestedType::Array { element: child, .. } => Ok(child.clone()),
            _ => Err(Error::Bind(format!("{operation} requires a LIST or ARRAY"))),
        },
        _ => Err(Error::Bind(format!("{operation} requires a LIST or ARRAY"))),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn selected_modes(
    arguments: &dyn ScalarBindArguments,
    actual: &[DataType],
    targets: &[DataType],
) -> Result<Vec<CastMode>> {
    actual
        .iter()
        .zip(targets)
        .enumerate()
        .map(|(index, (actual, target))| {
            if actual == target {
                return Ok(CastMode::Implicit);
            }
            let mode = arguments.combination_cast_mode(index, target)?;
            if mode == CastMode::Assignment {
                return Err(Error::Internal(
                    "sequence binding selected an assignment cast".into(),
                ));
            }
            Ok(mode)
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sequence_values<'a>(value: &'a Value, operation: &str) -> Result<&'a [Value]> {
    let Value::Nested(value) = value else {
        return Err(Error::Internal(format!(
            "{operation} expected a sequence argument"
        )));
    };
    let NestedPayload::Sequence(values) = &value.payload else {
        return Err(Error::Internal(format!(
            "{operation} expected a sequence payload"
        )));
    };
    Ok(values)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sequence_result(data_type: &DataType, values: Vec<Value>) -> Result<Value> {
    NestedValue::value(data_type.clone(), NestedPayload::Sequence(values))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn check_output_count(count: usize, operation: &str, query: &QueryContext) -> Result<()> {
    if count > MAX_SEQUENCE_CHILDREN {
        return Err(Error::Resource(format!(
            "{operation} exceeds 16 million child values"
        )));
    }
    query.check_rows(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        common::type_registry::ascii::{self, MaterializedAscii},
        parallel::InterruptHandle,
    };

    struct Arguments {
        types: Vec<DataType>,
        integer_literals: Vec<Option<i128>>,
        string_literals: Vec<bool>,
        modes: Vec<CastMode>,
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl Arguments {
        fn new(types: Vec<DataType>) -> Self {
            let len = types.len();
            Self {
                types,
                integer_literals: vec![None; len],
                string_literals: vec![false; len],
                modes: vec![CastMode::Explicit; len],
            }
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl ScalarBindArguments for Arguments {
        fn len(&self) -> usize {
            self.types.len()
        }

        fn data_type(&self, index: usize) -> Result<DataType> {
            self.types
                .get(index)
                .cloned()
                .ok_or_else(|| Error::Bind("test argument outside signature".into()))
        }

        fn integer_literal(&self, index: usize) -> Result<Option<i128>> {
            self.data_type(index)?;
            Ok(self.integer_literals[index])
        }

        fn is_string_literal(&self, index: usize) -> Result<bool> {
            self.data_type(index)?;
            Ok(self.string_literals[index])
        }

        fn combination_cast_mode(&self, index: usize, _: &DataType) -> Result<CastMode> {
            self.data_type(index)?;
            Ok(self.modes[index])
        }

        fn constant(&self, _: usize) -> Result<Value> {
            Err(Error::Internal(
                "sequence binding must not evaluate arguments".into(),
            ))
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn function(name: &'static str, operation: SequenceOperation) -> SequenceFunction {
        SequenceFunction { name, operation }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn list(data_type: DataType, values: Vec<Value>) -> Result<Value> {
        NestedValue::value(
            NestedType::List(data_type).data_type(),
            NestedPayload::Sequence(values),
        )
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn search_retains_literal_combination_and_cast_modes() -> Result<()> {
        let query = QueryContext::background();
        let tiny_list = NestedType::List(DataType::TinyInt).data_type();
        let mut fitting = Arguments::new(vec![tiny_list.clone(), DataType::Integer]);
        fitting.integer_literals[1] = Some(2);
        let bound = function("list_contains", SequenceOperation::Contains)
            .bind(&fitting, &query)?
            .unwrap();
        assert_eq!(
            bound.argument_types(&fitting.types, query.types())?,
            vec![tiny_list.clone(), DataType::TinyInt]
        );
        assert_eq!(bound.argument_cast_mode(0), CastMode::Implicit);
        assert_eq!(bound.argument_cast_mode(1), CastMode::Explicit);
        assert_eq!(
            bound.return_type(&[tiny_list.clone(), DataType::TinyInt], query.types())?,
            DataType::Boolean
        );

        let mut widening = Arguments::new(vec![tiny_list, DataType::Integer]);
        widening.integer_literals[1] = Some(1000);
        let bound = function("list_position", SequenceOperation::Position)
            .bind(&widening, &query)?
            .unwrap();
        assert_eq!(
            bound.argument_types(&widening.types, query.types())?,
            vec![
                NestedType::List(DataType::Integer).data_type(),
                DataType::Integer
            ]
        );
        assert_eq!(bound.argument_cast_mode(0), CastMode::Explicit);
        assert_eq!(bound.argument_cast_mode(1), CastMode::Implicit);
        assert_eq!(
            bound.return_type(
                &[
                    NestedType::List(DataType::Integer).data_type(),
                    DataType::Integer
                ],
                query.types()
            )?,
            DataType::Integer
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn mechanics_retain_array_to_list_results_and_selected_resize_casts() -> Result<()> {
        let query = QueryContext::background();
        let array = NestedType::Array {
            element: DataType::Integer,
            length: 2,
        }
        .data_type();
        let list = NestedType::List(DataType::Integer).data_type();
        let indices = NestedType::List(DataType::Integer).data_type();
        let select_args = Arguments::new(vec![array.clone(), indices]);
        let select = function("array_select", SequenceOperation::Select)
            .bind(&select_args, &query)?
            .unwrap();
        assert_eq!(
            select.argument_types(&select_args.types, query.types())?,
            vec![list.clone(), NestedType::List(DataType::BigInt).data_type()]
        );
        assert_eq!(
            select.return_type(
                &[list.clone(), NestedType::List(DataType::BigInt).data_type()],
                query.types()
            )?,
            list
        );
        assert_eq!(select.argument_cast_mode(0), CastMode::Implicit);
        assert_eq!(select.argument_cast_mode(1), CastMode::Implicit);

        let resize_args = Arguments::new(vec![array, DataType::Integer, DataType::Varchar]);
        let resize = function("list_resize", SequenceOperation::Resize)
            .bind(&resize_args, &query)?
            .unwrap();
        assert_eq!(
            resize.argument_types(&resize_args.types, query.types())?,
            vec![
                NestedType::List(DataType::Integer).data_type(),
                DataType::UBigInt,
                DataType::Integer
            ]
        );
        assert_eq!(resize.argument_cast_mode(0), CastMode::Explicit);
        assert_eq!(resize.argument_cast_mode(1), CastMode::Explicit);
        assert_eq!(resize.argument_cast_mode(2), CastMode::Explicit);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn search_keeps_selected_nested_child_adapter_without_ambient_fallback() -> Result<()> {
        let mut types = TypeRegistry::builtins();
        types.register(ascii::FAMILY, Arc::new(MaterializedAscii))?;
        let child = ascii::data_type(16)?;
        let list_type = NestedType::List(child.clone()).data_type();
        let arguments = Arguments::new(vec![list_type.clone(), child.clone()]);
        let selected = QueryContext::background().with_types(Arc::new(types));
        let contains = function("list_contains", SequenceOperation::Contains)
            .bind(&arguments, &selected)?
            .unwrap();
        let input = NestedValue::value(
            list_type,
            NestedPayload::Sequence(vec![
                Value::extension(child.clone(), b"A".to_vec()),
                Value::Null,
            ]),
        )?;
        assert_eq!(
            contains.evaluate(
                &[input, Value::extension(child, b"a".to_vec())],
                &QueryContext::background()
            )?,
            Value::Boolean(true)
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn allocation_limits_and_cancellation_precede_partial_results() -> Result<()> {
        let input_type = NestedType::List(DataType::Integer).data_type();
        let resize_args = Arguments::new(vec![input_type.clone(), DataType::UBigInt]);
        let resize = function("list_resize", SequenceOperation::Resize)
            .bind(&resize_args, &QueryContext::background())?
            .unwrap();
        let bounded = QueryContext::new(InterruptHandle::default(), None, 2, 2)?;
        assert!(matches!(
            resize.evaluate(
                &[
                    list(DataType::Integer, vec![Value::Integer(1)])?,
                    Value::Unsigned(3)
                ],
                &bounded
            ),
            Err(Error::Resource(_))
        ));

        let search_args = Arguments::new(vec![input_type, DataType::Integer]);
        let search = function("list_contains", SequenceOperation::Contains)
            .bind(&search_args, &QueryContext::background())?
            .unwrap();
        let interrupt = InterruptHandle::default();
        let cancelled = QueryContext::new(interrupt.clone(), None, 2048, usize::MAX)?;
        interrupt.interrupt();
        assert!(matches!(
            search.evaluate(
                &[
                    list(DataType::Integer, vec![Value::Integer(1)])?,
                    Value::Integer(1)
                ],
                &cancelled
            ),
            Err(Error::Interrupted)
        ));
        Ok(())
    }
}
