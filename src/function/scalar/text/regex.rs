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
    global: bool,
    keep: bool,
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
    for name in ["regexp_replace", "regexp_extract", "regexp_escape"] {
        registry
            .register_scalar(Arc::new(RegexValueFunction {
                name,
                signature: None,
                options: RegexOptions::default(),
                constant: None,
                extract_group: None,
                extract_group_is_null: false,
                constant_replacement: None,
            }))
            .expect("unique regexp value function");
    }
}

/// The value functions intentionally share compilation with predicates, but do
/// not use predicate selection: a replacement/extraction must retain every row.
#[derive(Clone, Debug)]
struct RegexValueFunction {
    name: &'static str,
    signature: Option<Vec<DataType>>,
    options: RegexOptions,
    constant: Option<Regex>,
    extract_group: Option<usize>,
    extract_group_is_null: bool,
    constant_replacement: Option<String>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for RegexValueFunction {
    fn name(&self) -> &str {
        self.name
    }
    fn bind(
        &self,
        args: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        let n = args.len();
        let valid = match self.name {
            "regexp_escape" => n == 1,
            "regexp_replace" => matches!(n, 3 | 4),
            "regexp_extract" => matches!(n, 2 | 3 | 4),
            _ => false,
        };
        if !valid {
            return Err(Error::Bind(format!(
                "invalid argument count for {}",
                self.name
            )));
        }
        if self.name == "regexp_extract"
            && n == 4
            && matches!(args.data_type(2)?, DataType::Varchar)
        {
            return Err(Error::Bind(
                "Could not choose a best candidate function for regexp_extract".into(),
            ));
        }
        let mut sig = Vec::with_capacity(n);
        for i in 0..n {
            let ty = args.data_type(i)?;
            let expect_int = self.name == "regexp_extract"
                && ((n == 3 && i == 2 && !matches!(ty, DataType::Varchar)) || (n == 4 && i == 2));
            if expect_int {
                sig.push(DataType::BigInt);
            } else if matches!(ty, DataType::Varchar | DataType::Null) {
                sig.push(DataType::Varchar);
            } else {
                return Err(Error::Bind(format!(
                    "{} requires VARCHAR arguments",
                    self.name
                )));
            }
        }
        let options_index = match self.name {
            "regexp_replace" if n == 4 => Some(3),
            "regexp_extract" if n == 3 && sig[2] == DataType::Varchar => Some(2),
            "regexp_extract" if n == 4 => Some(3),
            _ => None,
        };
        let options = match options_index {
            Some(i) => {
                if !args.is_closed(i)? {
                    return Err(Error::Bind(format!(
                        "Regex options field for {} must be a constant expression",
                        self.name
                    )));
                }
                parse_options_for(
                    args.constant_as(i, &DataType::Varchar, CastMode::Implicit)?,
                    self.name == "regexp_replace",
                    self.name == "regexp_extract",
                )?
            }
            None => RegexOptions::default(),
        };
        let constant = if self.name == "regexp_escape" {
            None
        } else {
            args.constant_if_closed(1)?
                .map(|v| match v {
                    Value::Null => Ok(None),
                    Value::Varchar(p) => compile(&p, options, MatchKind::Partial).map(Some),
                    _ => Err(Error::Internal("regexp pattern not VARCHAR".into())),
                })
                .transpose()?
                .flatten()
        };
        let mut extract_group_is_null = false;
        let extract_group = if self.name == "regexp_extract" && n >= 3 && sig[2] == DataType::BigInt
        {
            if !args.is_closed(2)? {
                return Err(Error::Bind(
                    "regexp_extract group must be a constant expression".into(),
                ));
            }
            match args.constant_as(2, &DataType::BigInt, CastMode::Implicit)? {
                Value::Null => {
                    extract_group_is_null = true;
                    Some(0)
                }
                Value::Integer(value) => {
                    let group = usize::try_from(value).map_err(|_| {
                        Error::InvalidInput("Group index must be between 0 and 9".into())
                    })?;
                    if group > 9 {
                        return Err(Error::InvalidInput(
                            "Group index must be between 0 and 9".into(),
                        ));
                    }
                    Some(group)
                }
                _ => return Err(Error::Internal("regexp_extract group is not BIGINT".into())),
            }
        } else {
            None
        };
        let constant_replacement = if self.name == "regexp_replace" {
            match (args.constant_if_closed(2)?, constant.as_ref()) {
                (Some(Value::Varchar(replacement)), Some(pattern)) => {
                    Some(re2_replacement(&replacement, pattern.captures_len())?)
                }
                (Some(Value::Null) | None, _) | (_, None) => None,
                (Some(_), Some(_)) => {
                    return Err(Error::Internal(
                        "regexp replacement constant is not VARCHAR".into(),
                    ));
                }
            }
        } else {
            None
        };
        Ok(Some(Arc::new(Self {
            name: self.name,
            signature: Some(sig),
            options,
            constant,
            extract_group,
            extract_group_is_null,
            constant_replacement,
        })))
    }
    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        self.signature
            .as_ref()
            .filter(|s| s.len() == arguments.len())
            .cloned()
            .ok_or_else(|| Error::Bind(format!("no overload for {}({arguments:?})", self.name)))
    }
    fn return_type(&self, _: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        Ok(DataType::Varchar)
    }
    fn is_total(&self, _: &[Option<&Value>]) -> bool {
        self.name == "regexp_escape"
    }
    fn supports_batch_evaluation(&self, _: &[DataType]) -> bool {
        true
    }
    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        // The common replacement shape has a statement-local pattern and
        // replacement.  Reading a full row for it needlessly clones those two
        // constants and goes through the dynamic-pattern cache for every
        // input.  Keep this path deliberately narrow: the generic evaluator
        // remains responsible for dynamic values and their first-error order.
        if let Some(output) = self.constant_replace_batch(arguments, query)? {
            return Ok(Some(output));
        }
        let mut row = Vec::with_capacity(arguments.columns().len());
        let mut out = Vec::new();
        out.try_reserve_exact(arguments.len())
            .map_err(|_| Error::Resource("cannot allocate regexp value function result".into()))?;
        // A small per-batch cache preserves first-error order while avoiding repeated dynamic compilation.
        let mut cache: Vec<(String, Regex)> = Vec::new();
        for i in 0..arguments.len() {
            if i % 1024 == 0 {
                query.check()?;
            }
            arguments.read_row(i, &mut row)?;
            out.push(self.apply(&row, &mut cache)?);
        }
        Vector::flat(DataType::Varchar, out).map(Some)
    }
    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        self.apply(arguments, &mut Vec::new())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl RegexValueFunction {
    fn constant_replace_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        if self.name != "regexp_replace" {
            return Ok(None);
        }
        let (Some(regex), Some(replacement), Some(input)) = (
            self.constant.as_ref(),
            self.constant_replacement.as_deref(),
            arguments.columns().first().and_then(VarcharBatch::new),
        ) else {
            return Ok(None);
        };

        let mut output = Vec::new();
        output
            .try_reserve_exact(arguments.len())
            .map_err(|_| Error::Resource("cannot allocate regexp replacement result".into()))?;
        for index in 0..arguments.len() {
            if index % 1024 == 0 {
                query.check()?;
            }
            let value = input.get(index)?;
            let Value::Varchar(value) = value else {
                if value.is_null() {
                    output.push(Value::Null);
                    continue;
                }
                return Err(Error::Internal(
                    "regexp replacement input is not VARCHAR".into(),
                ));
            };
            let replaced = if self.options.global {
                regex.replace_all(value, replacement)
            } else {
                regex.replace(value, replacement)
            };
            output.push(Value::Varchar(replaced.into_owned()));
        }
        query.check()?;
        Vector::flat(DataType::Varchar, output).map(Some)
    }

    fn apply(&self, a: &[Value], cache: &mut Vec<(String, Regex)>) -> Result<Value> {
        if self.name == "regexp_extract" && self.extract_group_is_null {
            return if a.first().is_some_and(Value::is_null) || a.get(1).is_some_and(Value::is_null)
            {
                Ok(Value::Null)
            } else {
                Ok(Value::Varchar(String::new()))
            };
        }
        if a.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        if self.name == "regexp_escape" {
            return match a {
                [Value::Varchar(s)] => Ok(Value::Varchar(re2_escape(s))),
                _ => Err(Error::Internal("regexp_escape argument".into())),
            };
        }
        let (Value::Varchar(input), Value::Varchar(pattern)) = (&a[0], &a[1]) else {
            return Err(Error::Internal("regexp value VARCHAR arguments".into()));
        };
        let regex = match &self.constant {
            Some(r) => r,
            None => {
                if let Some((_, r)) = cache.iter().find(|(p, _)| p == pattern) {
                    r
                } else {
                    let r = compile(pattern, self.options, MatchKind::Partial)?;
                    if cache.len() < 32 {
                        cache.push((pattern.clone(), r));
                        cache.last().map(|x| &x.1).unwrap()
                    } else {
                        return self.apply_uncached(input, pattern, a);
                    }
                }
            }
        };
        self.apply_regex(input, regex, a)
    }
    fn apply_uncached(&self, input: &str, pattern: &str, a: &[Value]) -> Result<Value> {
        let r = compile(pattern, self.options, MatchKind::Partial)?;
        self.apply_regex(input, &r, a)
    }
    fn apply_regex(&self, input: &str, regex: &Regex, a: &[Value]) -> Result<Value> {
        match self.name {
            "regexp_replace" => {
                let Value::Varchar(replacement) = &a[2] else {
                    return Err(Error::Internal("regexp replacement".into()));
                };
                let dynamic_replacement;
                let replacement = if let Some(replacement) = &self.constant_replacement {
                    replacement.as_str()
                } else {
                    dynamic_replacement = re2_replacement(replacement, regex.captures_len())?;
                    dynamic_replacement.as_str()
                };
                let result = if self.options.global {
                    regex.replace_all(input, replacement)
                } else {
                    regex.replace(input, replacement)
                };
                Ok(Value::Varchar(result.into_owned()))
            }
            "regexp_extract" => {
                let group = self.extract_group.unwrap_or(0);
                if group >= regex.captures_len() {
                    return if self.options.keep {
                        Ok(Value::Varchar(input.to_owned()))
                    } else {
                        Ok(Value::Varchar(String::new()))
                    };
                }
                match regex.captures(input).and_then(|c| c.get(group)) {
                    Some(m) => Ok(Value::Varchar(m.as_str().to_owned())),
                    None if self.options.keep => Ok(Value::Varchar(input.to_owned())),
                    None => Ok(Value::Varchar(String::new())),
                }
            }
            _ => Err(Error::Internal("unknown regexp value function".into())),
        }
    }
}

