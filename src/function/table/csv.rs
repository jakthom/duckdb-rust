//! Bounded, single-file CSV lexical reader used by the `read_csv` adapter.
//!
//! Dialect/type detection, file expansion, compression, and rejected-row modes
//! deliberately do not belong here.  The scanner preserves enough field state
//! across reads to make the fixed input buffer unobservable to callers.
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use crate::{
    common::{
        DataType, Error, NestedPayload, NestedType, Result, Value,
        cast::{BoundCast, CastMode},
        vector::DataChunk,
    },
    function::table::{
        TableFunction, TableFunctionArgument, TableFunctionBind, TableFunctionBindContext,
        TableFunctionState,
    },
    parallel::QueryContext,
    planner::Field,
};

const BUFFER_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CsvOptions {
    pub delimiter: u8,
    pub quote: u8,
    pub escape: u8,
    pub null: Vec<u8>,
    pub header: bool,
    pub allow_quoted_nulls: bool,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            quote: b'"',
            escape: b'"',
            null: Vec::new(),
            header: false,
            allow_quoted_nulls: true,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct CsvField {
    pub value: Option<String>,
}

#[derive(Debug)]
pub(super) struct CsvReader {
    file: File,
    options: CsvOptions,
    buffer: [u8; BUFFER_BYTES],
    start: usize,
    end: usize,
    row: Vec<CsvField>,
    field: Vec<u8>,
    field_quoted: bool,
    field_start: bool,
    in_quotes: bool,
    quote_pending: bool,
    escape_pending: bool,
    carriage_return: bool,
    skipped_header: bool,
    record_started: bool,
}

impl CsvReader {
    pub(super) fn open(path: &Path, options: CsvOptions) -> Result<Self> {
        if options.delimiter == b'\n' || options.delimiter == b'\r' {
            return Err(Error::Bind("CSV delimiter cannot be a newline".into()));
        }
        Ok(Self {
            file: File::open(path).map_err(|error| {
                Error::Io(std::io::Error::other(format!(
                    "could not open CSV file {}: {error}",
                    path.display()
                )))
            })?,
            options,
            buffer: [0; BUFFER_BYTES],
            start: 0,
            end: 0,
            row: Vec::new(),
            field: Vec::new(),
            field_quoted: false,
            field_start: true,
            in_quotes: false,
            quote_pending: false,
            escape_pending: false,
            carriage_return: false,
            skipped_header: false,
            record_started: false,
        })
    }

    /// Return up to `max_rows` complete records. A file read is bounded to the
    /// fixed buffer; field/quote state is retained until a record completes.
    pub(super) fn next_rows(
        &mut self,
        max_rows: usize,
        context: &QueryContext,
    ) -> Result<Option<Vec<Vec<CsvField>>>> {
        if max_rows == 0 {
            return Ok(Some(Vec::new()));
        }
        let mut rows = Vec::with_capacity(max_rows);
        loop {
            context.check()?;
            if self.start == self.end {
                let read = self.file.read(&mut self.buffer).map_err(|error| {
                    Error::Io(std::io::Error::other(format!(
                        "could not read CSV file: {error}"
                    )))
                })?;
                self.start = 0;
                self.end = read;
                if read == 0 {
                    self.finish_eof(&mut rows)?;
                    return Ok((!rows.is_empty()).then_some(rows));
                }
            }
            let byte = self.buffer[self.start];
            self.start += 1;
            self.consume(byte, &mut rows)?;
            if rows.len() == max_rows {
                return Ok(Some(rows));
            }
        }
    }

    fn consume(&mut self, byte: u8, rows: &mut Vec<Vec<CsvField>>) -> Result<()> {
        loop {
            if self.carriage_return {
                self.carriage_return = false;
                self.finish_record(rows)?;
                if byte == b'\n' {
                    return Ok(());
                }
                continue;
            }
            if self.escape_pending {
                self.field.push(byte);
                self.escape_pending = false;
                self.record_started = true;
                return Ok(());
            }
            if self.quote_pending {
                self.quote_pending = false;
                if byte == self.options.quote {
                    self.field.push(byte);
                    self.record_started = true;
                    return Ok(());
                }
                self.in_quotes = false;
                continue;
            }
            if self.in_quotes {
                if self.options.escape != self.options.quote && byte == self.options.escape {
                    self.escape_pending = true;
                } else if byte == self.options.quote {
                    if self.options.escape == self.options.quote {
                        self.quote_pending = true;
                    } else {
                        self.in_quotes = false;
                    }
                } else {
                    self.field.push(byte);
                }
                self.record_started = true;
                return Ok(());
            }
            if self.field_start && byte == self.options.quote {
                self.in_quotes = true;
                self.field_quoted = true;
                self.field_start = false;
                self.record_started = true;
                return Ok(());
            }
            match byte {
                b if b == self.options.delimiter => self.finish_field()?,
                b'\n' => self.finish_record(rows)?,
                b'\r' => self.carriage_return = true,
                _ => {
                    self.field.push(byte);
                    self.field_start = false;
                    self.record_started = true;
                }
            }
            return Ok(());
        }
    }

    fn finish_eof(&mut self, rows: &mut Vec<Vec<CsvField>>) -> Result<()> {
        if self.quote_pending {
            // With quote-as-escape, a trailing quote can only close the field:
            // another quote would have been needed to represent an escaped one.
            self.quote_pending = false;
            self.in_quotes = false;
        }
        if self.escape_pending || self.in_quotes {
            return Err(Error::Conversion("CSV unterminated quoted field".into()));
        }
        if self.carriage_return
            || self.record_started
            || !self.row.is_empty()
            || !self.field.is_empty()
        {
            self.carriage_return = false;
            self.finish_record(rows)?;
        }
        Ok(())
    }

    fn finish_field(&mut self) -> Result<()> {
        let bytes = std::mem::take(&mut self.field);
        let value = String::from_utf8(bytes)
            .map_err(|_| Error::Conversion("CSV field is not valid UTF-8".into()))?;
        let value = if (!self.field_quoted || self.options.allow_quoted_nulls)
            && value.as_bytes() == self.options.null.as_slice()
        {
            None
        } else {
            Some(value)
        };
        self.row.push(CsvField { value });
        self.field_quoted = false;
        self.field_start = true;
        Ok(())
    }

    fn finish_record(&mut self, rows: &mut Vec<Vec<CsvField>>) -> Result<()> {
        self.finish_field()?;
        self.record_started = false;
        if self.options.header && !self.skipped_header {
            self.skipped_header = true;
            self.row.clear();
        } else {
            rows.push(std::mem::take(&mut self.row));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) struct ReadCsv;

#[derive(Debug)]
struct CsvBindData {
    path: PathBuf,
    options: CsvOptions,
    types: Vec<DataType>,
    casts: Vec<BoundCast>,
}

#[derive(Debug)]
struct CsvState {
    reader: CsvReader,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TableFunction for ReadCsv {
    fn name(&self) -> &str {
        "read_csv"
    }

    fn bind(
        &self,
        arguments: &[TableFunctionArgument],
        context: &TableFunctionBindContext<'_>,
    ) -> Result<TableFunctionBind> {
        context.query.check()?;
        let mut path = None;
        let mut columns = None;
        let mut options = CsvOptions::default();
        for argument in arguments {
            match argument
                .name
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                None => {
                    if path.is_some() {
                        return Err(Error::Bind("read_csv accepts one file path".into()));
                    }
                    path = Some(text_argument(argument, "file path")?);
                }
                Some("columns") => {
                    if columns.is_some() {
                        return Err(Error::Bind(
                            "read_csv columns specified more than once".into(),
                        ));
                    }
                    columns = Some(columns_argument(argument)?);
                }
                Some("header") => options.header = boolean_argument(argument, "header")?,
                Some("allow_quoted_nulls") => {
                    options.allow_quoted_nulls = boolean_argument(argument, "allow_quoted_nulls")?
                }
                Some("delim") | Some("sep") => {
                    options.delimiter = byte_argument(argument, "delimiter")?
                }
                Some("quote") => options.quote = byte_argument(argument, "quote")?,
                Some("escape") => options.escape = byte_argument(argument, "escape")?,
                Some("nullstr") => options.null = text_argument(argument, "nullstr")?.into_bytes(),
                Some("auto_detect") | Some("sample_size") | Some("all_varchar") => {
                    return Err(Error::NotImplemented(
                        "read_csv auto detection is outside the explicit-schema reader".into(),
                    ));
                }
                Some(option) => {
                    return Err(Error::NotImplemented(format!(
                        "read_csv option {option} is not implemented"
                    )));
                }
            }
        }
        let path = path.ok_or_else(|| Error::Bind("read_csv requires a file path".into()))?;
        let columns = columns.ok_or_else(|| {
            Error::Bind("read_csv requires explicit columns={'name':'TYPE'}".into())
        })?;
        if columns.is_empty() {
            return Err(Error::Bind("read_csv columns cannot be empty".into()));
        }
        let mut schema = Vec::with_capacity(columns.len());
        let mut types = Vec::with_capacity(columns.len());
        let mut casts = Vec::with_capacity(columns.len());
        for (name, type_name) in columns {
            let data_type = (context.resolve_type)(&type_name)?;
            let cast = context.casts.bind(
                &DataType::Varchar,
                &data_type,
                CastMode::Explicit,
                context.query.types(),
            )?;
            schema.push(Field::new(name, data_type.clone()));
            types.push(data_type);
            casts.push(cast);
        }
        Ok(TableFunctionBind::new(
            schema,
            CsvBindData {
                path: PathBuf::from(path),
                options,
                types,
                casts,
            },
        ))
    }

    fn init(
        &self,
        bind: &TableFunctionBind,
        context: &QueryContext,
    ) -> Result<Box<dyn TableFunctionState>> {
        context.check()?;
        let data = bind
            .data()
            .downcast_ref::<CsvBindData>()
            .ok_or_else(|| Error::Internal("read_csv bind data type mismatch".into()))?;
        Ok(Box::new(CsvState {
            reader: CsvReader::open(&data.path, data.options.clone())?,
        }))
    }

    fn scan(
        &self,
        bind: &TableFunctionBind,
        state: &mut dyn TableFunctionState,
        max_rows: usize,
        context: &QueryContext,
    ) -> Result<Option<DataChunk>> {
        if max_rows == 0 {
            return Ok(None);
        }
        let data = bind
            .data()
            .downcast_ref::<CsvBindData>()
            .ok_or_else(|| Error::Internal("read_csv bind data type mismatch".into()))?;
        let state = state
            .downcast_mut::<CsvState>()
            .ok_or_else(|| Error::Internal("read_csv state type mismatch".into()))?;
        let Some(rows) = state.reader.next_rows(max_rows, context)? else {
            return Ok(None);
        };
        let mut values = Vec::with_capacity(rows.len());
        for (row_number, row) in rows.into_iter().enumerate() {
            context.check()?;
            if row.len() != data.casts.len() {
                return Err(Error::Conversion(format!(
                    "CSV record {} has {} columns; expected {}",
                    row_number + 1,
                    row.len(),
                    data.casts.len()
                )));
            }
            let mut converted = Vec::with_capacity(row.len());
            for (field, cast) in row.into_iter().zip(&data.casts) {
                let input = field.value.map_or(Value::Null, Value::Varchar);
                converted.push(cast.apply(&input, context)?);
            }
            values.push(converted);
        }
        DataChunk::from_rows(&data.types, &values).map(Some)
    }
}

fn text_argument(argument: &TableFunctionArgument, name: &str) -> Result<String> {
    match &argument.value {
        Value::Varchar(value) => Ok(value.clone()),
        _ => Err(Error::Bind(format!("read_csv {name} must be VARCHAR"))),
    }
}

fn boolean_argument(argument: &TableFunctionArgument, name: &str) -> Result<bool> {
    match argument.value {
        Value::Boolean(value) => Ok(value),
        _ => Err(Error::Bind(format!("read_csv {name} must be BOOLEAN"))),
    }
}

fn byte_argument(argument: &TableFunctionArgument, name: &str) -> Result<u8> {
    let value = text_argument(argument, name)?;
    let bytes = value.as_bytes();
    if bytes.len() != 1 || !bytes[0].is_ascii() {
        return Err(Error::Bind(format!(
            "read_csv {name} must be one ASCII byte"
        )));
    }
    Ok(bytes[0])
}

fn columns_argument(argument: &TableFunctionArgument) -> Result<Vec<(String, String)>> {
    let Value::Nested(value) = &argument.value else {
        return Err(Error::Bind("read_csv columns must be a STRUCT".into()));
    };
    let DataType::Nested(metadata) = &value.data_type else {
        return Err(Error::Internal(
            "CSV STRUCT value lacks nested metadata".into(),
        ));
    };
    let NestedType::Struct(names) = metadata.as_ref() else {
        return Err(Error::Bind("read_csv columns must be a STRUCT".into()));
    };
    let NestedPayload::Struct(values) = &value.payload else {
        return Err(Error::Bind("read_csv columns must be a STRUCT".into()));
    };
    if names.len() != values.len() {
        return Err(Error::Internal(
            "CSV STRUCT value has inconsistent fields".into(),
        ));
    }
    names
        .iter()
        .zip(values)
        .map(|((name, _), value)| match value {
            Value::Varchar(type_name) => Ok((name.clone(), type_name.clone())),
            _ => Err(Error::Bind(format!(
                "read_csv type for column {name} must be VARCHAR"
            ))),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::QueryContext;
    use std::io::Write;

    fn rows(input: &[u8], options: CsvOptions) -> Result<Vec<Vec<CsvField>>> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(input)?;
        let mut reader = CsvReader::open(file.path(), options)?;
        let context = QueryContext::background();
        let mut result = Vec::new();
        while let Some(next) = reader.next_rows(1, &context)? {
            result.extend(next);
        }
        Ok(result)
    }

    #[test]
    fn chunked_reader_preserves_quotes_nulls_and_crlf() -> Result<()> {
        let long = "x".repeat(BUFFER_BYTES + 19);
        let input =
            format!("name,note,value\r\nalpha,\"has, comma\",\\N\r\nbeta,\"{long}\",\"\\N\"\r\n");
        let parsed = rows(
            input.as_bytes(),
            CsvOptions {
                null: b"\\N".to_vec(),
                header: true,
                ..CsvOptions::default()
            },
        )?;
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0][0].value.as_deref(), Some("alpha"));
        assert_eq!(parsed[0][1].value.as_deref(), Some("has, comma"));
        assert_eq!(parsed[0][2].value, None);
        assert_eq!(parsed[1][1].value.as_deref(), Some(long.as_str()));
        assert_eq!(parsed[1][2].value, None);
        Ok(())
    }

    #[test]
    fn quote_escape_and_partial_final_record_are_preserved() -> Result<()> {
        let doubled = rows(b"\"one \"\"two\"\"\",3\n", CsvOptions::default())?;
        assert_eq!(doubled[0][0].value.as_deref(), Some("one \"two\""));
        let escaped = rows(
            b"\"one \\\"two\\\"\",3",
            CsvOptions {
                escape: b'\\',
                ..CsvOptions::default()
            },
        )?;
        assert_eq!(escaped[0][0].value.as_deref(), Some("one \"two\""));
        assert_eq!(escaped[0][1].value.as_deref(), Some("3"));
        Ok(())
    }

    #[test]
    fn malformed_quote_is_an_error() {
        assert!(rows(b"a,\"unterminated", CsvOptions::default()).is_err());
    }

    #[test]
    fn quote_escape_crossing_buffer_boundary_is_preserved() -> Result<()> {
        let prefix = "x".repeat(BUFFER_BYTES - 2);
        let input = ["\"", &prefix, "\"\"", "tail", "\"\"\"", ",ok\n"].concat();
        let parsed = rows(input.as_bytes(), CsvOptions::default())?;
        assert_eq!(
            parsed[0][0].value.as_deref(),
            Some(format!("{prefix}\"tail\"").as_str())
        );
        Ok(())
    }

    #[test]
    fn cancellation_is_observed_before_reading() -> Result<()> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(b"a\n")?;
        let mut reader = CsvReader::open(file.path(), CsvOptions::default())?;
        let interrupt = crate::parallel::InterruptHandle::default();
        interrupt.interrupt();
        let context = QueryContext::new(interrupt, None, 1, 1)?;
        assert!(matches!(
            reader.next_rows(1, &context),
            Err(Error::Interrupted)
        ));
        Ok(())
    }
}
