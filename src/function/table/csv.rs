//! Bounded, single-file CSV lexical reader used by the `read_csv` adapter.
//!
//! Dialect/type detection, file expansion, compression, and rejected-row modes
//! deliberately do not belong here.  The scanner preserves enough field state
//! across reads to make the fixed input buffer unobservable to callers.
use std::{fs::File, io::Read, path::Path};

use crate::{
    common::{Error, Result},
    parallel::QueryContext,
};

const BUFFER_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CsvOptions {
    pub delimiter: u8,
    pub quote: u8,
    pub escape: u8,
    pub null: Vec<u8>,
    pub header: bool,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            quote: b'"',
            escape: b'"',
            null: Vec::new(),
            header: false,
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
        let value = if !self.field_quoted && value.as_bytes() == self.options.null.as_slice() {
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
        assert_eq!(parsed[1][2].value.as_deref(), Some("\\N"));
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
