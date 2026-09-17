//! RE2-compatible regular-expression predicates.
//!
//! Patterns and subject strings are Rust `String`s, so their byte lengths are
//! retained exactly: embedded NUL is data rather than a terminator.  The
//! selected regex crate is deliberately pinned alongside the reference's
//! Unicode version; do not replace this with a C-string based adapter.

use std::sync::Arc;

use regex::{Regex, RegexBuilder};

use super::VarcharBatch;
use crate::{
    common::{
        DataType, Error, Result, Value,
        cast::CastMode,
        type_registry::TypeRegistry,
        vector::{DataChunk, Vector},
    },
    function::{FunctionRegistry, ScalarBindArguments, ScalarFunction},
    parallel::QueryContext,
};

#[derive(Clone, Copy, Debug)]
enum MatchKind {
    Partial,
    Full,
}

#[derive(Clone, Copy, Debug, Default)]
struct RegexOptions {
    case_insensitive: bool,
    literal: bool,
    dot_matches_new_line: bool,
}

#[derive(Clone, Debug)]
enum CompiledPattern {
    Regex(Regex),
    Literal(LiteralMatch),
    AsciiInsensitive {
        literal: LiteralMatch,
        fallback: Regex,
    },
}

#[derive(Clone, Debug)]
enum LiteralMatch {
    Contains(String),
    Exact(String),
    Prefix(String),
    Suffix(String),
}

#[derive(Clone, Debug)]
struct RegexPredicate {
    name: &'static str,
    kind: MatchKind,
    options: RegexOptions,
    constant_pattern: Option<CompiledPattern>,
    signature: Option<Vec<DataType>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for (name, kind) in [
        ("regexp_matches", MatchKind::Partial),
        ("regexp_full_match", MatchKind::Full),
    ] {
        registry
            .register_scalar(Arc::new(RegexPredicate {
                name,
                kind,
                options: RegexOptions::default(),
                constant_pattern: None,
                signature: None,
            }))
            .expect("unique regexp predicate function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for RegexPredicate {
    fn name(&self) -> &str {
        self.name
    }

    fn batch_kind(
        &self,
        _: crate::function::ScalarBatchAccess,
    ) -> Option<crate::function::ScalarBatchKind> {
        Some(crate::function::ScalarBatchKind::builtin(
            crate::function::ScalarBatchIdentity::RegexPredicate,
        ))
    }

    fn select_batch(
        &self,
        _: crate::function::ScalarBatchAccess,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vec<usize>>> {
        self.select_matches(arguments, query).map(Some)
    }

    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        if !matches!(arguments.len(), 2 | 3) {
            return Err(Error::Bind(format!(
                "{} requires a string, regex and optional options",
                self.name
            )));
        }
        let signature = (0..arguments.len())
            .map(|index| arguments.data_type(index))
            .collect::<Result<Vec<_>>>()?;
        if !signature
            .iter()
            .all(|data_type| matches!(data_type, DataType::Varchar | DataType::Null))
        {
            return Err(Error::Bind(format!(
                "{} requires VARCHAR arguments",
                self.name
            )));
        }

        let options = if arguments.len() == 3 {
            if !arguments.is_closed(2)? {
                return Err(Error::Bind(format!(
                    "Regex options field for {} must be a constant expression",
                    self.name
                )));
            }
            parse_options(arguments.constant_as(2, &DataType::Varchar, CastMode::Implicit)?)?
        } else {
            RegexOptions::default()
        };

        // Native binding stores a prepared RE2 only for closed non-NULL
        // patterns. NULL patterns preserve ordinary NULL propagation and must
        // not turn into a bind-time compile error.
        let constant_pattern = arguments
            .constant_if_closed(1)?
            .map(|value| match value {
                Value::Null => Ok(None),
                Value::Varchar(pattern) => compile_pattern(&pattern, options, self.kind).map(Some),
                _ => Err(Error::Internal(
                    "regexp constant pattern is not VARCHAR".into(),
                )),
            })
            .transpose()?
            .flatten();
        query.check()?;
        Ok(Some(Arc::new(Self {
            name: self.name,
            kind: self.kind,
            options,
            constant_pattern,
            signature: Some(vec![DataType::Varchar; arguments.len()]),
        })))
    }

    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        let signature = self.signature.as_ref().ok_or_else(|| {
            Error::Unsupported("regexp predicates require selected statement-local binding".into())
        })?;
        if arguments.len() != signature.len() {
            return Err(Error::Internal("bound regexp predicate arity".into()));
        }
        Ok(signature.clone())
    }

    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        let signature = self.signature.as_ref().ok_or_else(|| {
            Error::Unsupported("regexp predicates require selected statement-local binding".into())
        })?;
        if arguments == signature {
            Ok(DataType::Boolean)
        } else {
            Err(Error::Bind(format!(
                "no overload for {}({arguments:?})",
                self.name
            )))
        }
    }

