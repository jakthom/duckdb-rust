//! Stable LIST/ARRAY sorting and grade permutations with selected child adapters.
use super::*;
use crate::{
    common::{cast::CastMode, type_registry::BoundType},
    function::ArgumentEvaluation,
};
use std::cmp::Ordering;

const MAX_SEQUENCE_CHILDREN: usize = 16_777_216;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SortOperation {
    Sort,
    GradeUp,
    ReverseSort,
}

#[derive(Debug)]
struct SortFunction {
    name: &'static str,
    operation: SortOperation,
}

#[derive(Debug)]
struct BoundSortFunction {
    name: &'static str,
    operation: SortOperation,
    arguments: Vec<BoundType>,
    result: BoundType,
    child: Option<BoundType>,
    list_index: usize,
    descending: bool,
    nulls_first: bool,
    known_null: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for (name, operation) in [
        ("list_sort", SortOperation::Sort),
        ("array_sort", SortOperation::Sort),
        ("list_grade_up", SortOperation::GradeUp),
        ("array_grade_up", SortOperation::GradeUp),
        ("grade_up", SortOperation::GradeUp),
        ("list_reverse_sort", SortOperation::ReverseSort),
        ("array_reverse_sort", SortOperation::ReverseSort),
    ] {
        registry
            .register_scalar(Arc::new(SortFunction { name, operation }))
            .expect("unique nested sort function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for SortFunction {
    fn name(&self) -> &str {
        self.name
    }

    fn accepts_named_arguments(&self) -> bool {
        true
    }

    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        let labels = argument_labels(self.operation, arguments.len(), self.name)?;
        let sources = argument_sources(arguments, labels, self.name)?;
        let actual = (0..arguments.len())
            .map(|index| arguments.data_type(index))
            .collect::<Result<Vec<_>>>()?;
        let list_index = sources[0];
        let (list_type, child_type) = list_target(&actual[list_index], self.name)?;
        let mut targets = actual.clone();
        targets[list_index] = list_type.clone();
        for &index in sources.iter().skip(1) {
            option_type(arguments, &actual[index], index, self.name)?;
            targets[index] = DataType::Varchar;
        }

        // Native default-NULL handling chooses the overload first, then avoids
        // all bind callbacks, including constant-option parsing and settings.
        let mut known_null = false;
        for index in 0..arguments.len() {
            if arguments.is_provably_null(index)? {
                known_null = true;
                break;
            }
        }

        let mut ascending = None;
        let mut nulls_first = None;
        if !known_null && self.operation != SortOperation::ReverseSort && sources.len() >= 2 {
            match constant_option(arguments, sources[1], "sort_order", self.name)? {
                Some(value) => ascending = parse_sort_order(&value)?,
                None => known_null = true,
            }
        }
        if !known_null {
            let null_index = match self.operation {
                SortOperation::Sort | SortOperation::GradeUp if sources.len() == 3 => {
                    Some(sources[2])
                }
                SortOperation::ReverseSort if sources.len() == 2 => Some(sources[1]),
                _ => None,
            };
            if let Some(index) = null_index {
                match constant_option(arguments, index, "null_order", self.name)? {
                    Some(value) => nulls_first = parse_null_order(&value)?,
                    None => known_null = true,
                }
            }
        }

        let (descending, nulls_first) = if known_null {
            (false, false)
        } else if self.operation == SortOperation::ReverseSort {
            let (default_descending, _) = query.settings().ordering(None, None, query)?;
            // `ordering` accepts an ascending override. The resolved default's
            // descending bit is exactly the ascending bit for its reversal.
            query
                .settings()
                .ordering(Some(default_descending), nulls_first, query)?
        } else {
            query.settings().ordering(ascending, nulls_first, query)?
        };
        let nominal_result = if child_type.is_none() {
            DataType::Null
        } else if self.operation == SortOperation::GradeUp {
            NestedType::List(DataType::BigInt).data_type()
        } else {
            list_type
        };
        let result_type = if known_null {
            DataType::Null
        } else {
            nominal_result
        };
        let bound_arguments = targets
            .iter()
            .map(|data_type| query.types().bind(data_type))
            .collect::<Result<Vec<_>>>()?;
        let child = child_type
            .as_ref()
            .map(|data_type| query.types().bind(data_type))
            .transpose()?;
        Ok(Some(Arc::new(BoundSortFunction {
            name: self.name,
            operation: self.operation,
            arguments: bound_arguments,
            result: query.types().bind(&result_type)?,
            child,
            list_index,
            descending,
            nulls_first,
            known_null,
        })))
    }

    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Err(Error::Internal(format!(
            "{} requires contextual sort binding",
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
impl ScalarFunction for BoundSortFunction {
    fn name(&self) -> &str {
        self.name
    }

    fn accepts_named_arguments(&self) -> bool {
        true
    }

    fn argument_evaluation(&self) -> ArgumentEvaluation {
        if self.known_null {
            ArgumentEvaluation::TypeOnly
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

    fn argument_cast_mode(&self, _: usize) -> CastMode {
        CastMode::Implicit
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
                return Err(Error::Internal(
                    "constant NULL nested sort received arguments".into(),
                ));
            }
            return Ok(Value::Null);
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
        if arguments.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let child = self
            .child
            .as_ref()
            .ok_or_else(|| Error::Internal("nested sort has no selected child type".into()))?;
        let values = sequence_values(&arguments[self.list_index], self.name)?;
        check_output_count(values.len(), self.name, query)?;
        let permutation =
            stable_permutation(values, child, self.descending, self.nulls_first, query)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(values.len())
            .map_err(|_| Error::Resource(format!("{} result allocation failed", self.name)))?;
        for index in permutation {
            query.check()?;
            output.push(if self.operation == SortOperation::GradeUp {
                Value::Integer(
                    i128::try_from(
                        index.checked_add(1).ok_or_else(|| {
                            Error::Resource("nested grade position overflow".into())
                        })?,
                    )
                    .map_err(|_| Error::Resource("nested grade position overflow".into()))?,
                )
            } else {
                values[index].clone()
            });
        }
        let result = NestedValue::value(
            self.result.data_type().clone(),
            NestedPayload::Sequence(output),
        )?;
        self.result.validate(&result, query)?;
        Ok(result)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn argument_labels(
    operation: SortOperation,
    count: usize,
    name: &str,
) -> Result<&'static [&'static str]> {
    match (operation, count) {
        (SortOperation::Sort | SortOperation::GradeUp, 1) => Ok(&["list"]),
        (SortOperation::Sort | SortOperation::GradeUp, 2) => Ok(&["list", "sort_order"]),
        (SortOperation::Sort | SortOperation::GradeUp, 3) => {
            Ok(&["list", "sort_order", "null_order"])
        }
        (SortOperation::ReverseSort, 1) => Ok(&["list"]),
        (SortOperation::ReverseSort, 2) => Ok(&["list", "null_order"]),
        _ => Err(Error::Bind(format!("invalid {name} argument count"))),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn argument_sources(
    arguments: &dyn ScalarBindArguments,
    labels: &[&str],
    name: &str,
) -> Result<Vec<usize>> {
    let mut sources = vec![None; labels.len()];
    let mut positional = 0_usize;
    let mut named = false;
    for source in 0..arguments.len() {
        let target = if let Some(argument_name) = arguments.argument_name(source)? {
            named = true;
            labels
                .iter()
                .position(|label| label.eq_ignore_ascii_case(argument_name))
                .ok_or_else(|| {
                    Error::Bind(format!(
                        "no {name} overload has an argument named {argument_name}"
                    ))
                })?
        } else {
            if named {
                return Err(Error::Bind(format!(
                    "positional arguments cannot follow named arguments in {name}"
                )));
            }
            let target = positional;
            positional += 1;
            target
        };
        if sources[target].replace(source).is_some() {
            return Err(Error::Bind(format!(
                "duplicate named argument {} in {name}",
                labels[target]
            )));
        }
    }
    sources
        .into_iter()
        .enumerate()
        .map(|(index, source)| {
            source.ok_or_else(|| Error::Bind(format!("{name} requires argument {}", labels[index])))
        })
        .collect()
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn list_target(data_type: &DataType, name: &str) -> Result<(DataType, Option<DataType>)> {
    if *data_type == DataType::Null {
        return Ok((DataType::Null, None));
    }
    let DataType::Nested(metadata) = data_type else {
        return Err(Error::Bind(format!("{name} requires a LIST or ARRAY")));
    };
    let child = match metadata.as_ref() {
        NestedType::List(child) | NestedType::Array { element: child, .. } => child.clone(),
        _ => return Err(Error::Bind(format!("{name} requires a LIST or ARRAY"))),
    };
    Ok((NestedType::List(child.clone()).data_type(), Some(child)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn option_type(
    arguments: &dyn ScalarBindArguments,
    actual: &DataType,
    index: usize,
    name: &str,
) -> Result<()> {
    if matches!(actual, DataType::Null | DataType::Varchar)
        || arguments.combination_cast_mode(index, &DataType::Varchar)? == CastMode::Implicit
    {
        Ok(())
    } else {
        Err(Error::Bind(format!(
            "no {name} overload accepts {actual} for an ordering option"
        )))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn constant_option(
    arguments: &dyn ScalarBindArguments,
    index: usize,
    label: &str,
    name: &str,
) -> Result<Option<String>> {
    if !arguments.is_closed(index)? {
        return Err(Error::Bind(format!(
            "The \"{label}\" argument in function \"{name}\" must be a constant expression"
        )));
    }
    match arguments.constant_as(index, &DataType::Varchar, CastMode::Implicit)? {
        Value::Varchar(value) => Ok(Some(value)),
        Value::Null => Ok(None),
        _ => Err(Error::Internal(format!(
            "selected {name} {label} must be VARCHAR"
        ))),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_sort_order(value: &str) -> Result<Option<bool>> {
    match value.to_ascii_uppercase().as_str() {
        "DEFAULT" | "ORDER_DEFAULT" => Ok(None),
        "ASC" | "ASCENDING" => Ok(Some(true)),
        "DESC" | "DESCENDING" => Ok(Some(false)),
        _ => Err(Error::NotImplemented(format!(
            "Enum value: unrecognized value \"{value}\" for enum \"OrderType\""
        ))),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_null_order(value: &str) -> Result<Option<bool>> {
    match value.to_ascii_uppercase().as_str() {
        "DEFAULT" | "ORDER_DEFAULT" => Ok(None),
        "NULLS FIRST" | "NULLS_FIRST" => Ok(Some(true)),
        "NULLS LAST" | "NULLS_LAST" => Ok(Some(false)),
        _ => Err(Error::NotImplemented(format!(
            "Enum value: unrecognized value \"{value}\" for enum \"OrderByNullType\""
        ))),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn stable_permutation(
    values: &[Value],
    child: &BoundType,
    descending: bool,
    nulls_first: bool,
    query: &QueryContext,
) -> Result<Vec<usize>> {
    check_output_count(values.len(), "sorted list", query)?;
    let mut permutation = Vec::new();
    permutation
        .try_reserve_exact(values.len())
        .map_err(|_| Error::Resource("nested sort permutation allocation failed".into()))?;
    permutation.extend(0..values.len());
    let mut scratch = Vec::new();
    scratch
        .try_reserve_exact(values.len())
        .map_err(|_| Error::Resource("nested sort scratch allocation failed".into()))?;
    scratch.resize(values.len(), 0);
    let mut width = 1_usize;
    while width < values.len() {
        let step = width
            .checked_mul(2)
            .ok_or_else(|| Error::Resource("nested sort width overflow".into()))?;
        for start in (0..values.len()).step_by(step) {
            query.check()?;
            let middle = start
                .checked_add(width)
                .ok_or_else(|| Error::Resource("nested sort range overflow".into()))?
                .min(values.len());
            let end = middle
                .checked_add(width)
                .ok_or_else(|| Error::Resource("nested sort range overflow".into()))?
                .min(values.len());
            let (mut left, mut right) = (start, middle);
            for output in &mut scratch[start..end] {
                let take_left = left < middle
                    && (right == end
                        || compare_value(
                            &values[permutation[left]],
                            &values[permutation[right]],
                            child,
                            descending,
                            nulls_first,
                            query,
                        )? != Ordering::Greater);
                let position = if take_left {
                    let position = left;
                    left += 1;
                    position
                } else {
                    let position = right;
                    right += 1;
                    position
                };
                *output = permutation[position];
            }
        }
        std::mem::swap(&mut permutation, &mut scratch);
        width = step;
    }
    query.check()?;
    Ok(permutation)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compare_value(
    left: &Value,
    right: &Value,
    child: &BoundType,
    descending: bool,
    nulls_first: bool,
    query: &QueryContext,
) -> Result<Ordering> {
    query.check()?;
    Ok(match (left.is_null(), right.is_null()) {
        (true, true) => Ordering::Equal,
        (true, false) => {
            if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (false, true) => {
            if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (false, false) => {
            let comparison = child.compare(left, right, query)?;
            if descending {
                comparison.reverse()
            } else {
                comparison
            }
        }
    })
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
    use crate::parallel::InterruptHandle;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn stable_grade_permutation_keeps_ties_and_places_nulls() -> Result<()> {
        let query = QueryContext::background();
        let child = query.types().bind(&DataType::Integer)?;
        let values = [
            Value::Integer(2),
            Value::Integer(1),
            Value::Integer(2),
            Value::Integer(1),
        ];
        assert_eq!(
            stable_permutation(&values, &child, false, false, &query)?,
            vec![1, 3, 0, 2]
        );
        let values = [Value::Integer(2), Value::Null, Value::Integer(1)];
        assert_eq!(
            stable_permutation(&values, &child, true, true, &query)?,
            vec![1, 0, 2]
        );
        assert_eq!(
            stable_permutation(&values, &child, false, false, &query)?,
            vec![2, 0, 1]
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn option_parsers_match_pinned_enum_spellings() -> Result<()> {
        assert_eq!(parse_sort_order("dEsC")?, Some(false));
        assert_eq!(parse_sort_order("ORDER_DEFAULT")?, None);
        assert_eq!(parse_null_order("nuLls FIRST")?, Some(true));
        assert_eq!(parse_null_order("NULLS_LAST")?, Some(false));
        assert!(parse_sort_order(" DESC ").is_err());
        assert!(parse_null_order("FIRST").is_err());
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn stable_permutation_honors_cancellation_and_row_limits() -> Result<()> {
        let values = [Value::Integer(2), Value::Integer(1), Value::Integer(3)];
        let query = QueryContext::background();
        let child = query.types().bind(&DataType::Integer)?;
        let interrupt = InterruptHandle::default();
        interrupt.interrupt();
        let cancelled = QueryContext::new(interrupt, None, 2, 10)?;
        assert!(matches!(
            stable_permutation(&values, &child, false, false, &cancelled),
            Err(Error::Interrupted)
        ));
        let limited = QueryContext::new(InterruptHandle::default(), None, 2, 2)?;
        assert!(matches!(
            stable_permutation(&values, &child, false, false, &limited),
            Err(Error::Resource(_))
        ));
        Ok(())
    }
}
