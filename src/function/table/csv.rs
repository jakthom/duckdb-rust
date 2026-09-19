//! Bounded, single-file CSV lexical reader used by the `read_csv` adapter.
//!
//! Dialect/type detection, file expansion, compression, and rejected-row modes
//! deliberately do not belong here.  The scanner preserves enough field state
//! across reads to make the fixed input buffer unobservable to callers.
use std::{
    collections::BTreeSet,
    fs::File,
    io::Read,
    ops::Range,
    path::{Path, PathBuf},
};

use crate::{
    common::{
        DataType, Error, NestedPayload, NestedType, Result, Value,
        cast::{BoundCast, CastMode},
        vector::{DataChunk, Vector},
    },
    function::table::{
        TableFunction, TableFunctionArgument, TableFunctionBind, TableFunctionBindContext,
        TableFunctionState,
    },
    parallel::QueryContext,
    planner::Field,
};

const BUFFER_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_LINE_BYTES: usize = 2_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CsvOptions {
    pub delimiter: u8,
    pub quote: u8,
    pub escape: u8,
    pub null: Vec<u8>,
    pub header: bool,
    pub allow_quoted_nulls: bool,
    pub max_line_bytes: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            quote: b'"',
            escape: b'"',
            null: Vec::new(),
            header: false,
            allow_quoted_nulls: true,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct CsvField {
    pub value: Option<Range<usize>>,
}

#[derive(Debug)]
pub(super) struct CsvBatch {
    pub arena: String,
    pub rows: Vec<Vec<CsvField>>,
}

#[derive(Debug)]
pub(super) struct CsvReader {
    file: File,
    options: CsvOptions,
    buffer: [u8; BUFFER_BYTES],
    start: usize,
    end: usize,
    row: Vec<CsvField>,
    arena: Vec<u8>,
    field_offset: usize,
    field_quoted: bool,
    field_start: bool,
    in_quotes: bool,
    quote_pending: bool,
    escape_pending: bool,
    carriage_return: bool,
    skipped_header: bool,
    record_started: bool,
    record_bytes: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
            arena: Vec::new(),
            field_offset: 0,
            field_quoted: false,
            field_start: true,
            in_quotes: false,
            quote_pending: false,
            escape_pending: false,
            carriage_return: false,
            skipped_header: false,
            record_started: false,
            record_bytes: 0,
        })
    }

    /// Return up to `max_rows` complete records. A file read is bounded to the
    /// fixed buffer; field/quote state is retained until a record completes.
    pub(super) fn next_rows(
        &mut self,
        max_rows: usize,
        context: &QueryContext,
    ) -> Result<Option<CsvBatch>> {
        if max_rows == 0 {
            return Ok(Some(CsvBatch {
                arena: String::new(),
                rows: Vec::new(),
            }));
        }
        let mut rows = Vec::with_capacity(max_rows);
        loop {
            if self.start == self.end {
                context.check()?;
                let read = self.file.read(&mut self.buffer).map_err(|error| {
                    Error::Io(std::io::Error::other(format!(
                        "could not read CSV file: {error}"
                    )))
                })?;
                self.start = 0;
                self.end = read;
                if read == 0 {
                    self.finish_eof(&mut rows)?;
                    return Ok((!rows.is_empty()).then(|| self.take_batch(rows)));
                }
            }
            if !(self.in_quotes
                || self.quote_pending
                || self.escape_pending
                || self.carriage_return
                || self.field_start && self.buffer[self.start] == self.options.quote)
            {
                let ordinary = {
                    let input = &self.buffer[self.start..self.end];
                    input
                        .iter()
                        .position(|byte| {
                            matches!(*byte, b'\n' | b'\r') || *byte == self.options.delimiter
                        })
                        .unwrap_or(input.len())
                };
                if ordinary != 0 {
                    let start = self.start;
                    let end = start + ordinary;
                    self.count_record_bytes(ordinary)?;
                    self.arena
                        .try_reserve(ordinary)
                        .map_err(|_| Error::Resource("CSV field allocation failed".into()))?;
                    self.arena.extend_from_slice(&self.buffer[start..end]);
                    self.field_start = false;
                    self.record_started = true;
                    self.start += ordinary;
                    continue;
                }
            }
            let byte = self.buffer[self.start];
            self.start += 1;
            self.consume(byte, &mut rows)?;
            if rows.len() == max_rows {
                return Ok(Some(self.take_batch(rows)));
            }
        }
    }

    fn consume(&mut self, byte: u8, rows: &mut Vec<Vec<CsvField>>) -> Result<()> {
        // The record limit includes quoted embedded newlines and quote bytes,
        // but excludes the terminal LF/CRLF delimiter.
        // A pending quote is a closing quote unless the next byte is another
        // quote (the quote-as-escape form). A newline or CR at that point is
        // therefore a record delimiter too.
        let terminal_delimiter =
            matches!(byte, b'\n' | b'\r') && (!self.in_quotes || self.quote_pending);
        if !terminal_delimiter {
            self.count_record_bytes(1)?;
        }
        if self.carriage_return {
            self.carriage_return = false;
            if byte == b'\n' {
                self.finish_record(rows)?;
                return Ok(());
            }
            return Err(Error::Conversion(
                "CSV carriage return must be followed by newline".into(),
            ));
        }
        if self.escape_pending {
            self.push_field_byte(byte)?;
            self.escape_pending = false;
            self.record_started = true;
            return Ok(());
        }
        if self.quote_pending {
            self.quote_pending = false;
            if self.options.escape == self.options.quote && byte == self.options.quote {
                self.push_field_byte(byte)?;
                self.record_started = true;
                return Ok(());
            }
            self.in_quotes = false;
            return match byte {
                b if b == self.options.delimiter => self.finish_field(),
                b'\n' => self.finish_record(rows),
                b'\r' => {
                    self.carriage_return = true;
                    Ok(())
                }
                _ => Err(Error::Conversion(
                    "CSV character after closing quote is not a delimiter or newline".into(),
                )),
            };
        }
        if self.in_quotes {
            if self.options.escape != self.options.quote && byte == self.options.escape {
                self.escape_pending = true;
            } else if byte == self.options.quote {
                self.quote_pending = true;
            } else {
                self.push_field_byte(byte)?;
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
                self.push_field_byte(byte)?;
                self.field_start = false;
                self.record_started = true;
            }
        }
        Ok(())
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
            || self.arena.len() != self.field_offset
        {
            self.carriage_return = false;
            self.finish_record(rows)?;
        }
        Ok(())
    }

    fn push_field_byte(&mut self, byte: u8) -> Result<()> {
        if self.arena.len() == self.arena.capacity() {
            self.arena
                .try_reserve(1)
                .map_err(|_| Error::Resource("CSV field allocation failed".into()))?;
        }
        self.arena.push(byte);
        Ok(())
    }

    fn count_record_bytes(&mut self, bytes: usize) -> Result<()> {
        self.record_bytes = self
            .record_bytes
            .checked_add(bytes)
            .ok_or_else(|| Error::Resource("CSV record length overflow".into()))?;
        if self.record_bytes > self.options.max_line_bytes {
            return Err(Error::Resource(format!(
                "CSV record exceeds maximum line size of {} bytes",
                self.options.max_line_bytes
            )));
        }
        Ok(())
    }

    fn finish_field(&mut self) -> Result<()> {
        let range = self.field_offset..self.arena.len();
        let bytes = &self.arena[range.clone()];
        std::str::from_utf8(bytes)
            .map_err(|_| Error::Conversion("CSV field is not valid UTF-8".into()))?;
        let value = if (!self.field_quoted || self.options.allow_quoted_nulls)
            && bytes == self.options.null.as_slice()
        {
            None
        } else {
            Some(range)
        };
        self.row.push(CsvField { value });
        self.field_offset = self.arena.len();
        self.field_quoted = false;
        self.field_start = true;
        Ok(())
    }

    fn finish_record(&mut self, rows: &mut Vec<Vec<CsvField>>) -> Result<()> {
        self.finish_field()?;
        self.record_started = false;
        self.record_bytes = 0;
        if self.options.header && !self.skipped_header {
            self.skipped_header = true;
            self.row.clear();
            self.arena.clear();
            self.field_offset = 0;
        } else {
            let mut next = Vec::new();
            next.try_reserve(self.row.len())
                .map_err(|_| Error::Resource("CSV row allocation failed".into()))?;
            rows.push(std::mem::replace(&mut self.row, next));
        }
        Ok(())
    }

    fn take_batch(&mut self, rows: Vec<Vec<CsvField>>) -> CsvBatch {
        let arena = String::from_utf8(std::mem::take(&mut self.arena))
            .expect("CSV fields are validated before they enter a completed batch");
        self.field_offset = 0;
        CsvBatch { arena, rows }
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
        let mut seen = BTreeSet::new();
        let mut explicit_no_sniffing = false;
        for argument in arguments {
            let name = argument.name.as_deref().map(str::to_ascii_lowercase);
            if let Some(name) = &name
                && !seen.insert(name.clone())
            {
                return Err(Error::Bind(format!(
                    "read_csv option {name} specified more than once"
                )));
            }
            match name.as_deref() {
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
                Some("max_line_size") | Some("maximum_line_size") => {
                    options.max_line_bytes = usize_argument(argument, "max_line_size")?
                }
                Some("auto_detect") if !boolean_argument(argument, "auto_detect")? => {
                    explicit_no_sniffing = true
                }
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
        if !explicit_no_sniffing {
            return Err(Error::NotImplemented(
                "read_csv explicit-schema mode requires auto_detect=false".into(),
            ));
        }
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
        let Some(CsvBatch { arena, rows }) = state.reader.next_rows(max_rows, context)? else {
            return Ok(None);
        };
        let count = rows.len();
        let preserve = data
            .casts
            .iter()
            .map(BoundCast::can_preserve_plain_varchar_storage)
            .collect::<Vec<_>>();
        let mut values = data
            .casts
            .iter()
            .zip(&preserve)
            .map(|(_, preserve)| {
                (!*preserve)
                    .then(|| Vec::with_capacity(count))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        let mut ranges = data
            .casts
            .iter()
            .zip(&preserve)
            .map(|(_, preserve)| {
                (*preserve)
                    .then(|| Vec::with_capacity(count))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        let mut arenas = data.types.iter().map(|_| String::new()).collect::<Vec<_>>();
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
            for (column, (field, cast)) in row.into_iter().zip(&data.casts).enumerate() {
                if preserve[column] {
                    let range = field
                        .value
                        .map(|source| {
                            let start = arenas[column].len();
                            let bytes = &arena[source];
                            arenas[column].try_reserve(bytes.len()).map_err(|_| {
                                Error::Resource("CSV column allocation failed".into())
                            })?;
                            arenas[column].push_str(bytes);
                            Ok(start..arenas[column].len())
                        })
                        .transpose()?;
                    ranges[column].push(range);
                } else {
                    let input = field
                        .value
                        .map_or(Value::Null, |range| Value::Varchar(arena[range].to_owned()));
                    values[column].push(cast.apply_owned(input, context)?);
                }
            }
        }
        let columns = data
            .types
            .iter()
            .zip(values.into_iter().zip(ranges).zip(arenas))
            .zip(preserve)
            .map(|((data_type, ((values, ranges), arena)), preserve)| {
                if preserve {
                    Vector::packed_utf8(std::sync::Arc::new(arena), ranges)
                } else {
                    Vector::flat(data_type.clone(), values)
                }
            })
            .collect::<Result<_>>()?;
        DataChunk::new(columns, count).map(Some)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn text_argument(argument: &TableFunctionArgument, name: &str) -> Result<String> {
    match &argument.value {
        Value::Varchar(value) => Ok(value.clone()),
        _ => Err(Error::Bind(format!("read_csv {name} must be VARCHAR"))),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn boolean_argument(argument: &TableFunctionArgument, name: &str) -> Result<bool> {
    match argument.value {
        Value::Boolean(value) => Ok(value),
        _ => Err(Error::Bind(format!("read_csv {name} must be BOOLEAN"))),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn usize_argument(argument: &TableFunctionArgument, name: &str) -> Result<usize> {
    let value = usize::try_from(argument.value.as_i128()?)
        .map_err(|_| Error::Bind(format!("read_csv {name} must be a nonnegative integer")))?;
    if value == 0 {
        return Err(Error::Bind(format!("read_csv {name} must be positive")));
    }
    Ok(value)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

    #[derive(Debug)]
    struct TextField {
        value: Option<String>,
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn rows(input: &[u8], options: CsvOptions) -> Result<Vec<Vec<TextField>>> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(input)?;
        let mut reader = CsvReader::open(file.path(), options)?;
        let context = QueryContext::background();
        let mut result = Vec::new();
        while let Some(next) = reader.next_rows(1, &context)? {
            let CsvBatch { arena, rows } = next;
            result.extend(rows.into_iter().map(|row| {
                row.into_iter()
                    .map(|field| TextField {
                        value: field.value.map(|range| arena[range].to_owned()),
                    })
                    .collect()
            }));
        }
        Ok(result)
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn malformed_quote_is_an_error() {
        assert!(rows(b"a,\"unterminated", CsvOptions::default()).is_err());
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn strict_quote_and_line_bounds_reject_malformed_records() -> Result<()> {
        assert!(rows(b"\"x\"y\n", CsvOptions::default()).is_err());
        assert!(
            rows(
                b"\"x\"y\n",
                CsvOptions {
                    escape: b'\\',
                    ..CsvOptions::default()
                }
            )
            .is_err()
        );
        assert!(rows(b"a\rb\n", CsvOptions::default()).is_err());
        assert_eq!(
            rows(b"a\r", CsvOptions::default())?[0][0].value.as_deref(),
            Some("a")
        );
        assert_eq!(
            rows(
                b"\"x\\y\"\n",
                CsvOptions {
                    escape: b'\\',
                    ..CsvOptions::default()
                }
            )
            .unwrap()[0][0]
                .value
                .as_deref(),
            Some("xy")
        );
        assert!(
            rows(
                b"abcdef\n",
                CsvOptions {
                    max_line_bytes: 4,
                    ..CsvOptions::default()
                }
            )
            .is_err()
        );
        assert!(
            rows(
                b"abc\n",
                CsvOptions {
                    max_line_bytes: 2,
                    ..CsvOptions::default()
                }
            )
            .is_err()
        );
        assert_eq!(
            rows(
                b"abc\r\n",
                CsvOptions {
                    max_line_bytes: 3,
                    ..CsvOptions::default()
                }
            )?[0][0]
                .value
                .as_deref(),
            Some("abc")
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn quoted_newlines_count_toward_line_bounds_and_cross_input_buffers() -> Result<()> {
        let quoted = b"\"a\nb\"\n";
        let parsed = rows(
            quoted,
            CsvOptions {
                max_line_bytes: 5,
                ..CsvOptions::default()
            },
        )?;
        assert_eq!(parsed[0][0].value.as_deref(), Some("a\nb"));
        assert!(
            rows(
                quoted,
                CsvOptions {
                    max_line_bytes: 4,
                    ..CsvOptions::default()
                }
            )
            .is_err()
        );

        let prefix = "x".repeat(BUFFER_BYTES - 2);
        let input = format!("\"{prefix}\nend\"\n");
        let parsed = rows(
            input.as_bytes(),
            CsvOptions {
                max_line_bytes: input.len() - 1,
                ..CsvOptions::default()
            },
        )?;
        assert_eq!(
            parsed[0][0].value.as_deref(),
            Some(format!("{prefix}\nend").as_str())
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn utf8_split_across_input_buffers_is_valid_but_invalid_utf8_errors() -> Result<()> {
        let prefix = "x".repeat(BUFFER_BYTES - 2);
        let valid = format!("{prefix}é\n");
        assert_eq!(
            rows(valid.as_bytes(), CsvOptions::default())?[0][0]
                .value
                .as_deref(),
            Some(format!("{prefix}é").as_str())
        );
        assert!(rows(b"\xff\n", CsvOptions::default()).is_err());
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn ordinary_unquoted_fields_cross_the_large_input_buffer() -> Result<()> {
        let prefix = "x".repeat(BUFFER_BYTES + 23);
        let parsed = rows(format!("{prefix},tail\n").as_bytes(), CsvOptions::default())?;
        assert_eq!(parsed[0][0].value.as_deref(), Some(prefix.as_str()));
        assert_eq!(parsed[0][1].value.as_deref(), Some("tail"));
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn completed_records_share_one_validated_utf8_arena() -> Result<()> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(b"a,\\N\nb,c\n")?;
        let mut reader = CsvReader::open(
            file.path(),
            CsvOptions {
                null: b"\\N".to_vec(),
                ..CsvOptions::default()
            },
        )?;
        let batch = reader
            .next_rows(2, &QueryContext::background())?
            .expect("two completed records");
        assert_eq!(batch.arena, "a\\Nbc");
        assert_eq!(batch.rows.len(), 2);
        assert_eq!(batch.rows[0][0].value, Some(0..1));
        assert_eq!(batch.rows[0][1].value, None);
        assert_eq!(batch.rows[1][0].value, Some(3..4));
        assert_eq!(batch.rows[1][1].value, Some(4..5));
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn utf8_validation_cannot_cross_csv_field_boundaries() {
        assert!(rows(b"\xc3,\xa9\n", CsvOptions::default()).is_err());
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn header_empty_fields_and_eof_reset_each_batch_arena() -> Result<()> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(b"h1,h2\n,\na,b")?;
        let mut reader = CsvReader::open(
            file.path(),
            CsvOptions {
                header: true,
                null: b"\\N".to_vec(),
                ..CsvOptions::default()
            },
        )?;
        let first = reader
            .next_rows(1, &QueryContext::background())?
            .expect("first record");
        assert_eq!(first.arena, "");
        assert_eq!(first.rows[0][0].value, Some(0..0));
        assert_eq!(first.rows[0][1].value, Some(0..0));
        let second = reader
            .next_rows(1, &QueryContext::background())?
            .expect("EOF record");
        assert_eq!(second.arena, "ab");
        assert_eq!(second.rows[0][0].value, Some(0..1));
        assert_eq!(second.rows[0][1].value, Some(1..2));
        assert!(reader.next_rows(1, &QueryContext::background())?.is_none());
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