    fn is_total(&self, _: &[Option<&Value>]) -> bool {
        // A successfully compiled statement-local pattern cannot report a
        // row-dependent error. Dynamic patterns retain scalar error order.
        self.constant_pattern.is_some()
    }

    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        matches!(
            arguments,
            [DataType::Varchar, DataType::Varchar]
                | [DataType::Varchar, DataType::Varchar, DataType::Varchar]
        )
    }

    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        let types = arguments
            .columns()
            .iter()
            .map(|column| column.data_type().clone())
            .collect::<Vec<_>>();
        if !self.supports_batch_evaluation(&types) {
            return Ok(None);
        }
        let input = &arguments.columns()[0];
        let pattern = &arguments.columns()[1];
        let (Some(input), Some(pattern)) = (VarcharBatch::new(input), VarcharBatch::new(pattern))
        else {
            return self.batch_fallback(arguments, query).map(Some);
        };
        let mut output = Vec::new();
        output
            .try_reserve_exact(arguments.len())
            .map_err(|_| Error::Resource("cannot allocate regexp predicate result".into()))?;
        let mut cache = Vec::<(String, CompiledPattern)>::new();
        for index in 0..arguments.len() {
            if index % 1024 == 0 {
                query.check()?;
            }
            output.push(self.match_values(input.get(index)?, pattern.get(index)?, &mut cache)?);
        }
        query.check()?;
        Vector::flat(DataType::Boolean, output).map(Some)
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let expected = self
            .signature
            .as_ref()
            .ok_or_else(|| Error::Internal("unbound regexp predicate".into()))?;
        if arguments.len() != expected.len() {
            return Err(Error::Internal("regexp predicate argument count".into()));
        }
        let [input, pattern, ..] = arguments else {
            return Err(Error::Internal("regexp predicate argument shape".into()));
        };
        match (input, pattern) {
            (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
            (Value::Varchar(input), Value::Varchar(pattern)) => {
                let matched = match &self.constant_pattern {
                    Some(pattern) => pattern.is_match(input),
                    None => compile_pattern(pattern, self.options, self.kind)?.is_match(input),
                };
                Ok(Value::Boolean(matched))
            }
            _ => Err(Error::Internal(
                "regexp predicate arguments are not VARCHAR".into(),
            )),
        }
    }
}

