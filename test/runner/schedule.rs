//! SQLLogicTest loop definitions, substitutions, conditions, and bounded scheduling.
//!
//! This is a direct Rust counterpart of the loop-specific portions of pinned
//! `test/sqlite/sqllogic_command.cpp`.  Parsing SQL records remains in the
//! byte-preserving parser; this module owns only the execution-time loop state.

use duckdb_rust::{Error, Result};
use std::hash::{BuildHasher, Hasher};

const MAX_LOOP_ITERATIONS: usize = 100_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LoopDefinition {
    pub name: String,
    pub values: Vec<String>,
    pub concurrent: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LoopFrame {
    pub name: String,
    pub value: String,
    /// Zero-based ordinal in the expanded loop, used by record accounting.
    pub ordinal: usize,
    pub concurrent: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Comparison {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Condition {
    System {
        skip_if: bool,
        name: String,
    },
    Loop {
        skip_if: bool,
        iterator: String,
        comparison: Comparison,
        value: String,
    },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[allow(dead_code)]
pub(crate) fn parse_loop(
    arguments: &[String],
    concurrent: bool,
    foreach: bool,
) -> Result<LoopDefinition> {
    parse_loop_with(arguments, concurrent, foreach, |_| {
        Err(Error::Execution(
            "foreach variable requires a live SQLLogic session".into(),
        ))
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn parse_loop_with<F>(
    arguments: &[String],
    concurrent: bool,
    foreach: bool,
    mut variable: F,
) -> Result<LoopDefinition>
where
    F: FnMut(&str) -> Result<Vec<String>>,
{
    if foreach {
        if arguments.len() < 2 {
            return Err(Error::Execution(
                "expected foreach ITERATOR VALUE [VALUE ...]".into(),
            ));
        }
        let mut values = Vec::new();
        for argument in &arguments[1..] {
            expand_foreach_token(argument, &mut values, &mut variable)?;
        }
        if values.is_empty() {
            return Err(Error::Unsupported(
                "foreach expansion produced no iterations".into(),
            ));
        }
        Ok(LoopDefinition {
            name: arguments[0].clone(),
            values,
            concurrent,
        })
    } else {
        if arguments.len() != 3 {
            return Err(Error::Execution("expected loop ITERATOR START END".into()));
        }
        let start = parse_stoi(&arguments[1])?;
        let end = parse_stoi(&arguments[2])?;
        // The development pin stores the stoi result in idx_t. Preserve that
        // unsigned conversion and the post-body wrapping increment; v1.5.5
        // used signed int here, but development behavior is authoritative.
        let mut current = start as u64;
        let end = end as u64;
        let mut values = Vec::new();
        loop {
            if values.len() == MAX_LOOP_ITERATIONS {
                return Err(Error::Unsupported(
                    "loop expansion exceeds record limit".into(),
                ));
            }
            values.push(current.to_string());
            current = current.wrapping_add(1);
            if current >= end {
                break;
            }
        }
        Ok(LoopDefinition {
            name: arguments[0].clone(),
            values,
            concurrent,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_stoi(value: &str) -> Result<i32> {
    let value = value.trim_start();
    let bytes = value.as_bytes();
    let mut end = usize::from(matches!(bytes.first(), Some(b'+' | b'-')));
    let first_digit = end;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if end == first_digit {
        return Err(Error::Execution(
            "loop start and end must be numbers".into(),
        ));
    }
    value[..end]
        .parse()
        .map_err(|_| Error::Execution("loop start and end must be numbers".into()))
}

/// Expand the collections present in both pinned runners. Unknown angle-bracket
/// tokens remain literal; the engine/runtime registry is not guessed here.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn expand_foreach_token<F>(token: &str, output: &mut Vec<String>, variable: &mut F) -> Result<()>
where
    F: FnMut(&str) -> Result<Vec<String>>,
{
    let lower = token.trim().to_ascii_lowercase();
    let signed = ["tinyint", "smallint", "integer", "bigint", "hugeint"];
    let unsigned = ["utinyint", "usmallint", "uinteger", "ubigint", "uhugeint"];
    let floating = ["float", "double"];
    const ALL_COLUMNS: &[&str] = &[
        "bool",
        "tinyint",
        "smallint",
        "int",
        "bigint",
        "hugeint",
        "uhugeint",
        "utinyint",
        "usmallint",
        "uint",
        "ubigint",
        "date",
        "time",
        "timestamp",
        "timestamp_s",
        "timestamp_ms",
        "timestamp_ns",
        "time_tz",
        "timestamp_tz",
        "float",
        "double",
        "dec_4_1",
        "dec_9_4",
        "dec_18_6",
        "dec38_10",
        "uuid",
        "interval",
        "varchar",
        "blob",
        "bit",
        "small_enum",
        "medium_enum",
        "large_enum",
        "int_array",
        "double_array",
        "date_array",
        "timestamp_array",
        "timestamptz_array",
        "varchar_array",
        "nested_int_array",
        "struct",
        "struct_of_arrays",
        "array_of_structs",
        "map",
        "union",
        "fixed_int_array",
        "fixed_varchar_array",
        "fixed_nested_int_array",
        "fixed_nested_varchar_array",
        "fixed_struct_array",
        "struct_of_fixed_array",
        "fixed_array_of_int_list",
        "list_of_fixed_int_array",
    ];
    match lower.as_str() {
        "<signed>" => output.extend(signed.map(str::to_string)),
        "<unsigned>" => output.extend(unsigned.map(str::to_string)),
        "<integral>" => output.extend(signed.into_iter().chain(unsigned).map(str::to_string)),
        "<numeric>" => output.extend(
            signed
                .into_iter()
                .chain(unsigned)
                .chain(floating)
                .map(str::to_string),
        ),
        "<alltypes>" => output.extend(
            signed
                .into_iter()
                .chain(unsigned)
                .chain(floating)
                .chain(["bool", "interval", "varchar"])
                .map(str::to_string),
        ),
        "<compression>" => output.extend(
            [
                "none",
                "uncompressed",
                "rle",
                "bitpacking",
                "dictionary",
                "fsst",
                "dict_fsst",
                "alp",
                "alprd",
            ]
            .map(str::to_string),
        ),
        "<all_types_columns>" => {
            output.extend(ALL_COLUMNS.iter().map(|value| (*value).to_string()))
        }
        value if value.starts_with("<variable:") => {
            if !value.ends_with('>') {
                return Err(Error::Execution(format!(
                    "invalid foreach variable {token}"
                )));
            }
            output.extend(variable(&token["<variable:".len()..token.len() - 1])?);
        }
        value if value.starts_with('!') => {
            let remove = &token[1..];
            if let Some(index) = output.iter().position(|value| value == remove) {
                output.remove(index);
            } else {
                output.push(token.to_string());
            }
        }
        _ => output.push(token.to_string()),
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn parse_condition(
    text: &str,
    skip_if: bool,
    original_sqlite: bool,
) -> Result<Condition> {
    if original_sqlite {
        return Ok(Condition::System {
            skip_if,
            name: text.to_ascii_lowercase(),
        });
    }
    // Pinned TryParseConditions accepts a conjunction of comparisons. The
    // caller splits `&&` and stores each comparison independently.
    const OPERATORS: [(&str, Comparison); 6] = [
        ("<>", Comparison::NotEqual),
        (">=", Comparison::GreaterEqual),
        (">", Comparison::Greater),
        ("<=", Comparison::LessEqual),
        ("<", Comparison::Less),
        ("=", Comparison::Equal),
    ];
    for (operator, comparison) in OPERATORS {
        if let Some(index) = text.find(operator) {
            if text[index + operator.len()..].contains(operator) {
                return Err(Error::Execution(format!("invalid loop condition {text:?}")));
            }
            let iterator = &text[..index];
            let value = &text[index + operator.len()..];
            if iterator.is_empty() || value.is_empty() {
                return Err(Error::Execution(format!("invalid loop condition {text:?}")));
            }
            return Ok(Condition::Loop {
                skip_if,
                iterator: iterator.to_string(),
                comparison,
                value: value.to_string(),
            });
        }
    }
    Ok(Condition::System {
        skip_if,
        name: text.to_ascii_lowercase(),
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn selected(
    conditions: &[Condition],
    loops: &[LoopFrame],
    original_sqlite: bool,
) -> Result<bool> {
    for condition in conditions {
        let holds = match condition {
            Condition::System { name, .. } => {
                name == "duckdb" || (original_sqlite && name == "postgresql")
            }
            Condition::Loop {
                iterator,
                comparison,
                value,
                ..
            } => {
                let frame = loops
                    .iter()
                    .rev()
                    .find(|frame| &frame.name == iterator)
                    .ok_or_else(|| {
                        Error::Bind(format!(
                            "Condition in onlyif/skipif not found: {iterator} must be a loop iterator name"
                        ))
                    })?;
                match comparison {
                    Comparison::Equal => frame.value == *value,
                    Comparison::NotEqual => frame.value != *value,
                    comparison => {
                        let left: i64 = frame.value.parse().map_err(|_| {
                            Error::Execution(format!(
                                "loop value {:?} is not a signed 64-bit number",
                                frame.value
                            ))
                        })?;
                        let right: i64 = value.parse().map_err(|_| {
                            Error::Execution(format!(
                                "loop condition value {value:?} is not a signed 64-bit number"
                            ))
                        })?;
                        match comparison {
                            Comparison::Less => left < right,
                            Comparison::LessEqual => left <= right,
                            Comparison::Greater => left > right,
                            Comparison::GreaterEqual => left >= right,
                            Comparison::Equal | Comparison::NotEqual => unreachable!(),
                        }
                    }
                }
            }
        };
        let skip_if = match condition {
            Condition::System { skip_if, .. } | Condition::Loop { skip_if, .. } => *skip_if,
        };
        if holds == skip_if {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn replace_loops(mut input: Vec<u8>, loops: &[LoopFrame]) -> Result<Vec<u8>> {
    for frame in loops {
        let names: Vec<_> = frame.name.split(',').collect();
        let values: Vec<_> = frame.value.split(',').collect();
        if names.len() != values.len() {
            return Err(Error::Execution(format!(
                "foreach loop: number of commas in iterator ({}) does not match replacement ({})",
                frame.name, frame.value
            )));
        }
        for (name, value) in names.into_iter().zip(values) {
            for marker in [format!("${{{name}}}"), format!("{{{name}}}")] {
                input = replace_all(&input, marker.as_bytes(), value.as_bytes());
            }
        }
    }
    Ok(input)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn replace_all(input: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
    if needle.is_empty() {
        return input.to_vec();
    }
    let mut result = Vec::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        if input[cursor..].starts_with(needle) {
            result.extend_from_slice(replacement);
            cursor += needle.len();
        } else {
            result.push(input[cursor]);
            cursor += 1;
        }
    }
    result
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn shuffled_indexes(count: usize) -> Vec<usize> {
    let state = std::collections::hash_map::RandomState::new();
    let mut random = state.build_hasher();
    random.write_usize(count);
    let mut indexes: Vec<_> = (0..count).collect();
    for upper in (1..count).rev() {
        let next = random.finish() as usize % (upper + 1);
        indexes.swap(upper, next);
        random.write_usize(upper);
    }
    indexes
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Shuffle and launch every iteration with a distinct connection owner. The
/// pinned runner joins every spawned sibling and reports failures afterwards.
/// Callers can share its opportunistic stop flag, but this scheduler never
/// interrupts an already running thread.
pub(crate) fn run_concurrent<T, F>(count: usize, run: F) -> Result<Vec<Result<T>>>
where
    T: Send,
    F: Fn(usize) -> Result<T> + Sync,
{
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut values: Vec<Option<Result<T>>> = (0..count).map(|_| None).collect();
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(count);
        for index in shuffled_indexes(count) {
            let run = &run;
            handles.push((index, scope.spawn(move || run(index))));
        }
        for (index, handle) in handles {
            values[index] = Some(
                handle
                    .join()
                    .map_err(|_| Error::Internal("concurrent loop worker panicked".into()))?,
            );
        }
        Ok::<_, Error>(())
    })?;
    Ok(values.into_iter().map(Option::unwrap).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn half_open_loops_conditions_and_tuple_substitution_match_source() -> Result<()> {
        assert_eq!(
            parse_loop(&["i".into(), "2".into(), "5".into()], false, false)?.values,
            ["2", "3", "4"]
        );
        assert_eq!(
            parse_loop(&["i".into(), "5".into(), "2".into()], false, false)?.values,
            ["5"]
        );
        assert_eq!(
            parse_loop(&["i".into(), "-1".into(), "2".into()], false, false)?.values,
            [u64::MAX.to_string(), "0".into(), "1".into()]
        );
        assert!(parse_loop(&["i".into(), "2147483648".into(), "3".into()], false, false).is_err());
        assert_eq!(
            parse_loop(&["i".into(), "1tail".into(), "3tail".into()], false, false)?.values,
            ["1", "2"]
        );
        assert!(parse_loop(&["i".into(), "a".into(), "!a".into()], false, true).is_err());
        assert!(parse_condition("i==1", false, false).is_err());
        assert!(matches!(
            parse_condition("i!=1", false, false)?,
            Condition::Loop { iterator, .. } if iterator == "i!"
        ));
        let collections = parse_loop_with(
            &[
                "kind".into(),
                "<all_types_columns>".into(),
                "<compression>".into(),
                "<variable:choices>".into(),
            ],
            false,
            true,
            |name| {
                assert_eq!(name, "choices");
                Ok(vec!["x,y".into()])
            },
        )?;
        assert_eq!(collections.values.len(), 53 + 9 + 1);
        let frames = [LoopFrame {
            name: "left,right".into(),
            value: "a,b".into(),
            ordinal: 0,
            concurrent: false,
        }];
        assert_eq!(replace_loops(b"{left}-${right}".to_vec(), &frames)?, b"a-b");
        let conditions = [parse_condition("left,right=a,b", false, false)?];
        assert!(selected(&conditions, &frames, false)?);
        assert!(
            replace_loops(
                b"{left}".to_vec(),
                &[LoopFrame {
                    value: "a,b,c".into(),
                    ..frames[0].clone()
                }]
            )
            .is_err()
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn concurrent_scheduler_joins_every_sibling_after_failure() {
        let entered = AtomicUsize::new(0);
        let completed = AtomicUsize::new(0);
        let result = run_concurrent(4, |index| {
            entered.fetch_add(1, Ordering::SeqCst);
            if index == 0 {
                return Err(Error::Execution("sentinel".into()));
            }
            completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .unwrap();
        assert!(result[0].is_err());
        assert!(result[1..].iter().all(Result::is_ok));
        assert_eq!(entered.load(Ordering::SeqCst), 4);
        assert_eq!(completed.load(Ordering::SeqCst), 3);
    }
}
