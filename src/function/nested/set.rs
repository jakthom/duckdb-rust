//! LIST/ARRAY set predicates and hash/probe-order intersection.
use super::*;
use crate::{
    common::{
        cast::CastMode,
        type_registry::{BoundType, KeyContext},
    },
    function::ArgumentEvaluation,
};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetOperation {
    HasAny,
    HasAll,
    Intersect,
}

#[derive(Debug)]
struct SetFunction {
    name: &'static str,
    operation: SetOperation,
    reverse: bool,
}

#[derive(Debug)]
struct BoundSetFunction {
    name: &'static str,
    operation: SetOperation,
    reverse: bool,
    arguments: Vec<BoundType>,
    modes: Vec<CastMode>,
    result: BoundType,
    child: Option<BoundType>,
    known_null: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for (name, operation, reverse) in [
        ("list_has_any", SetOperation::HasAny, false),
        ("array_has_any", SetOperation::HasAny, false),
        ("&&", SetOperation::HasAny, false),
        ("list_has_all", SetOperation::HasAll, false),
        ("array_has_all", SetOperation::HasAll, false),
        ("@>", SetOperation::HasAll, false),
        ("<@", SetOperation::HasAll, true),
        ("list_intersect", SetOperation::Intersect, false),
        ("array_intersect", SetOperation::Intersect, false),
    ] {
        registry
            .register_scalar(Arc::new(SetFunction {
                name,
                operation,
                reverse,
            }))
            .expect("unique nested set function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for SetFunction {
    fn name(&self) -> &str {
        self.name
    }

    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        if arguments.len() != 2 {
            return Err(Error::Bind(format!(
                "{} requires exactly two LIST or ARRAY arguments",
                self.name
            )));
        }
        let actual = (0..2)
            .map(|index| arguments.data_type(index))
            .collect::<Result<Vec<_>>>()?;
        let left = sequence_child(&actual[0], self.name)?;
        let right = sequence_child(&actual[1], self.name)?;
        let child = match (left, right) {
            (Some(left), Some(right)) => Some(query.types().common_type(&left, &right)?),
            (Some(child), None) | (None, Some(child)) => Some(child),
            (None, None) => None,
        };
        let target = child
            .as_ref()
            .map(|child| NestedType::List(child.clone()).data_type())
            .unwrap_or(DataType::Null);
        let targets = vec![target.clone(), target];
        let modes = selected_modes(arguments, &actual, &targets)?;
        let known_null = if self.operation == SetOperation::Intersect {
            false
        } else {
            arguments.is_provably_null(0)? || arguments.is_provably_null(1)?
        };
        let result = if self.operation == SetOperation::Intersect {
            child
                .as_ref()
                .map(|child| NestedType::List(child.clone()).data_type())
                .unwrap_or(DataType::Null)
        } else {
            DataType::Boolean
        };
        Ok(Some(Arc::new(BoundSetFunction {
            name: self.name,
            operation: self.operation,
            reverse: self.reverse,
            arguments: targets
                .iter()
                .map(|target| query.types().bind(target))
                .collect::<Result<_>>()?,
            modes,
            result: query.types().bind(&result)?,
            child: child
                .as_ref()
                .map(|child| query.types().bind(child))
                .transpose()?,
            known_null,
        })))
    }

    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Err(Error::Internal(format!(
            "{} requires contextual set binding",
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
impl ScalarFunction for BoundSetFunction {
    fn name(&self) -> &str {
        self.name
    }

    fn argument_evaluation(&self) -> ArgumentEvaluation {
        if self.known_null {
            ArgumentEvaluation::TypeOnly
        } else if self.operation == SetOperation::Intersect {
            ArgumentEvaluation::Eager
        } else {
            ArgumentEvaluation::NullOnConstant
        }
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
        if self.known_null {
            if !arguments.is_empty() {
                return Err(Error::Internal(format!(
                    "constant NULL {} received arguments",
                    self.name
                )));
            }
            let result = Value::Null;
            self.result.validate(&result, query)?;
            return Ok(result);
        }
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
            SetOperation::HasAny | SetOperation::HasAll if arguments.iter().any(Value::is_null) => {
                Value::Null
            }
            SetOperation::HasAny => {
                let (left, right) = self.predicate_inputs(arguments)?;
                Value::Boolean(has_any(left, right, self.child()?, query)?)
            }
            SetOperation::HasAll => {
                let (left, right) = self.predicate_inputs(arguments)?;
                Value::Boolean(has_all(left, right, self.child()?, query)?)
            }
            SetOperation::Intersect if arguments[0].is_null() => Value::Null,
            SetOperation::Intersect if arguments[1].is_null() => {
                sequence_result(self.result.data_type(), Vec::new())?
            }
            SetOperation::Intersect => intersect(
                sequence_values(&arguments[0], self.name)?,
                sequence_values(&arguments[1], self.name)?,
                self.child()?,
                self.result.data_type(),
                query,
            )?,
        };
        self.result.validate(&result, query)?;
        Ok(result)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BoundSetFunction {
    fn child(&self) -> Result<&BoundType> {
        self.child
            .as_ref()
            .ok_or_else(|| Error::Internal(format!("{} has no selected child type", self.name)))
    }

    fn predicate_inputs<'a>(&self, arguments: &'a [Value]) -> Result<(&'a [Value], &'a [Value])> {
        let (left, right) = if self.reverse {
            (&arguments[1], &arguments[0])
        } else {
            (&arguments[0], &arguments[1])
        };
        Ok((
            sequence_values(left, self.name)?,
            sequence_values(right, self.name)?,
        ))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sequence_child(data_type: &DataType, name: &str) -> Result<Option<DataType>> {
    match data_type {
        DataType::Null => Ok(None),
        DataType::Nested(metadata) => match metadata.as_ref() {
            NestedType::List(child) | NestedType::Array { element: child, .. } => {
                Ok(Some(child.clone()))
            }
            _ => Err(Error::Bind(format!(
                "{name} requires exactly two LIST or ARRAY arguments"
            ))),
        },
        _ => Err(Error::Bind(format!(
            "{name} requires exactly two LIST or ARRAY arguments"
        ))),
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
                    "set binding selected an assignment cast".into(),
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
fn check_children(values: &[Value], _: &str, query: &QueryContext) -> Result<()> {
    query.check_rows(values.len())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn key_set(capacity: usize, operation: &str) -> Result<HashSet<Vec<u8>>> {
    let mut keys = HashSet::new();
    keys.try_reserve(capacity)
        .map_err(|_| Error::Resource(format!("cannot allocate {operation} keys")))?;
    Ok(keys)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn append_key(
    child: &BoundType,
    value: &Value,
    key: &mut Vec<u8>,
    query: &QueryContext,
) -> Result<()> {
    key.clear();
    child.append_key_with_context(value, KeyContext::SortEquivalence, key, query)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn collect_keys(
    values: &[Value],
    child: &BoundType,
    operation: &str,
    query: &QueryContext,
) -> Result<HashSet<Vec<u8>>> {
    check_children(values, operation, query)?;
    let mut keys = key_set(values.len(), operation)?;
    for (index, value) in values.iter().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        if value.is_null() {
            continue;
        }
        let mut key = Vec::new();
        append_key(child, value, &mut key, query)?;
        keys.insert(key);
    }
    query.check()?;
    Ok(keys)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn has_any(
    left: &[Value],
    right: &[Value],
    child: &BoundType,
    query: &QueryContext,
) -> Result<bool> {
    check_children(left, "list_has_any", query)?;
    check_children(right, "list_has_any", query)?;
    let (build, probe) = if left.len() <= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    let keys = collect_keys(build, child, "list_has_any", query)?;
    let mut key = Vec::new();
    for (index, value) in probe.iter().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        if value.is_null() {
            continue;
        }
        append_key(child, value, &mut key, query)?;
        if keys.contains(key.as_slice()) {
            return Ok(true);
        }
    }
    query.check()?;
    Ok(false)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn has_all(
    left: &[Value],
    right: &[Value],
    child: &BoundType,
    query: &QueryContext,
) -> Result<bool> {
    check_children(left, "list_has_all", query)?;
    check_children(right, "list_has_all", query)?;
    let keys = collect_keys(left, child, "list_has_all", query)?;
    let mut key = Vec::new();
    for (index, value) in right.iter().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        if value.is_null() {
            continue;
        }
        append_key(child, value, &mut key, query)?;
        if !keys.contains(key.as_slice()) {
            return Ok(false);
        }
    }
    query.check()?;
    Ok(true)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn intersect(
    left: &[Value],
    right: &[Value],
    child: &BoundType,
    result_type: &DataType,
    query: &QueryContext,
) -> Result<Value> {
    check_children(left, "list_intersect", query)?;
    check_children(right, "list_intersect", query)?;
    let capacity = left.len().min(right.len());
    query.check_rows(capacity)?;
    let use_left_for_hash = left.len() <= right.len();
    let (hash, iterate) = if use_left_for_hash {
        (left, right)
    } else {
        (right, left)
    };
    check_children(hash, "list_intersect", query)?;
    let mut keys = HashMap::new();
    keys.try_reserve(hash.len())
        .map_err(|_| Error::Resource("cannot allocate list_intersect keys".into()))?;
    let mut key = Vec::new();
    for (index, value) in hash.iter().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        if value.is_null() {
            continue;
        }
        append_key(child, value, &mut key, query)?;
        // DuckDB's map records the final left index when the left input is
        // hashed. Preserve that representative and the probe-side order.
        if use_left_for_hash {
            keys.insert(std::mem::take(&mut key), value.clone());
        } else {
            keys.insert(std::mem::take(&mut key), Value::Null);
        }
    }
    let mut emitted = key_set(capacity, "list_intersect result")?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(capacity)
        .map_err(|_| Error::Resource("cannot allocate list_intersect result".into()))?;
    for (index, value) in iterate.iter().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        if value.is_null() {
            continue;
        }
        append_key(child, value, &mut key, query)?;
        if let Some(left_value) = keys.get(key.as_slice())
            && !emitted.contains(key.as_slice())
        {
            emitted.insert(std::mem::take(&mut key));
            output.push(if use_left_for_hash {
                left_value.clone()
            } else {
                value.clone()
            });
        }
    }
    query.check()?;
    sequence_result(result_type, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        common::type_registry::{KeyWriter, PrimitiveTypes, TypeAdapter},
        parallel::InterruptHandle,
    };
    use std::{cmp::Ordering, sync::Arc};

    struct Arguments {
        types: Vec<DataType>,
        provably_null: Vec<bool>,
        modes: Vec<CastMode>,
    }

    impl Arguments {
        fn new(types: Vec<DataType>) -> Self {
            let len = types.len();
            Self {
                types,
                provably_null: vec![false; len],
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

        fn combination_cast_mode(&self, index: usize, _: &DataType) -> Result<CastMode> {
            self.data_type(index)?;
            Ok(self.modes[index])
        }

        fn is_provably_null(&self, index: usize) -> Result<bool> {
            self.data_type(index)?;
            Ok(self.provably_null[index])
        }

        fn constant(&self, _: usize) -> Result<Value> {
            Err(Error::Internal(
                "set binding must not evaluate arguments".into(),
            ))
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn function(name: &'static str, operation: SetOperation, reverse: bool) -> SetFunction {
        SetFunction {
            name,
            operation,
            reverse,
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn list_type(child: DataType) -> DataType {
        NestedType::List(child).data_type()
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn list(child: DataType, values: Vec<Value>) -> Result<Value> {
        NestedValue::value(list_type(child), NestedPayload::Sequence(values))
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn bind(
        name: &'static str,
        operation: SetOperation,
        reverse: bool,
        arguments: &Arguments,
        query: &QueryContext,
    ) -> Result<Arc<dyn ScalarFunction>> {
        function(name, operation, reverse)
            .bind(arguments, query)?
            .ok_or_else(|| Error::Internal("set function did not specialize".into()))
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn result_values(value: Value) -> Result<Vec<Value>> {
        let Value::Nested(value) = value else {
            return Err(Error::Internal("expected nested result".into()));
        };
        let NestedPayload::Sequence(values) = &value.payload else {
            return Err(Error::Internal("expected sequence result".into()));
        };
        Ok(values.clone())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn aliases_register_without_claiming_operator_syntax() -> Result<()> {
        let registry = FunctionRegistry::builtins();
        for name in [
            "list_has_any",
            "array_has_any",
            "&&",
            "list_has_all",
            "array_has_all",
            "@>",
            "<@",
            "list_intersect",
            "array_intersect",
        ] {
            assert_eq!(registry.scalar(name)?.name(), name);
        }
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn binding_normalizes_unequal_arrays_to_a_common_list_child() -> Result<()> {
        let left = NestedType::Array {
            element: DataType::SmallInt,
            length: 2,
        }
        .data_type();
        let right = NestedType::Array {
            element: DataType::Integer,
            length: 3,
        }
        .data_type();
        let arguments = Arguments::new(vec![left, right]);
        let query = QueryContext::background();
        for operation in [
            SetOperation::HasAny,
            SetOperation::HasAll,
            SetOperation::Intersect,
        ] {
            let bound = bind("set", operation, false, &arguments, &query)?;
            let expected = list_type(DataType::Integer);
            assert_eq!(
                bound.argument_types(&arguments.types, query.types())?,
                vec![expected.clone(), expected.clone()]
            );
            assert_eq!(bound.argument_cast_mode(0), CastMode::Explicit);
            assert_eq!(bound.argument_cast_mode(1), CastMode::Explicit);
            assert_eq!(
                bound.return_type(&[expected.clone(), expected.clone()], query.types())?,
                if operation == SetOperation::Intersect {
                    expected
                } else {
                    DataType::Boolean
                }
            );
        }
        assert!(
            bind(
                "set",
                SetOperation::HasAny,
                false,
                &Arguments::new(vec![DataType::Integer, list_type(DataType::Integer)]),
                &query
            )
            .is_err()
        );
        assert!(
            bind(
                "set",
                SetOperation::HasAny,
                false,
                &Arguments::new(vec![list_type(DataType::Integer)]),
                &query
            )
            .is_err()
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn predicates_ignore_child_nulls_and_duplicate_members() -> Result<()> {
        let ty = list_type(DataType::Integer);
        let arguments = Arguments::new(vec![ty.clone(), ty]);
        let query = QueryContext::background();
        let left = list(
            DataType::Integer,
            vec![
                Value::Integer(1),
                Value::Null,
                Value::Integer(1),
                Value::Integer(2),
            ],
        )?;
        let overlap = list(
            DataType::Integer,
            vec![Value::Null, Value::Integer(2), Value::Integer(3)],
        )?;
        let any = bind(
            "list_has_any",
            SetOperation::HasAny,
            false,
            &arguments,
            &query,
        )?;
        assert_eq!(
            any.evaluate(&[left.clone(), overlap.clone()], &query)?,
            Value::Boolean(true)
        );
        let all = bind(
            "list_has_all",
            SetOperation::HasAll,
            false,
            &arguments,
            &query,
        )?;
        assert_eq!(
            all.evaluate(&[left.clone(), overlap], &query)?,
            Value::Boolean(false)
        );
        assert_eq!(
            all.evaluate(
                &[
                    left.clone(),
                    list(DataType::Integer, vec![Value::Null, Value::Null])?
                ],
                &query
            )?,
            Value::Boolean(true)
        );
        assert_eq!(
            all.evaluate(
                &[left.clone(), list(DataType::Integer, Vec::new())?],
                &query
            )?,
            Value::Boolean(true)
        );
        assert_eq!(
            all.evaluate(
                &[
                    list(DataType::Integer, Vec::new())?,
                    list(DataType::Integer, vec![Value::Integer(1)])?
                ],
                &query
            )?,
            Value::Boolean(false)
        );
        let subset = bind("<@", SetOperation::HasAll, true, &arguments, &query)?;
        assert_eq!(
            subset.evaluate(
                &[list(DataType::Integer, vec![Value::Integer(1)])?, left],
                &query
            )?,
            Value::Boolean(true)
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn null_lifecycles_distinguish_predicates_from_intersection() -> Result<()> {
        let ty = list_type(DataType::Integer);
        let mut arguments = Arguments::new(vec![ty.clone(), ty.clone()]);
        arguments.provably_null[0] = true;
        let query = QueryContext::background();
        for operation in [SetOperation::HasAny, SetOperation::HasAll] {
            let bound = bind("predicate", operation, false, &arguments, &query)?;
            assert!(matches!(
                bound.argument_evaluation(),
                ArgumentEvaluation::TypeOnly
            ));
            assert_eq!(bound.evaluate(&[], &query)?, Value::Null);
        }
        let runtime_arguments = Arguments::new(vec![ty.clone(), ty]);
        let any = bind(
            "list_has_any",
            SetOperation::HasAny,
            false,
            &runtime_arguments,
            &query,
        )?;
        assert!(matches!(
            any.argument_evaluation(),
            ArgumentEvaluation::NullOnConstant
        ));
        assert_eq!(
            any.evaluate(
                &[
                    Value::Null,
                    list(DataType::Integer, vec![Value::Integer(1)])?
                ],
                &query
            )?,
            Value::Null
        );
        let intersect = bind(
            "list_intersect",
            SetOperation::Intersect,
            false,
            &arguments,
            &query,
        )?;
        assert!(matches!(
            intersect.argument_evaluation(),
            ArgumentEvaluation::Eager
        ));
        assert_eq!(
            intersect.evaluate(
                &[
                    Value::Null,
                    list(DataType::Integer, vec![Value::Integer(1)])?
                ],
                &query
            )?,
            Value::Null
        );
        assert!(
            result_values(intersect.evaluate(
                &[
                    list(DataType::Integer, vec![Value::Integer(1)])?,
                    Value::Null
                ],
                &query
            )?)?
            .is_empty()
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn intersection_is_distinct_and_preserves_pinned_left_representatives() -> Result<()> {
        let ty = list_type(DataType::Double);
        let arguments = Arguments::new(vec![ty.clone(), ty]);
        let query = QueryContext::background();
        let intersect = bind(
            "list_intersect",
            SetOperation::Intersect,
            false,
            &arguments,
            &query,
        )?;
        let nan = f64::from_bits(0x7ff8_0000_0000_1234);
        let other_nan = f64::from_bits(0xfff8_0000_0000_5678);
        let result = result_values(intersect.evaluate(
            &[
                list(
                    DataType::Double,
                    vec![
                        Value::Double(2.0),
                        Value::Double(nan),
                        Value::Double(-0.0),
                        Value::Double(2.0),
                        Value::Null,
                    ],
                )?,
                list(
                    DataType::Double,
                    vec![
                        Value::Double(other_nan),
                        Value::Double(2.0),
                        Value::Double(0.0),
                        Value::Null,
                    ],
                )?,
            ],
            &query,
        )?)?;
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], Value::Double(2.0));
        let Value::Double(result_nan) = result[1] else {
            panic!("expected NaN representative")
        };
        assert_eq!(result_nan.to_bits(), nan.to_bits());
        let Value::Double(result_zero) = result[2] else {
            panic!("expected zero representative")
        };
        assert_eq!(result_zero.to_bits(), (-0.0_f64).to_bits());
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn intersection_uses_the_pinned_hash_side_order_and_representative() -> Result<()> {
        let ty = list_type(DataType::Integer);
        let arguments = Arguments::new(vec![ty.clone(), ty]);
        let query = QueryContext::background();
        let intersect = bind(
            "list_intersect",
            SetOperation::Intersect,
            false,
            &arguments,
            &query,
        )?;
        // With the shorter left side hashed, DuckDB emits in right probe
        // order. Its key map identifies the left representative.
        assert_eq!(
            result_values(intersect.evaluate(
                &[
                    list(
                        DataType::Integer,
                        vec![Value::Integer(3), Value::Integer(2)]
                    )?,
                    list(
                        DataType::Integer,
                        vec![Value::Integer(2), Value::Integer(3), Value::Integer(2)],
                    )?,
                ],
                &query,
            )?)?,
            vec![Value::Integer(2), Value::Integer(3)]
        );
        Ok(())
    }

    #[derive(Debug)]
    struct FoldedSortKeys;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl TypeAdapter for FoldedSortKeys {
        fn name(&self) -> &'static str {
            "folded-sort-keys"
        }

        fn validate_type(&self, data_type: &DataType) -> Result<()> {
            PrimitiveTypes.validate_type(data_type)
        }

        fn validate_value(
            &self,
            data_type: &DataType,
            value: &Value,
            query: &QueryContext,
        ) -> Result<()> {
            PrimitiveTypes.validate_value(data_type, value, query)
        }

        fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
            PrimitiveTypes.common_type(left, right)
        }

        fn compare(
            &self,
            data_type: &DataType,
            left: &Value,
            right: &Value,
            query: &QueryContext,
        ) -> Result<Ordering> {
            PrimitiveTypes.compare(data_type, left, right, query)
        }

        fn write_key(
            &self,
            data_type: &DataType,
            value: &Value,
            output: &mut KeyWriter<'_>,
            query: &QueryContext,
        ) -> Result<()> {
            PrimitiveTypes.write_key(data_type, value, output, query)
        }

        fn write_key_with_context(
            &self,
            data_type: &DataType,
            value: &Value,
            key_context: KeyContext,
            output: &mut KeyWriter<'_>,
            query: &QueryContext,
        ) -> Result<()> {
            if key_context == KeyContext::Equality {
                return self.write_key(data_type, value, output, query);
            }
            query.check()?;
            let Value::Varchar(value) = value else {
                return Err(Error::Internal("folded key expected VARCHAR".into()));
            };
            output.extend_from_slice(value.to_ascii_lowercase().as_bytes())
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn nested_membership_forwards_sort_equivalence_through_retained_children() -> Result<()> {
        let mut types = TypeRegistry::builtins();
        types.replace(DataType::Varchar.family(), Arc::new(FoldedSortKeys))?;
        let selected = QueryContext::background().with_types(Arc::new(types));
        let inner_type = list_type(DataType::Varchar);
        let outer_type = list_type(inner_type.clone());
        let arguments = Arguments::new(vec![outer_type.clone(), outer_type]);
        let any = bind(
            "list_has_any",
            SetOperation::HasAny,
            false,
            &arguments,
            &selected,
        )?;
        let left_inner = list(DataType::Varchar, vec![Value::Varchar("A".into())])?;
        let right_inner = list(DataType::Varchar, vec![Value::Varchar("a".into())])?;
        let left = list(inner_type.clone(), vec![left_inner])?;
        let right = list(inner_type, vec![right_inner])?;
        assert_eq!(
            any.evaluate(&[left, right], &QueryContext::background())?,
            Value::Boolean(true)
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn cancellation_row_limits_and_key_limits_fail_without_a_result() -> Result<()> {
        let integer_type = list_type(DataType::Integer);
        let integer_arguments = Arguments::new(vec![integer_type.clone(), integer_type]);
        let function = bind(
            "list_intersect",
            SetOperation::Intersect,
            false,
            &integer_arguments,
            &QueryContext::background(),
        )?;
        let interrupt = InterruptHandle::default();
        interrupt.interrupt();
        let cancelled = QueryContext::new(interrupt, None, 2, 100)?;
        assert!(matches!(
            function.evaluate(
                &[
                    list(DataType::Integer, vec![Value::Integer(1)])?,
                    list(DataType::Integer, vec![Value::Integer(1)])?
                ],
                &cancelled
            ),
            Err(Error::Interrupted)
        ));
        let limited = QueryContext::new(InterruptHandle::default(), None, 2, 1)?;
        assert!(matches!(
            function.evaluate(
                &[
                    list(
                        DataType::Integer,
                        vec![Value::Integer(1), Value::Integer(2)]
                    )?,
                    list(DataType::Integer, vec![Value::Integer(2)])?
                ],
                &limited
            ),
            Err(Error::Resource(_))
        ));

        let varchar_type = list_type(DataType::Varchar);
        let varchar_arguments = Arguments::new(vec![varchar_type.clone(), varchar_type]);
        let function = bind(
            "list_has_any",
            SetOperation::HasAny,
            false,
            &varchar_arguments,
            &QueryContext::background(),
        )?;
        let oversized = "x".repeat(16 * 1024 * 1024);
        assert!(matches!(
            function.evaluate(
                &[
                    list(DataType::Varchar, vec![Value::Varchar(oversized.clone())])?,
                    list(DataType::Varchar, vec![Value::Varchar(oversized)])?
                ],
                &QueryContext::background()
            ),
            Err(Error::Resource(_))
        ));
        Ok(())
    }
}
