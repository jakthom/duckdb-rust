//! DuckDB's scalar `printf` and brace-style `format` functions.
//!
//! This deliberately formats `String`s rather than C strings: SQL VARCHARs
//! may contain NUL bytes and those bytes are copied into the result.

use std::sync::Arc;

use super::VarcharBatch;
use crate::{
    common::vector::{DataChunk, SignedI64At, Vector},
    common::{DataType, Error, Result, Value, cast::CastMode, type_registry::TypeRegistry},
    function::{FunctionRegistry, ScalarBindArguments, ScalarFunction},
    parallel::QueryContext,
};

#[derive(Clone, Debug)]
struct Formatting {
    name: &'static str,
    signature: Option<Vec<DataType>>,
    constant_plan: Option<FormatPlan>,
}

#[derive(Clone, Debug)]
enum FormatPlan {
    Printf(Vec<PrintfPart>),
    Brace(Vec<BracePart>),
}

#[derive(Clone, Debug)]
enum PrintfPart {
    Literal(String),
    Field(PrintfField),
}

#[derive(Clone, Debug)]
struct PrintfField {
    argument: usize,
    width: Option<FormatNumber>,
    precision: Option<FormatNumber>,
    left: bool,
    zero: bool,
    alternate: bool,
    plus: bool,
    space: bool,
    spec: u8,
}

#[derive(Clone, Debug)]
enum FormatNumber {
    Fixed(usize),
    Argument(usize),
}

