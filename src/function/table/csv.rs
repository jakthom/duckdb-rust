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

use memchr::{memchr, memchr_iter, memchr2, memchr3};

use crate::{
    common::{
        DataType, Error, NestedPayload, NestedType, Result, Value,
        cast::{BoundCast, CastMode},
        vector::{DataChunk, Vector},
    },
    function::table::{
        TableFunction, TableFunctionArgument, TableFunctionBind, TableFunctionBindContext,
        TableFunctionScanAcceptance, TableFunctionScanRequest, TableFunctionState,
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
    pub fields: Vec<CsvField>,
    pub row_ends: Vec<usize>,
}

#[derive(Debug)]
pub(super) struct CsvReader {
    file: File,
    options: CsvOptions,
    buffer: [u8; BUFFER_BYTES],
    start: usize,
    end: usize,
    fields: Vec<CsvField>,
    completed_fields: usize,
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
            fields: Vec::new(),
            completed_fields: 0,
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
                fields: Vec::new(),
                row_ends: Vec::new(),
            }));
        }
        let mut row_ends = Vec::with_capacity(max_rows);
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
                    self.finish_eof(&mut row_ends)?;
                    return Ok((!row_ends.is_empty()).then(|| self.take_batch(row_ends)));
                }
            }
            if self.try_complete_unquoted_record(&mut row_ends)? {
                if row_ends.len() == max_rows {
                    return Ok(Some(self.take_batch(row_ends)));
                }
                continue;
            }
            if self.copy_quoted_span()? {
                continue;
            }
            if !(self.in_quotes
                || self.quote_pending
                || self.escape_pending
                || self.carriage_return
                || self.field_start && self.buffer[self.start] == self.options.quote)
            {
                let ordinary = {
                    let input = &self.buffer[self.start..self.end];
                    memchr3(b'\n', b'\r', self.options.delimiter, input).unwrap_or(input.len())
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
            self.consume(byte, &mut row_ends)?;
            if row_ends.len() == max_rows {
                return Ok(Some(self.take_batch(row_ends)));
            }
        }
    }

    /// Copy a run inside a quoted field in one operation. Quotes and a distinct
    /// escape byte remain on the state-machine path; embedded newlines and CR
    /// are ordinary field bytes here.
    fn copy_quoted_span(&mut self) -> Result<bool> {
        if !self.in_quotes || self.quote_pending || self.escape_pending {
            return Ok(false);
        }
        let input = &self.buffer[self.start..self.end];
        let ordinary = if self.options.escape == self.options.quote {
            memchr(self.options.quote, input)
        } else {
            memchr2(self.options.quote, self.options.escape, input)
        }
        .unwrap_or(input.len());
        if ordinary == 0 {
            return Ok(false);
        }
        self.count_record_bytes(ordinary)?;
        self.arena
            .try_reserve(ordinary)
            .map_err(|_| Error::Resource("CSV field allocation failed".into()))?;
        let start = self.start;
        self.arena
            .extend_from_slice(&self.buffer[start..start + ordinary]);
        self.start += ordinary;
        self.record_started = true;
        Ok(true)
    }

    fn consume(&mut self, byte: u8, row_ends: &mut Vec<usize>) -> Result<()> {
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
                self.finish_record(row_ends)?;
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
                b'\n' => self.finish_record(row_ends),
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
            b'\n' => self.finish_record(row_ends)?,
            b'\r' => self.carriage_return = true,
            _ => {
                self.push_field_byte(byte)?;
                self.field_start = false;
                self.record_started = true;
            }
        }
        Ok(())
    }

    /// Consume one LF-terminated, unquoted record already entirely available in
    /// the input buffer. Quoted records, CRLF, incomplete input, and invalid
    /// UTF-8 stay on the state-machine path so its error ordering remains the
    /// observable behavior for those cases.
    fn try_complete_unquoted_record(&mut self, row_ends: &mut Vec<usize>) -> Result<bool> {
        let clean_start = self.field_start
            && self.options.quote != b'\n'
            && !self.in_quotes
            && !self.quote_pending
            && !self.escape_pending
            && !self.carriage_return
            && !self.record_started
            && self.record_bytes == 0
            && self.fields.len() == self.completed_fields
            && self.field_offset == self.arena.len();
        if !clean_start {
            return Ok(false);
        }

        let input = &self.buffer[self.start..self.end];
        let Some(first_special) = memchr3(b'\n', self.options.quote, b'\r', input) else {
            return Ok(false);
        };
        if input[first_special] != b'\n' {
            return Ok(false);
        }
        let record = &input[..first_special];
        if record.len() > self.options.max_line_bytes || std::str::from_utf8(record).is_err() {
            return Ok(false);
        }

        self.arena
            .try_reserve(record.len())
            .map_err(|_| Error::Resource("CSV field allocation failed".into()))?;

        let arena_start = self.arena.len();
        self.arena.extend_from_slice(record);
        let mut field_start = 0;
        for field_end in
            memchr_iter(self.options.delimiter, record).chain(std::iter::once(record.len()))
        {
            let range = (arena_start + field_start)..(arena_start + field_end);
            let bytes = &record[field_start..field_end];
            let value = if bytes == self.options.null.as_slice() {
                None
            } else {
                Some(range)
            };
            if self.fields.len() == self.fields.capacity() {
                self.fields
                    .try_reserve(1)
                    .map_err(|_| Error::Resource("CSV row allocation failed".into()))?;
            }
            self.fields.push(CsvField { value });
            field_start = field_end + 1;
        }
        self.field_offset = self.arena.len();
        self.start += first_special + 1;
        self.finish_completed_record(row_ends)?;
        Ok(true)
    }

    fn finish_eof(&mut self, row_ends: &mut Vec<usize>) -> Result<()> {
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
            || self.fields.len() != self.completed_fields
            || self.arena.len() != self.field_offset
        {
            self.carriage_return = false;
            self.finish_record(row_ends)?;
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
        self.fields.push(CsvField { value });
        self.field_offset = self.arena.len();
        self.field_quoted = false;
        self.field_start = true;
        Ok(())
    }

    fn finish_record(&mut self, row_ends: &mut Vec<usize>) -> Result<()> {
        self.finish_field()?;
        self.finish_completed_record(row_ends)
    }

    fn finish_completed_record(&mut self, row_ends: &mut Vec<usize>) -> Result<()> {
        self.record_started = false;
        self.record_bytes = 0;
        if self.options.header && !self.skipped_header {
            self.skipped_header = true;
            self.fields.clear();
            self.completed_fields = 0;
            self.arena.clear();
            self.field_offset = 0;
        } else {
            row_ends
                .try_reserve(1)
                .map_err(|_| Error::Resource("CSV row allocation failed".into()))?;
            row_ends.push(self.fields.len());
            self.completed_fields = self.fields.len();
        }
        Ok(())
    }

    fn take_batch(&mut self, row_ends: Vec<usize>) -> CsvBatch {
        let arena = String::from_utf8(std::mem::take(&mut self.arena))
            .expect("CSV fields are validated before they enter a completed batch");
        self.field_offset = 0;
        self.completed_fields = 0;
        CsvBatch {
            arena,
            fields: std::mem::take(&mut self.fields),
            row_ends,
        }
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
    reusable_arena: Option<std::sync::Arc<String>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ReadCsv {
    fn scan_impl(
        &self,
        bind: &TableFunctionBind,
        state: &mut dyn TableFunctionState,
        projection: Option<&[usize]>,
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
        // A completed batch leaves the reader at a record boundary. Reclaim
        // its allocation only after every output owner has released the arena.
        if let Some(arena) = state.reusable_arena.take()
            && let Ok(arena) = std::sync::Arc::try_unwrap(arena)
        {
            debug_assert!(state.reader.arena.is_empty());
            state.reader.arena = arena.into_bytes();
            state.reader.arena.clear();
        }
        let Some(CsvBatch {
            arena,
            mut fields,
            row_ends,
        }) = state.reader.next_rows(max_rows, context)?
        else {
            return Ok(None);
        };
        let count = row_ends.len();
        let preserve = data
            .casts
            .iter()
            .map(BoundCast::can_preserve_plain_varchar_storage)
            .collect::<Vec<_>>();
        // A source column may appear more than once in the requested order.
        // Retain one vector per source column and clone it only while assembling
        // the output, while still visiting every field/cast below.
        let selected = projection.map_or_else(
            || {
                (0..data.casts.len())
                    .map(|column| vec![column])
                    .collect::<Vec<_>>()
            },
            |projection| {
                let mut selected = vec![Vec::new(); data.casts.len()];
                for (output, &source) in projection.iter().enumerate() {
                    selected[source].push(output);
                }
                selected
            },
        );
        let mut values = data
            .casts
            .iter()
            .zip(preserve.iter().zip(&selected))
            .map(|(_, (preserve, selected))| {
                if !*preserve && !selected.is_empty() {
                    Vec::with_capacity(count)
                } else {
                    Vec::new()
                }
            })
            .collect::<Vec<_>>();
        let mut ranges = data
            .casts
            .iter()
            .zip(preserve.iter().zip(&selected))
            .map(|(_, (preserve, selected))| {
                if *preserve && !selected.is_empty() {
                    Vec::with_capacity(count)
                } else {
                    Vec::new()
                }
            })
            .collect::<Vec<_>>();
        let arena = std::sync::Arc::new(arena);
        {
            let mut drained = fields.drain(..);
            let mut previous_end = 0;
            for (row_number, end) in row_ends.into_iter().enumerate() {
                context.check()?;
                let width = end - previous_end;
                previous_end = end;
                if width != data.casts.len() {
                    return Err(Error::Conversion(format!(
                        "CSV record {} has {} columns; expected {}",
                        row_number + 1,
                        width,
                        data.casts.len()
                    )));
                }
                for (column, (field, cast)) in
                    drained.by_ref().take(width).zip(&data.casts).enumerate()
                {
                    if preserve[column] {
                        if !selected[column].is_empty() {
                            ranges[column].push(field.value);
                        }
                    } else {
                        let input = field.value.map(|range| &arena[range]);
                        let value = cast.apply_borrowed_varchar(input, context)?;
                        if !selected[column].is_empty() {
                            values[column].push(value);
                        }
                    }
                }
            }
        }
        let source_columns: Vec<Vector> = data
            .types
            .iter()
            .zip(values.into_iter().zip(ranges))
            .zip(preserve)
            .map(|((data_type, (values, ranges)), preserve)| {
                if preserve {
                    Vector::packed_utf8(arena.clone(), ranges)
                } else {
                    Vector::flat(data_type.clone(), values)
                }
            })
            .collect::<Result<_>>()?;
        let columns = match projection {
            None => source_columns,
            Some(projection) => projection
                .iter()
                .map(|&column| source_columns[column].clone())
                .collect(),
        };
        let chunk = DataChunk::new(columns, count)?;
        state.reader.fields = fields;
        state.reusable_arena = Some(arena);
        Ok(Some(chunk))
    }
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
            reusable_arena: None,
        }))
    }

    fn negotiate_scan(
        &self,
        bind: &TableFunctionBind,
        request: &TableFunctionScanRequest,
    ) -> TableFunctionScanAcceptance {
        // CSV still lexes every field and applies every cast before any output
        // remap. That keeps malformed unprojected fields and lazy cast errors
        // observable exactly as in the unoptimized source.
        let projection = request.projection.as_ref().and_then(|columns| {
            columns
                .iter()
                .all(|&column| column < bind.schema().len())
                .then(|| columns.clone())
        });
        // Predicate IDs are intentionally not accepted until the physical
        // filter bridge can remove exactly those residual conjuncts.
        TableFunctionScanAcceptance {
            projection,
            limit: None,
            predicate_ids: Vec::new(),
        }
    }

    fn scan(
        &self,
        bind: &TableFunctionBind,
        state: &mut dyn TableFunctionState,
        max_rows: usize,
        context: &QueryContext,
    ) -> Result<Option<DataChunk>> {
        self.scan_impl(bind, state, None, max_rows, context)
    }

    fn scan_with_request(
        &self,
        bind: &TableFunctionBind,
        state: &mut dyn TableFunctionState,
        request: &TableFunctionScanAcceptance,
        max_rows: usize,
        context: &QueryContext,
    ) -> Result<Option<DataChunk>> {
        self.scan_impl(
            bind,
            state,
            request.projection.as_deref(),
            max_rows,
            context,
        )
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
            let CsvBatch {
                arena,
                fields,
                row_ends,
            } = next;
            let mut fields = fields.into_iter();
            let mut previous_end = 0;
            for row_end in row_ends {
                let width = row_end - previous_end;
                previous_end = row_end;
                result.push(
                    fields
                        .by_ref()
                        .take(width)
                        .map(|field| TextField {
                            value: field.value.map(|range| arena[range].to_owned()),
                        })
                        .collect(),
                );
            }
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
        assert_eq!(batch.arena, "a,\\Nb,c");
        assert_eq!(batch.row_ends, vec![2, 4]);
        assert_eq!(batch.fields[0].value, Some(0..1));
        assert_eq!(batch.fields[1].value, None);
        assert_eq!(batch.fields[2].value, Some(4..5));
        assert_eq!(batch.fields[3].value, Some(6..7));
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn complete_unquoted_records_flatten_fields_and_preserve_dialect_options() -> Result<()> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(b"name;note;tail\na;\\N;\n;b;end\n")?;
        let mut reader = CsvReader::open(
            file.path(),
            CsvOptions {
                delimiter: b';',
                header: true,
                null: b"\\N".to_vec(),
                ..CsvOptions::default()
            },
        )?;
        let batch = reader
            .next_rows(2, &QueryContext::background())?
            .expect("two complete records");
        assert_eq!(batch.arena, "a;\\N;;b;end");
        assert_eq!(batch.row_ends, vec![3, 6]);
        assert_eq!(batch.fields[0].value, Some(0..1));
        assert_eq!(batch.fields[1].value, None);
        assert_eq!(batch.fields[2].value, Some(5..5));
        assert_eq!(batch.fields[3].value, Some(5..5));
        assert_eq!(batch.fields[4].value, Some(6..7));
        assert_eq!(batch.fields[5].value, Some(8..11));
        let nul_delimited = rows(
            b"left\0right\n",
            CsvOptions {
                delimiter: b'\0',
                null: b"\\N".to_vec(),
                ..CsvOptions::default()
            },
        )?;
        assert_eq!(nul_delimited[0][0].value.as_deref(), Some("left"));
        assert_eq!(nul_delimited[0][1].value.as_deref(), Some("right"));
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn complete_unquoted_fast_path_falls_back_for_quotes_crlf_limits_and_invalid_utf8() -> Result<()>
    {
        let options = CsvOptions {
            null: b"\\N".to_vec(),
            quote: b'\'',
            escape: b'\\',
            ..CsvOptions::default()
        };
        let parsed = rows(b"clean,record\n'quoted,record',tail\r\n", options.clone())?;
        assert_eq!(parsed[0][0].value.as_deref(), Some("clean"));
        assert_eq!(parsed[0][1].value.as_deref(), Some("record"));
        assert_eq!(parsed[1][0].value.as_deref(), Some("quoted,record"));
        assert_eq!(parsed[1][1].value.as_deref(), Some("tail"));
        assert!(
            rows(
                b"abc\n",
                CsvOptions {
                    max_line_bytes: 2,
                    ..options.clone()
                }
            )
            .is_err()
        );
        assert!(rows(b"\xc3,\xa9\n", options).is_err());
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn complete_unquoted_fast_path_handles_a_record_at_the_input_boundary() -> Result<()> {
        let prefix = "x".repeat(BUFFER_BYTES - 4);
        let input = format!("{prefix},z\nnext,row\n");
        let parsed = rows(
            input.as_bytes(),
            CsvOptions {
                null: b"\\N".to_vec(),
                ..CsvOptions::default()
            },
        )?;
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0][0].value.as_deref(), Some(prefix.as_str()));
        assert_eq!(parsed[0][1].value.as_deref(), Some("z"));
        assert_eq!(parsed[1][0].value.as_deref(), Some("next"));
        assert_eq!(parsed[1][1].value.as_deref(), Some("row"));
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn flattened_batches_keep_eof_and_partial_record_boundaries_distinct() -> Result<()> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(b"a,b\nc,d\n")?;
        let mut reader = CsvReader::open(file.path(), CsvOptions::default())?;
        let batch = reader
            .next_rows(8, &QueryContext::background())?
            .expect("short final batch");
        assert_eq!(batch.row_ends, vec![2, 4]);
        assert!(reader.next_rows(8, &QueryContext::background())?.is_none());

        let split_value = "x".repeat(BUFFER_BYTES + 7);
        let parsed = rows(
            format!(",{split_value},tail\na,,b\n").as_bytes(),
            CsvOptions {
                null: b"\\N".to_vec(),
                ..CsvOptions::default()
            },
        )?;
        assert_eq!(parsed[0][0].value.as_deref(), Some(""));
        assert_eq!(parsed[0][1].value.as_deref(), Some(split_value.as_str()));
        assert_eq!(parsed[0][2].value.as_deref(), Some("tail"));
        assert_eq!(parsed[1][0].value.as_deref(), Some("a"));
        assert_eq!(parsed[1][1].value.as_deref(), Some(""));
        assert_eq!(parsed[1][2].value.as_deref(), Some("b"));

        let mut header = tempfile::NamedTempFile::new()?;
        header.write_all(b"h1,h2\n")?;
        let mut header_reader = CsvReader::open(
            header.path(),
            CsvOptions {
                header: true,
                ..CsvOptions::default()
            },
        )?;
        assert!(
            header_reader
                .next_rows(8, &QueryContext::background())?
                .is_none()
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn newline_quote_uses_the_state_machine_path() {
        assert!(
            rows(
                b"\n",
                CsvOptions {
                    quote: b'\n',
                    ..CsvOptions::default()
                },
            )
            .is_err()
        );
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
        assert_eq!(first.arena, ",");
        assert_eq!(first.row_ends, vec![2]);
        assert_eq!(first.fields[0].value, Some(0..0));
        assert_eq!(first.fields[1].value, Some(1..1));
        let second = reader
            .next_rows(1, &QueryContext::background())?
            .expect("EOF record");
        assert_eq!(second.arena, "ab");
        assert_eq!(second.row_ends, vec![2]);
        assert_eq!(second.fields[0].value, Some(0..1));
        assert_eq!(second.fields[1].value, Some(1..2));
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
    fn bulk_quoted_spans_preserve_escapes_and_line_bounds_across_buffers() -> Result<()> {
        let prefix = "x".repeat(BUFFER_BYTES - 3);
        let doubled = ["\"", &prefix, "\"\"middle\"\"\"", ",tail\n"].concat();
        let parsed = rows(doubled.as_bytes(), CsvOptions::default())?;
        assert_eq!(
            parsed[0][0].value.as_deref(),
            Some(format!("{prefix}\"middle\"").as_str())
        );
        assert_eq!(parsed[0][1].value.as_deref(), Some("tail"));

        let escaped = ["\"", &prefix, "\\\"middle\\\"\",tail\n"].concat();
        let parsed = rows(
            escaped.as_bytes(),
            CsvOptions {
                escape: b'\\',
                ..CsvOptions::default()
            },
        )?;
        assert_eq!(
            parsed[0][0].value.as_deref(),
            Some(format!("{prefix}\"middle\"").as_str())
        );
        assert_eq!(parsed[0][1].value.as_deref(), Some("tail"));

        let over_limit = format!("\"{}\"\n", "y".repeat(BUFFER_BYTES + 1));
        assert!(matches!(
            rows(
                over_limit.as_bytes(),
                CsvOptions {
                    max_line_bytes: BUFFER_BYTES,
                    ..CsvOptions::default()
                },
            ),
            Err(Error::Resource(message)) if message.contains("maximum line size")
        ));
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn wide_variable_records_grow_metadata_without_changing_retained_batches() -> Result<()> {
        let empty_fields = 4097;
        let mut first_record = String::from("α,\\N");
        for _ in 0..empty_fields {
            first_record.push(',');
        }
        first_record.push('\n');
        let input = format!("{first_record}tail,🦆,\n");
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(input.as_bytes())?;
        let mut reader = CsvReader::open(
            file.path(),
            CsvOptions {
                null: b"\\N".to_vec(),
                ..CsvOptions::default()
            },
        )?;

        let first = reader
            .next_rows(1, &QueryContext::background())?
            .expect("wide first record");
        let width = empty_fields + 2;
        assert_eq!(first.row_ends, vec![width]);
        assert_eq!(first.fields.len(), width);
        assert_eq!(first.fields[0].value, Some(0.."α".len()));
        assert_eq!(first.fields[1].value, None);
        assert_eq!(first.fields[2].value, Some("α,\\N,".len().."α,\\N,".len()));
        assert_eq!(
            first.fields.last().unwrap().value,
            Some(first.arena.len()..first.arena.len())
        );
        let retained_arena = first.arena.clone();
        let retained_fields = first
            .fields
            .iter()
            .map(|field| field.value.clone())
            .collect::<Vec<_>>();

        let second = reader
            .next_rows(1, &QueryContext::background())?
            .expect("later record");
        assert_eq!(second.row_ends, vec![3]);
        assert_eq!(second.arena, "tail,🦆,");
        assert_eq!(second.fields[0].value, Some(0..4));
        assert_eq!(second.fields[1].value, Some(5..9));
        assert_eq!(second.fields[2].value, Some(10..10));
        assert_eq!(first.arena, retained_arena);
        assert_eq!(
            first
                .fields
                .iter()
                .map(|field| field.value.clone())
                .collect::<Vec<_>>(),
            retained_fields
        );
        assert!(reader.next_rows(1, &QueryContext::background())?.is_none());
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