// RE2 uses \\1 while Rust regex uses $1. Preserve escaped non-group text.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn re2_replacement(value: &str, captures_len: usize) -> Result<String> {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            if c == '$' {
                out.push_str("$$");
            } else {
                out.push(c);
            }
            continue;
        }
        let Some(next) = chars.next() else {
            return Err(Error::InvalidInput(
                "invalid regexp replacement trailing backslash".into(),
            ));
        };
        if next == '\\' {
            out.push('\\');
            continue;
        }
        if !next.is_ascii_digit() {
            return Err(Error::InvalidInput(
                "invalid regexp replacement backreference".into(),
            ));
        }
        let group = next.to_digit(10).unwrap() as usize;
        if group >= captures_len {
            return Err(Error::InvalidInput(format!(
                "regexp replacement group {group} is out of range"
            )));
        }
        // Braces keep a following alphanumeric literal from becoming part of
        // the capture name in the Rust regex replacement grammar.
        out.push_str("${");
        out.push(next);
        out.push('}');
    }
    Ok(out)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn re2_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_ascii() && !ch.is_ascii_alphanumeric() && ch != '_' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
    parse_options_for(value, false, false)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_options_for(value: Value, allow_global: bool, allow_keep: bool) -> Result<RegexOptions> {
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
            'g' if allow_global => parsed.global = true,
            'k' if allow_keep => parsed.keep = true,
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
        .map_err(|error| regex_compile_error(pattern, error))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn regex_compile_error(pattern: String, error: regex::Error) -> Error {
    let message = error.to_string();
    let message = if message.contains("unrecognized escape sequence")
        || message.contains("not a Unicode scalar value")
    {
        "invalid escape sequence".to_owned()
    } else if message.contains("repetition operator missing expression") {
        format!(
            "no argument for repetition operator: {}",
            pattern.chars().next().unwrap_or('*')
        )
    } else if message.contains("unclosed group") {
        "missing closing parenthesis".to_owned()
    } else {
        message
    };
    Error::InvalidInput(message)
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
