//! The local CSV `COPY ... TO` sink.  It owns only the format and temporary
//! output lifecycle; planning and query cancellation remain with the client
//! context that streams chunks into it.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    common::{DataChunk, Error, Result, Value},
    parser::ast,
    planner::Schema,
};

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Bound CSV output choices.  This is deliberately a narrow local-file CSV
/// surface; unsupported copy formats and transports fail during binding.
#[derive(Clone, Debug)]
pub(crate) struct CsvWriterOptions {
    delimiter: char,
    quote: char,
    escape: char,
    null: String,
    header: bool,
    force_quote: BTreeSet<String>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CsvWriterOptions {
    pub(crate) fn bind(options: &[ast::CopyOption], schema: &Schema) -> Result<Self> {
        let mut result = Self {
            delimiter: ',',
            quote: '"',
            escape: '"',
            null: String::new(),
            // DuckDB's CSV writer emits a header unless HEADER false is set.
            header: true,
            force_quote: BTreeSet::new(),
        };
        let mut seen = BTreeSet::new();
        for option in options {
            let key = match option {
                ast::CopyOption::Format(_) => "format",
                ast::CopyOption::Freeze(_) => "freeze",
                ast::CopyOption::Delimiter(_) => "delimiter",
                ast::CopyOption::Null(_) => "null",
                ast::CopyOption::Header(_) => "header",
                ast::CopyOption::Quote(_) => "quote",
                ast::CopyOption::Escape(_) => "escape",
                ast::CopyOption::ForceQuote(_) => "force_quote",
                ast::CopyOption::ForceNotNull(_) => "force_not_null",
                ast::CopyOption::ForceNull(_) => "force_null",
                ast::CopyOption::Encoding(_) => "encoding",
            };
            if !seen.insert(key) {
                return Err(Error::Bind(format!("duplicate COPY option {key}")));
            }
            match option {
                ast::CopyOption::Format(format) if format.value.eq_ignore_ascii_case("csv") => {}
                ast::CopyOption::Format(format) => {
                    return Err(Error::Unsupported(format!("COPY TO FORMAT {format}")));
                }
                ast::CopyOption::Delimiter(value) => result.delimiter = *value,
                ast::CopyOption::Null(value) => result.null = value.clone(),
                ast::CopyOption::Header(value) => result.header = *value,
                ast::CopyOption::Quote(value) => result.quote = *value,
                ast::CopyOption::Escape(value) => result.escape = *value,
                ast::CopyOption::ForceQuote(columns) => {
                    for column in columns {
                        let name = &column.value;
                        if !schema.iter().any(|field| field.name.eq_ignore_ascii_case(name)) {
                            return Err(Error::Bind(format!(
                                "force_quote expected to find {name} in COPY output"
                            )));
                        }
                        result.force_quote.insert(name.to_ascii_lowercase());
                    }
                }
                ast::CopyOption::Encoding(encoding)
                    if encoding.eq_ignore_ascii_case("utf8") || encoding.eq_ignore_ascii_case("utf-8") => {}
                ast::CopyOption::Encoding(encoding) => {
                    return Err(Error::Unsupported(format!("COPY TO ENCODING {encoding}")));
                }
                ast::CopyOption::Freeze(_)
                | ast::CopyOption::ForceNotNull(_)
                | ast::CopyOption::ForceNull(_) => {
                    return Err(Error::Unsupported(format!("COPY TO option {key}")));
                }
            }
        }
        if result.delimiter == result.quote || result.delimiter == result.escape {
            return Err(Error::Bind("COPY delimiter must differ from quote and escape".into()));
        }
        if result.null.contains(result.delimiter) || result.null.contains(result.quote) {
            return Err(Error::Bind("COPY NULL marker conflicts with CSV dialect".into()));
        }
        Ok(result)
    }
}

/// A temporary local file that is renamed only after every source chunk has
/// been accepted. Dropping an unfinished writer preserves an existing target.
pub(crate) struct CsvWriter {
    final_path: PathBuf,
    temporary_path: PathBuf,
    writer: BufWriter<File>,
    options: CsvWriterOptions,
    schema: Schema,
    finished: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CsvWriter {
    pub(crate) fn create(path: impl AsRef<Path>, options: CsvWriterOptions, schema: &Schema) -> Result<Self> {
        let final_path = path.as_ref().to_path_buf();
        let parent = final_path.parent().unwrap_or_else(|| Path::new("."));
        let filename = final_path
            .file_name()
            .ok_or_else(|| Error::InvalidInput("COPY TO requires a file name".into()))?;
        let mut temporary_path = None;
        let mut file = None;
        for _ in 0..128 {
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(
                ".{}.duckdb-rust-copy-{}-{sequence}.tmp",
                filename.to_string_lossy(),
                std::process::id(),
            ));
            match OpenOptions::new().write(true).create_new(true).open(&candidate) {
                Ok(created) => {
                    temporary_path = Some(candidate);
                    file = Some(created);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        let temporary_path = temporary_path.ok_or_else(|| {
            Error::Resource("unable to allocate a unique COPY temporary file".into())
        })?;
        Ok(Self {
            final_path,
            temporary_path,
            writer: BufWriter::new(file.expect("temporary path and file are installed together")),
            options,
            schema: schema.clone(),
            finished: false,
        })
    }

    pub(crate) fn write_header(&mut self) -> Result<()> {
        if self.options.header {
            let fields = self.schema.iter().map(|field| field.name.as_str());
            self.write_fields(fields, None)?;
        }
        Ok(())
    }

    pub(crate) fn write_chunk(&mut self, chunk: &DataChunk) -> Result<()> {
        if chunk.columns().len() != self.schema.len() {
            return Err(Error::Internal("COPY CSV chunk width differs from schema".into()));
        }
        for row in chunk.rows() {
            self.write_row(&row)?;
        }
        Ok(())
    }

    fn write_row(&mut self, row: &[Value]) -> Result<()> {
        if row.len() != self.schema.len() {
            return Err(Error::Internal("COPY CSV row width differs from schema".into()));
        }
        for (index, value) in row.iter().enumerate() {
            if index != 0 {
                write!(self.writer, "{}", self.options.delimiter)?;
            }
            match value {
                Value::Null => self.writer.write_all(self.options.null.as_bytes())?,
                value => self.write_field(
                    &value.to_string(),
                    self.options.force_quote.contains(&self.schema[index].name.to_ascii_lowercase()),
                )?,
            }
        }
        self.writer.write_all(b"\n")?;
        Ok(())
    }

    fn write_fields<'a>(&mut self, fields: impl Iterator<Item = &'a str>, force_quote: Option<bool>) -> Result<()> {
        for (index, field) in fields.enumerate() {
            if index != 0 {
                write!(self.writer, "{}", self.options.delimiter)?;
            }
            self.write_field(field, force_quote.unwrap_or(false))?;
        }
        self.writer.write_all(b"\n")?;
        Ok(())
    }

    fn write_field(&mut self, field: &str, force_quote: bool) -> Result<()> {
        let quote = self.options.quote;
        let needs_quote = force_quote
            // An unquoted empty non-NULL value would otherwise round-trip as
            // the default empty NULL marker. A value equal to a configured
            // marker needs the same protection.
            || (field.is_empty() && self.options.null.is_empty())
            || (!self.options.null.is_empty() && field == self.options.null)
            || field.contains(self.options.delimiter)
            || field.contains(quote)
            || field.contains('\n')
            || field.contains('\r');
        if !needs_quote {
            self.writer.write_all(field.as_bytes())?;
            return Ok(());
        }
        write!(self.writer, "{quote}")?;
        for character in field.chars() {
            if character == quote {
                write!(self.writer, "{}{}", self.options.escape, quote)?;
            } else {
                write!(self.writer, "{character}")?;
            }
        }
        write!(self.writer, "{quote}")?;
        Ok(())
    }

    /// The caller checks cancellation before this visibility boundary. Errors
    /// before rename leave any existing output untouched; after a successful
    /// rename the output is the completed statement result.
    pub(crate) fn finish(mut self) -> Result<()> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        fs::rename(&self.temporary_path, &self.final_path)?;
        self.finished = true;
        Ok(())
    }
}

impl Drop for CsvWriter {
    fn drop(&mut self) {
        if !self.finished {
            let _ = fs::remove_file(&self.temporary_path);
        }
    }
}