#[derive(Clone, Debug)]
enum BracePart {
    Literal(String),
    Field { argument: usize, spec: String },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn register(registry: &mut FunctionRegistry) {
    for name in ["printf", "format"] {
        registry
            .register_scalar(Arc::new(Formatting {
                name,
                signature: None,
                constant_plan: None,
            }))
            .expect("unique formatting function");
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for Formatting {
    fn name(&self) -> &str {
        self.name
    }

    fn bind(
        &self,
        arguments: &dyn ScalarBindArguments,
        query: &QueryContext,
    ) -> Result<Option<Arc<dyn ScalarFunction>>> {
        query.check()?;
        if arguments.is_empty() {
            return Err(Error::Bind(format!(
                "{} requires a format string",
                self.name
            )));
        }
        let signature = (0..arguments.len())
            .map(|i| arguments.data_type(i))
            .collect::<Result<Vec<_>>>()?;
        if !matches!(signature[0], DataType::Varchar | DataType::Null) {
            return Err(Error::Bind(format!(
                "{} format string must be VARCHAR",
                self.name
            )));
        }
        let constant_plan = match arguments.constant_if_closed(0)? {
            Some(Value::Varchar(format)) => self.compile(&format).ok(),
            _ => None,
        };
        Ok(Some(Arc::new(Self {
            name: self.name,
            signature: Some(signature),
            constant_plan,
        })))
    }

    fn argument_types(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<Vec<DataType>> {
        let signature = self.signature.as_ref().ok_or_else(|| {
            Error::Unsupported("formatting requires selected statement-local binding".into())
        })?;
        if arguments != signature
            || !matches!(arguments.first(), Some(DataType::Varchar | DataType::Null))
        {
            return Err(Error::Bind(format!(
                "no overload for {}({arguments:?})",
                self.name
            )));
        }
        // DuckDB normalizes the variadic values to the physical families
        // understood by fmt. Other logical families use their VARCHAR cast.
        let mut result = Vec::with_capacity(arguments.len());
        result.push(DataType::Varchar);
        result.extend(arguments[1..].iter().map(|argument| match argument {
            DataType::Boolean => DataType::Boolean,
            DataType::TinyInt | DataType::SmallInt | DataType::Integer | DataType::BigInt => {
                DataType::BigInt
            }
            DataType::UTinyInt | DataType::USmallInt | DataType::UInteger | DataType::UBigInt => {
                DataType::UBigInt
            }
            DataType::HugeInt => DataType::HugeInt,
            DataType::UHugeInt => DataType::UHugeInt,
            DataType::Float | DataType::Double | DataType::Decimal { .. } => DataType::Double,
            DataType::Varchar => DataType::Varchar,
            DataType::Null => DataType::Varchar,
            _ => DataType::Varchar,
        }));
        Ok(result)
    }

    fn argument_cast_mode(&self, index: usize) -> CastMode {
        if index == 0 {
            CastMode::Implicit
        } else {
            CastMode::Explicit
        }
    }

    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        if self.signature.as_deref() == Some(arguments)
            || arguments.first() == Some(&DataType::Varchar)
        {
            Ok(DataType::Varchar)
        } else {
            Err(Error::Bind(format!(
                "no overload for {}({arguments:?})",
                self.name
            )))
        }
    }

    fn supports_batch_evaluation(&self, arguments: &[DataType]) -> bool {
        self.signature
            .as_ref()
            .is_some_and(|signature| signature.len() == arguments.len())
    }

    fn evaluate_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        if let Some(result) = self.evaluate_bigint_varchar_batch(arguments, query)? {
            return Ok(Some(result));
        }
        let mut row = Vec::with_capacity(arguments.columns().len());
        let mut cache = Vec::<(String, FormatPlan)>::new();
        let mut output = Vec::new();
        output
            .try_reserve_exact(arguments.len())
            .map_err(|_| Error::Resource("cannot allocate formatting result".into()))?;
        for index in 0..arguments.len() {
            if index % 1024 == 0 {
                query.check()?;
            }
            arguments.read_row(index, &mut row)?;
            output.push(self.apply_with_cache(&row, &mut cache)?);
        }
        query.check()?;
        Vector::flat(DataType::Varchar, output).map(Some)
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        self.apply(arguments, query)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Formatting {
    fn evaluate_bigint_varchar_batch(
        &self,
        arguments: &DataChunk,
        query: &QueryContext,
    ) -> Result<Option<Vector>> {
        let [format, integer, text] = arguments.columns() else {
            return Ok(None);
        };
        if format.data_type() != &DataType::Varchar
            || integer.data_type() != &DataType::BigInt
            || text.data_type() != &DataType::Varchar
        {
            return Ok(None);
        }
        let (Some(format), Some(text)) = (VarcharBatch::new(format), VarcharBatch::new(text))
        else {
            return Ok(None);
        };
        let mut output = Vec::new();
        output
            .try_reserve_exact(arguments.len())
            .map_err(|_| Error::Resource("cannot allocate formatting result".into()))?;
        let mut cache = Vec::<(String, FormatPlan)>::new();
        for index in 0..arguments.len() {
            if index % 1024 == 0 {
                query.check()?;
            }
            let (Value::Varchar(format), SignedI64At::Value(integer), Value::Varchar(text)) = (
                format.get(index)?,
                integer.signed_i64_at(index),
                text.get(index)?,
            ) else {
                if format.get(index)?.is_null()
                    || matches!(integer.signed_i64_at(index), SignedI64At::Null)
                    || text.get(index)?.is_null()
                {
                    output.push(Value::Null);
                    continue;
                }
                return Ok(None);
            };
            let plan = if let Some(plan) = &self.constant_plan {
                plan
            } else if let Some(position) =
                cache.iter().position(|(candidate, _)| candidate == format)
            {
                &cache[position].1
            } else {
                let plan = self.compile(format)?;
                if !plan_supports_bigint_varchar(&plan) {
                    return Ok(None);
                }
                cache.push((format.clone(), plan));
                &cache.last().expect("format plan was just appended").1
            };
            if !plan_supports_bigint_varchar(plan) {
                return Ok(None);
            }
            output.push(Value::Varchar(render_bigint_varchar_plan(
                plan, integer, text,
            )?));
        }
        query.check()?;
        Vector::flat(DataType::Varchar, output).map(Some)
    }

    fn compile(&self, format: &str) -> Result<FormatPlan> {
        if self.name == "printf" {
            compile_printf(format).map(FormatPlan::Printf)
        } else {
            compile_brace(format).map(FormatPlan::Brace)
        }
    }

    fn render(&self, plan: &FormatPlan, values: &[Value]) -> Result<String> {
        match plan {
            FormatPlan::Printf(parts) => render_printf_plan(parts, values),
            FormatPlan::Brace(parts) => render_brace_plan(parts, values),
        }
    }

    fn apply(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        let mut cache = Vec::new();
        self.apply_with_cache(arguments, &mut cache)
    }

    fn apply_with_cache(
        &self,
        arguments: &[Value],
        cache: &mut Vec<(String, FormatPlan)>,
    ) -> Result<Value> {
        let (format, values) = arguments.split_first().ok_or_else(|| {
            Error::Internal("formatting argument count changed after binding".into())
        })?;
        let Value::Varchar(format) = format else {
            if format.is_null() {
                return Ok(Value::Null);
            }
            return Err(Error::Internal(
                "format string was not VARCHAR after binding".into(),
            ));
        };
        if values.iter().any(Value::is_null) {
            return Ok(Value::Null);
        }
        let result = if let Some(plan) = &self.constant_plan {
            self.render(plan, values)?
        } else if let Some((_, plan)) = cache.iter().find(|(candidate, _)| candidate == format) {
            self.render(plan, values)?
        } else {
            let plan = self.compile(format)?;
            let result = self.render(&plan, values)?;
            if cache.len() < 32 {
                cache.push((format.clone(), plan));
            }
            result
        };
        Ok(Value::Varchar(result))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn plan_supports_bigint_varchar(plan: &FormatPlan) -> bool {
    match plan {
        FormatPlan::Printf(parts) => parts.iter().all(|part| match part {
            PrintfPart::Literal(_) => true,
            PrintfPart::Field(field) => {
                !matches!(field.width, Some(FormatNumber::Argument(_)))
                    && !matches!(field.precision, Some(FormatNumber::Argument(_)))
                    && matches!((field.argument, field.spec), (0, b'd' | b'i') | (1, b's'))
            }
        }),
        FormatPlan::Brace(parts) => parts.iter().all(|part| match part {
            BracePart::Literal(_) => true,
            BracePart::Field { argument, spec } => *argument <= 1 && spec.is_empty(),
        }),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn render_bigint_varchar_plan(plan: &FormatPlan, integer: i64, text: &str) -> Result<String> {
    let mut output = String::with_capacity(text.len().saturating_add(32));
    match plan {
        FormatPlan::Printf(parts) => {
            for part in parts {
                match part {
                    PrintfPart::Literal(literal) => output.push_str(literal),
                    PrintfPart::Field(field) if field.argument == 0 => {
                        let mut rendered = integer.to_string();
                        if !rendered.starts_with('-') {
                            if field.plus {
                                rendered.insert(0, '+');
                            } else if field.space {
                                rendered.insert(0, ' ');
                            }
                        }
                        append_padded(
                            &mut output,
                            &rendered,
                            fixed_format_number(field.width.as_ref()),
                            field.left,
                            field.zero,
                            true,
                        );
                    }
                    PrintfPart::Field(field) => {
                        let precision = fixed_format_number(field.precision.as_ref());
                        let rendered = match precision {
                            Some(limit) => {
                                let end = text
                                    .char_indices()
                                    .nth(limit)
                                    .map_or(text.len(), |(index, _)| index);
                                &text[..end]
                            }
                            None => text,
                        };
                        append_padded(
                            &mut output,
                            rendered,
                            fixed_format_number(field.width.as_ref()),
                            field.left,
                            field.zero,
                            false,
                        );
                    }
                }
            }
        }
        FormatPlan::Brace(parts) => {
            for part in parts {
                match part {
                    BracePart::Literal(literal) => output.push_str(literal),
                    BracePart::Field { argument: 0, .. } => {
                        use std::fmt::Write;
                        write!(&mut output, "{integer}").map_err(|_| {
                            Error::Resource("cannot allocate formatting result".into())
                        })?;
                    }
                    BracePart::Field { argument: 1, .. } => output.push_str(text),
                    BracePart::Field { .. } => {
                        return Err(Error::Internal(
                            "unsupported field reached formatting fast path".into(),
                        ));
                    }
                }
            }
        }
    }
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn fixed_format_number(number: Option<&FormatNumber>) -> Option<usize> {
    match number {
        Some(FormatNumber::Fixed(number)) => Some(*number),
        None | Some(FormatNumber::Argument(_)) => None,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn append_padded(
    output: &mut String,
    rendered: &str,
    width: Option<usize>,
    left: bool,
    zero: bool,
    sign_aware: bool,
) {
    let padding = width
        .unwrap_or_default()
        .saturating_sub(rendered.chars().count());
    if left {
        output.push_str(rendered);
        output.extend(std::iter::repeat_n(' ', padding));
    } else if zero
        && sign_aware
        && rendered
            .as_bytes()
            .first()
            .is_some_and(|sign| matches!(sign, b'-' | b'+' | b' '))
    {
        output.push(char::from(rendered.as_bytes()[0]));
        output.extend(std::iter::repeat_n('0', padding));
        output.push_str(&rendered[1..]);
    } else {
        output.extend(std::iter::repeat_n(if zero { '0' } else { ' ' }, padding));
        output.push_str(rendered);
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compile_printf(format: &str) -> Result<Vec<PrintfPart>> {
    let mut parts = Vec::new();
    let mut cursor = 0;
    let bytes = format.as_bytes();
    let mut value = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] != b'%' {
            // Percent is ASCII, so the next position is necessarily a UTF-8
            // boundary. Copying this slice avoids turning every non-ASCII byte
            // into a separately encoded Latin-1 character.
            let next = bytes[cursor..]
                .iter()
                .position(|byte| *byte == b'%')
                .map(|offset| cursor + offset)
                .unwrap_or(bytes.len());
            parts.push(PrintfPart::Literal(format[cursor..next].to_owned()));
            cursor = next;
            continue;
        }
        cursor += 1;
        if bytes.get(cursor) == Some(&b'%') {
            parts.push(PrintfPart::Literal("%".to_owned()));
            cursor += 1;
            continue;
        }
        let start = cursor;
        while matches!(bytes.get(cursor), Some(b'-' | b'+' | b' ' | b'0' | b'#')) {
            cursor += 1;
        }
        let left = bytes[start..cursor].contains(&b'-');
        let zero = bytes[start..cursor].contains(&b'0') && !left;
        let width = if bytes.get(cursor) == Some(&b'*') {
            cursor += 1;
            let width = FormatNumber::Argument(value);
            value += 1;
            Some(width)
        } else {
            let width_start = cursor;
            while matches!(bytes.get(cursor), Some(b'0'..=b'9')) {
                cursor += 1;
            }
            parse_usize(&bytes[width_start..cursor])?.map(FormatNumber::Fixed)
        };
        let precision = if bytes.get(cursor) == Some(&b'.') {
            cursor += 1;
            if bytes.get(cursor) == Some(&b'*') {
                cursor += 1;
                let precision = FormatNumber::Argument(value);
                value += 1;
                Some(precision)
            } else {
                let p = cursor;
                while matches!(bytes.get(cursor), Some(b'0'..=b'9')) {
                    cursor += 1;
                }
                Some(FormatNumber::Fixed(
                    parse_usize(&bytes[p..cursor])?.unwrap_or(0),
                ))
            }
        } else {
            None
        };
        // h, l, j, z and t are type annotations in C printf. DuckDB's fmt
        // accepts common length modifiers; logical Values already have width.
        while matches!(
            bytes.get(cursor),
            Some(b'h' | b'l' | b'j' | b'z' | b't' | b'L')
        ) {
            cursor += 1;
        }
        let spec = *bytes
            .get(cursor)
            .ok_or_else(|| Error::InvalidInput("unterminated printf format specifier".into()))?;
        cursor += 1;
        let argument = value;
        value += 1;
        let alternate = bytes[start..cursor].contains(&b'#');
        let plus = bytes[start..cursor].contains(&b'+');
        let space = bytes[start..cursor].contains(&b' ');
        parts.push(PrintfPart::Field(PrintfField {
            argument,
            width,
            precision,
            left,
            zero,
            alternate,
            plus,
            space,
            spec,
        }));
    }
    Ok(parts)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn render_printf_plan(parts: &[PrintfPart], values: &[Value]) -> Result<String> {
    let mut output = String::new();
    for part in parts {
        let PrintfPart::Field(field) = part else {
            let PrintfPart::Literal(literal) = part else {
                unreachable!()
            };
            output.push_str(literal);
            continue;
        };
        let width = resolve_format_number(field.width.as_ref(), values, "width")?;
        let precision = resolve_format_number(field.precision.as_ref(), values, "precision")?;
        let argument = values.get(field.argument).ok_or_else(|| {
            Error::InvalidInput("printf format has more specifiers than arguments".into())
        })?;
        let mut rendered = render_printf(field.spec, argument, precision, field.alternate)?;
        if matches!(
            field.spec,
            b'd' | b'i' | b'f' | b'F' | b'e' | b'E' | b'g' | b'G'
        ) && !rendered.starts_with('-')
        {
            if field.plus {
                rendered.insert(0, '+');
            } else if field.space {
                rendered.insert(0, ' ');
            }
        }
        if let Some(width) = width {
            let pad = width.saturating_sub(rendered.chars().count());
            if pad != 0 {
                let fill = if field.zero { '0' } else { ' ' };
                if field.left {
                    rendered.extend(std::iter::repeat_n(' ', pad));
                } else {
                    // C printf places zero padding after a sign/prefix.
                    if fill == '0'
                        && (rendered.starts_with('-')
                            || rendered.starts_with('+')
                            || rendered.starts_with(' '))
                    {
                        let sign = rendered.remove(0);
                        rendered = sign.to_string()
                            + &std::iter::repeat_n(fill, pad).collect::<String>()
                            + &rendered;
                    } else if fill == '0'
                        && (rendered.starts_with("0x")
                            || rendered.starts_with("0X")
                            || rendered.starts_with("0b"))
                    {
                        let prefix = rendered[..2].to_owned();
                        rendered = prefix
                            + &std::iter::repeat_n(fill, pad).collect::<String>()
                            + &rendered[2..];
                    } else {
                        rendered = std::iter::repeat_n(fill, pad).collect::<String>() + &rendered;
                    }
                }
            }
        }
        output.push_str(&rendered);
    }
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn resolve_format_number(
    number: Option<&FormatNumber>,
    values: &[Value],
    name: &str,
) -> Result<Option<usize>> {
    match number {
        None => Ok(None),
        Some(FormatNumber::Fixed(number)) => Ok(Some(*number)),
        Some(FormatNumber::Argument(index)) => {
            let value = values
                .get(*index)
                .ok_or_else(|| Error::InvalidInput(format!("printf {name} argument is missing")))?;
            printf_width(value).map(Some)
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn printf_width(value: &Value) -> Result<usize> {
    let value = value.as_i128()?;
    usize::try_from(value)
        .map_err(|_| Error::InvalidInput("printf width or precision must be nonnegative".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn parse_usize(bytes: &[u8]) -> Result<Option<usize>> {
    if bytes.is_empty() {
        return Ok(None);
    }
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Some)
        .ok_or_else(|| Error::InvalidInput("printf width or precision is out of range".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn render_printf(
    spec: u8,
    value: &Value,
    precision: Option<usize>,
    alternate: bool,
) -> Result<String> {
    let mut text = match spec {
        b's' => match value {
            Value::Varchar(text) => text.clone(),
            _ => return Err(Error::InvalidType("printf %s requires VARCHAR".into())),
        },
        b'd' | b'i' => match value {
            Value::Boolean(v) => i128::from(*v).to_string(),
            Value::Integer(v) => v.to_string(),
            Value::Unsigned(v) => v.to_string(),
            _ => return Err(Error::InvalidType("printf %d requires an integer".into())),
        },
        b'u' => match value {
            Value::Unsigned(v) => v.to_string(),
            Value::Integer(v) => (*v as u128).to_string(),
            _ => return Err(Error::InvalidType("printf %u requires an integer".into())),
        },
        b'x' | b'X' => {
            let n = match value {
                Value::Unsigned(v) => *v,
                Value::Integer(v) => *v as u128,
                _ => return Err(Error::InvalidType("printf %x requires an integer".into())),
            };
            let prefix = if alternate {
                if spec == b'x' { "0x" } else { "0X" }
            } else {
                ""
            };
            if spec == b'x' {
                format!("{prefix}{n:x}")
            } else {
                format!("{prefix}{n:X}")
            }
        }
        b'b' => {
            let n = match value {
                Value::Unsigned(v) => *v,
                Value::Integer(v) => *v as u128,
                _ => return Err(Error::InvalidType("printf %b requires an integer".into())),
            };
            if alternate {
                format!("0b{n:b}")
            } else {
                format!("{n:b}")
            }
        }
        b'o' => {
            let n = match value {
                Value::Unsigned(v) => *v,
                Value::Integer(v) => *v as u128,
                _ => return Err(Error::InvalidType("printf %o requires an integer".into())),
            };
            if alternate {
                format!("0{n:o}")
            } else {
                format!("{n:o}")
            }
        }
        b'f' | b'F' => {
            let n = value.as_f64()?;
            match precision {
                Some(p) => format!("{n:.p$}"),
                None => format!("{n:.6}"),
            }
        }
        b'e' | b'E' => {
            let n = value.as_f64()?;
            let out = match precision {
                Some(p) => format!("{n:.p$e}"),
                None => format!("{n:.6e}"),
            };
            let out = normalize_exponent(&out);
            if spec == b'E' {
                out.to_uppercase()
            } else {
                out
            }
        }
        b'g' | b'G' => {
            let n = value.as_f64()?;
            let out = match precision {
                Some(p) => format!("{n:.p$}"),
                None => n.to_string(),
            };
            if spec == b'G' {
                out.to_uppercase()
            } else {
                out
            }
        }
        b'c' => match value {
            Value::Varchar(s) if s.chars().count() == 1 => s.clone(),
            Value::Integer(n) => u8::try_from(*n)
                .ok()
                .filter(|n| *n < 128)
                .map(|n| (n as char).to_string())
                .ok_or_else(|| {
                    Error::InvalidInput("Invalid UTF8 produced by format string".into())
                })?,
            Value::Unsigned(n) => u8::try_from(*n)
                .ok()
                .filter(|n| *n < 128)
                .map(|n| (n as char).to_string())
                .ok_or_else(|| {
                    Error::InvalidInput("Invalid UTF8 produced by format string".into())
                })?,
            _ => {
                return Err(Error::InvalidType(
                    "printf %c requires a character or integer".into(),
                ));
            }
        },
        _ => {
            return Err(Error::InvalidInput(format!(
                "invalid printf format specifier %{spec}"
            )));
        }
    };
    if spec == b's'
        && let Some(limit) = precision
    {
        text = text.chars().take(limit).collect();
    }
    Ok(text)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn value_text(value: &Value) -> String {
    match value {
        Value::Float(v) => v.to_string(),
        Value::Double(v) => v.to_string(),
        _ => value.to_string(),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn normalize_exponent(value: &str) -> String {
    let Some((mantissa, exponent)) = value.split_once('e') else {
        return value.to_owned();
    };
    match exponent.parse::<i32>() {
        Ok(exponent) => format!("{mantissa}e{exponent:+03}"),
        Err(_) => value.to_owned(),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compile_brace(format: &str) -> Result<Vec<BracePart>> {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut chars = format.chars().peekable();
    let mut next = 0usize;
    while let Some(ch) = chars.next() {
        match ch {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                if !literal.is_empty() {
                    parts.push(BracePart::Literal(std::mem::take(&mut literal)));
                }
                let mut field = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => field.push(c),
                        None => return Err(Error::InvalidInput("unclosed format field".into())),
                    }
                }
                let (index, spec) = match field.split_once(':') {
                    Some((i, s)) => (i, s),
                    None => (field.as_str(), ""),
                };
                let index = if index.is_empty() {
                    let i = next;
                    next += 1;
                    i
                } else {
                    index.parse::<usize>().map_err(|_| {
                        Error::InvalidInput("format field index must be numeric".into())
                    })?
                };
                parts.push(BracePart::Field {
                    argument: index,
                    spec: spec.to_owned(),
                });
            }
            '}' => return Err(Error::InvalidInput("unmatched } in format string".into())),
            c => literal.push(c),
        }
    }
    if !literal.is_empty() {
        parts.push(BracePart::Literal(literal));
    }
    Ok(parts)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn render_brace_plan(parts: &[BracePart], values: &[Value]) -> Result<String> {
    let mut output = String::new();
    for part in parts {
        match part {
            BracePart::Literal(literal) => output.push_str(literal),
            BracePart::Field { argument, spec } => {
                let value = values.get(*argument).ok_or_else(|| {
                    Error::InvalidInput("format field index exceeds argument count".into())
                })?;
                output.push_str(&render_brace(value, spec)?);
            }
        }
    }
    Ok(output)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn render_brace(value: &Value, spec: &str) -> Result<String> {
    if spec.is_empty() {
        return Ok(value_text(value));
    }
    let bytes = spec.as_bytes();
    let explicit_type = bytes.last().copied().filter(|ty| ty.is_ascii_alphabetic());
    let ty = explicit_type.unwrap_or(b's');
    let body = if explicit_type.is_some() {
        &spec[..spec.len() - 1]
    } else {
        spec
    };
    let zero = body.starts_with('0');
    let body = body.trim_start_matches(['0', '+', ' ', '#']);
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    let width = if digits == 0 {
        None
    } else {
        body[..digits].parse::<usize>().ok()
    };
    let precision = body
        .strip_prefix(&body[..digits])
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.parse::<usize>().ok());
    let mut result = match ty {
        b's' => match value {
            Value::Varchar(text) => match precision {
                Some(precision) => text.chars().take(precision).collect(),
                None => text.clone(),
            },
            Value::Float(number) if explicit_type.is_none() => precision.map_or_else(
                || number.to_string(),
                |p| general_precision(f64::from(*number), p),
            ),
            Value::Double(number) if explicit_type.is_none() => {
                precision.map_or_else(|| number.to_string(), |p| general_precision(*number, p))
            }
            Value::Integer(_) | Value::Unsigned(_) | Value::Decimal { .. } | Value::Bignum(_)
                if explicit_type.is_none() && precision.is_some() =>
            {
                let Some(precision) = precision else {
                    return Err(Error::Internal(
                        "numeric format precision disappeared".into(),
                    ));
                };
                general_precision(value.as_f64()?, precision)
            }
            _ if explicit_type.is_none() => value_text(value),
            _ => return Err(Error::InvalidType("format {:s} requires VARCHAR".into())),
        },
        b'd' | b'x' | b'X' | b'o' | b'b' | b'f' | b'e' | b'E' | b'g' | b'G' => {
            render_printf(ty, value, precision, spec.contains('#'))?
        }
        _ => {
            return Err(Error::InvalidInput(format!(
                "unsupported format specifier {spec:?}"
            )));
        }
    };
    if let Some(width) = width {
        let padding = width.saturating_sub(result.chars().count());
        if padding != 0 {
            let fill = if zero { '0' } else { ' ' };
            result = std::iter::repeat_n(fill, padding).collect::<String>() + &result;
        }
    }
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// `fmt`'s bare precision is significant digits for a general numeric field;
/// retain trailing zeroes because SQL formatting is presentation, not a cast.
fn general_precision(number: f64, precision: usize) -> String {
    if !number.is_finite() {
        return number.to_string();
    }
    if number == 0.0 {
        let decimals = precision.saturating_sub(1);
        return format!("{number:.decimals$}");
    }
    let exponent = number.abs().log10().floor() as i32;
    let decimals = (precision as i32 - exponent - 1).max(0) as usize;
    format!("{number:.decimals$}")
}