impl RegexPredicate {
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn select_matches(&self, arguments: &DataChunk, query: &QueryContext) -> Result<Vec<usize>> {
        let [input, pattern, ..] = arguments.columns() else {
            return Err(Error::Internal(
                "regexp selection argument count changed after binding".into(),
            ));
        };
        if input.data_type() != &DataType::Varchar || pattern.data_type() != &DataType::Varchar {
            return Err(Error::Internal(
                "regexp selection types changed after binding".into(),
            ));
        }
        let input_batch = VarcharBatch::new(input);
        let pattern_batch = VarcharBatch::new(pattern);
        let mut selected = Vec::new();
        selected
            .try_reserve_exact(arguments.len())
            .map_err(|_| Error::Resource("cannot allocate regexp selection".into()))?;
        let mut cache = Vec::new();
        for index in 0..arguments.len() {
            if index % 1024 == 0 {
                query.check()?;
            }
            let matched = if let (Some(input), Some(pattern)) = (input_batch, pattern_batch) {
                self.match_values(input.get(index)?, pattern.get(index)?, &mut cache)?
            } else {
                let input = input.value(index).ok_or_else(|| {
                    Error::Internal("regexp input selection is out of bounds".into())
                })?;
                let pattern = pattern.value(index).ok_or_else(|| {
                    Error::Internal("regexp pattern selection is out of bounds".into())
                })?;
                self.match_values(&input, &pattern, &mut cache)?
            };
            if matched == Value::Boolean(true) {
                selected.push(index);
            }
        }
        query.check()?;
        Ok(selected)
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn match_values(
        &self,
        input: &Value,
        pattern: &Value,
        cache: &mut Vec<(String, CompiledPattern)>,
    ) -> Result<Value> {
        let (Value::Varchar(input), Value::Varchar(pattern)) = (input, pattern) else {
            return if input.is_null() || pattern.is_null() {
                Ok(Value::Null)
            } else {
                Err(Error::Internal(
                    "regexp predicate arguments are not VARCHAR".into(),
                ))
            };
        };
        if let Some(pattern) = &self.constant_pattern {
            return Ok(Value::Boolean(pattern.is_match(input)));
        }
        if let Some((_, compiled)) = cache.iter().find(|(candidate, _)| candidate == pattern) {
            return Ok(Value::Boolean(compiled.is_match(input)));
        }
        let compiled = compile_pattern(pattern, self.options, self.kind)?;
        let matched = compiled.is_match(input);
        if cache.len() < 32 {
            cache.push((pattern.clone(), compiled));
        }
        Ok(Value::Boolean(matched))
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn batch_fallback(&self, arguments: &DataChunk, query: &QueryContext) -> Result<Vector> {
        let mut row = Vec::with_capacity(arguments.columns().len());
        let mut output = Vec::new();
        output
            .try_reserve_exact(arguments.len())
            .map_err(|_| Error::Resource("cannot allocate regexp predicate result".into()))?;
        let mut cache = Vec::new();
        for index in 0..arguments.len() {
            if index % 1024 == 0 {
                query.check()?;
            }
            arguments.read_row(index, &mut row)?;
            output.push(self.match_values(&row[0], &row[1], &mut cache)?);
        }
        query.check()?;
        Vector::flat(DataType::Boolean, output)
    }
}

impl CompiledPattern {
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn is_match(&self, input: &str) -> bool {
        match self {
            Self::Regex(regex) => regex.is_match(input),
            Self::Literal(kind) => kind.is_match(input),
            Self::AsciiInsensitive { literal, fallback } => {
                if input.is_ascii() {
                    literal.is_ascii_case_insensitive_match(input)
                } else {
                    fallback.is_match(input)
                }
            }
        }
    }
}

impl LiteralMatch {
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn is_match(&self, input: &str) -> bool {
        match self {
            Self::Contains(literal) => input.contains(literal),
            Self::Exact(literal) => input == literal,
            Self::Prefix(literal) => input.starts_with(literal),
            Self::Suffix(literal) => input.ends_with(literal),
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn is_ascii_case_insensitive_match(&self, input: &str) -> bool {
        let contains = |literal: &str| {
            literal.is_empty()
                || input
                    .as_bytes()
                    .windows(literal.len())
                    .any(|candidate| candidate.eq_ignore_ascii_case(literal.as_bytes()))
        };
        match self {
            Self::Contains(literal) => contains(literal),
            Self::Exact(literal) => input.eq_ignore_ascii_case(literal),
            Self::Prefix(literal) => input
                .get(..literal.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(literal)),
            Self::Suffix(literal) => input
                .get(input.len().saturating_sub(literal.len())..)
                .is_some_and(|suffix| {
                    input.len() >= literal.len() && suffix.eq_ignore_ascii_case(literal)
                }),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_options(value: Value) -> Result<RegexOptions> {
    let Value::Varchar(options) = value else {
        return if value.is_null() {
            Err(Error::InvalidInput(
                "Regex options field must not be NULL".into(),
            ))
        } else {
            Err(Error::InvalidInput(
                "Regex options field must be a string".into(),
            ))
        };
    };
    let mut parsed = RegexOptions::default();
    for option in options.chars() {
        match option {
            'c' => parsed.case_insensitive = false,
            'i' => parsed.case_insensitive = true,
            'l' => parsed.literal = true,
            // RE2's m/n/p each retain the normal, newline-sensitive dot.
            'm' | 'n' | 'p' => parsed.dot_matches_new_line = false,
            's' => parsed.dot_matches_new_line = true,
            ' ' | '\t' | '\n' => {}
            'g' => {
                return Err(Error::InvalidInput(
                    "Option 'g' (global replace) is only valid for regexp_replace".into(),
                ));
            }
            'k' => {
                return Err(Error::InvalidInput(
                    "Option 'k' (keep input on no match) is only valid for regexp_extract".into(),
                ));
            }
            option => {
                return Err(Error::InvalidInput(format!(
                    "Unrecognized Regex option {option}"
                )));
            }
        }
    }
    Ok(parsed)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compile(pattern: &str, options: RegexOptions, kind: MatchKind) -> Result<Regex> {
    let pattern = if options.literal {
        regex::escape(pattern)
    } else {
        pattern.to_owned()
    };
    // RE2 FullMatch matches the whole input independent of a pattern's own
    // anchors. Rust regex's absolute anchors give the same no-trailing-newline
    // behavior as RE2 rather than the line-oriented `$` behavior.
    let pattern = match kind {
        MatchKind::Partial => pattern,
        MatchKind::Full => format!(r"\A(?:{pattern})\z"),
    };
    let mut builder = RegexBuilder::new(&pattern);
    builder
        .case_insensitive(options.case_insensitive)
        .dot_matches_new_line(options.dot_matches_new_line);
    builder
        .build()
        .map_err(|error| Error::InvalidInput(error.to_string()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compile_pattern(
    pattern: &str,
    options: RegexOptions,
    kind: MatchKind,
) -> Result<CompiledPattern> {
    if options.case_insensitive && pattern.is_ascii() {
        let mut literal_options = options;
        literal_options.case_insensitive = false;
        if let Some(literal) = literal_match(pattern, literal_options, kind) {
            return Ok(CompiledPattern::AsciiInsensitive {
                literal,
                fallback: compile(pattern, options, kind)?,
            });
        }
    }
    if let Some(literal) = literal_match(pattern, options, kind) {
        return Ok(CompiledPattern::Literal(literal));
    }
    compile(pattern, options, kind).map(CompiledPattern::Regex)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn literal_match(pattern: &str, options: RegexOptions, kind: MatchKind) -> Option<LiteralMatch> {
    if options.case_insensitive {
        return None;
    }
    if options.literal {
        return Some(match kind {
            MatchKind::Partial => LiteralMatch::Contains(pattern.to_owned()),
            MatchKind::Full => LiteralMatch::Exact(pattern.to_owned()),
        });
    }
    let is_literal = |text: &str| {
        !text.bytes().any(|byte| {
            matches!(
                byte,
                b'.' | b'+'
                    | b'*'
                    | b'?'
                    | b'('
                    | b')'
                    | b'|'
                    | b'['
                    | b']'
                    | b'{'
                    | b'}'
                    | b'\\'
                    | b'^'
                    | b'$'
            )
        })
    };
    match kind {
        MatchKind::Full => {
            let literal = pattern
                .strip_prefix('^')
                .and_then(|pattern| pattern.strip_suffix('$'))
                .unwrap_or(pattern);
            is_literal(literal).then(|| LiteralMatch::Exact(literal.to_owned()))
        }
        MatchKind::Partial => {
            if let Some(literal) = pattern
                .strip_prefix('^')
                .and_then(|pattern| pattern.strip_suffix('$'))
                .filter(|literal| is_literal(literal))
            {
                Some(LiteralMatch::Exact(literal.to_owned()))
            } else if let Some(literal) = pattern
                .strip_prefix('^')
                .filter(|literal| is_literal(literal))
            {
                Some(LiteralMatch::Prefix(literal.to_owned()))
            } else if let Some(literal) = pattern
                .strip_suffix('$')
                .filter(|literal| is_literal(literal))
            {
                Some(LiteralMatch::Suffix(literal.to_owned()))
            } else {
                is_literal(pattern).then(|| LiteralMatch::Contains(pattern.to_owned()))
            }
        }
    }
}
