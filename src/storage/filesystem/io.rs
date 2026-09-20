//! Bounded local-file operations used by checkpoint publication.
use std::io::{self, Read, Write};

use crate::{
    common::{Error, Result},
    parallel::QueryContext,
};

pub(super) const CHUNK_BYTES: usize = 64 * 1024;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_to_end<R: Read>(
    reader: &mut R,
    limit: usize,
    context: &QueryContext,
) -> Result<Vec<u8>> {
    context.check()?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; CHUNK_BYTES];
    loop {
        context.check()?;
        let remaining = limit
            .checked_sub(bytes.len())
            .ok_or_else(|| Error::Resource("local file read exceeds its limit".into()))?;
        if remaining == 0 {
            let mut probe = [0_u8; 1];
            return match retry_read(reader, &mut probe, context)? {
                0 => Ok(bytes),
                _ => Err(Error::Resource(
                    "local file reader limits input to 512 MiB".into(),
                )),
            };
        }
        let end = remaining.min(CHUNK_BYTES);
        let count = retry_read(reader, &mut chunk[..end], context)?;
        if count == 0 {
            return Ok(bytes);
        }
        // `try_reserve` lets Vec grow geometrically; exact reservation each
        // chunk could repeatedly copy the complete prefix.
        bytes
            .try_reserve(count)
            .map_err(|_| Error::Resource("local file read allocation failed".into()))?;
        bytes.extend_from_slice(&chunk[..count]);
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write_all<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    context: &QueryContext,
) -> Result<()> {
    context.check()?;
    let mut start = 0;
    while start < bytes.len() {
        let end = start
            .checked_add(CHUNK_BYTES)
            .unwrap_or(bytes.len())
            .min(bytes.len());
        let mut written = 0;
        while written < end - start {
            context.check()?;
            let count = retry_write(writer, &bytes[start + written..end], context)?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "local file made no write progress",
                )
                .into());
            }
            written = written
                .checked_add(count)
                .ok_or_else(|| Error::Resource("local file write length overflow".into()))?;
        }
        start = end;
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn retry_read<R: Read>(reader: &mut R, bytes: &mut [u8], context: &QueryContext) -> Result<usize> {
    loop {
        context.check()?;
        match reader.read(bytes) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => return result.map_err(Into::into),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn retry_write<W: Write>(writer: &mut W, bytes: &[u8], context: &QueryContext) -> Result<usize> {
    loop {
        context.check()?;
        match writer.write(bytes) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => return result.map_err(Into::into),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn range_end(offset: u64, length: usize) -> Result<()> {
    offset
        .checked_add(
            u64::try_from(length)
                .map_err(|_| Error::OutOfRange("local positioned range length overflow".into()))?,
        )
        .ok_or_else(|| Error::OutOfRange("local positioned range offset overflow".into()))?;
    Ok(())
}

/// Borrow the already leased file; every positioned syscall is bounded and
/// cancellation-aware. Native checkpoint loading and basis verification use
/// this reader before decoding the owned bytes.
pub(super) struct LocalFileReader<'a> {
    file: &'a mut std::fs::File,
    context: &'a QueryContext,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> LocalFileReader<'a> {
    pub(super) fn new(file: &'a mut std::fs::File, context: &'a QueryContext) -> Self {
        Self { file, context }
    }

    pub(super) fn read_all(&mut self, limit: usize) -> Result<Vec<u8>> {
        self.context.check()?;
        let length = usize::try_from(self.file.metadata()?.len())
            .ok()
            .filter(|length| *length <= limit)
            .ok_or_else(|| {
                Error::Resource("local positioned file exceeds its read limit".into())
            })?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| Error::Resource("local positioned read allocation failed".into()))?;
        bytes.resize(length, 0);
        self.read_exact_at(0, &mut bytes)?;
        self.context.check()?;
        if self.file.metadata()?.len() != length as u64 {
            return Err(Error::Io(io::Error::other(
                "local file length changed during read",
            )));
        }
        Ok(bytes)
    }

    pub(super) fn read_exact_at(&mut self, offset: u64, bytes: &mut [u8]) -> Result<()> {
        read_exact_at(self.file, offset, bytes, self.context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn read_exact_positioned(
    offset: u64,
    bytes: &mut [u8],
    context: &QueryContext,
    mut read: impl FnMut(u64, &mut [u8]) -> io::Result<usize>,
) -> Result<()> {
    range_end(offset, bytes.len())?;
    context.check()?;
    let mut completed = 0;
    while completed < bytes.len() {
        context.check()?;
        let end = completed.saturating_add(CHUNK_BYTES).min(bytes.len());
        match read(offset + completed as u64, &mut bytes[completed..end]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "local positioned read reached EOF",
                )
                .into());
            }
            Ok(count) if count <= end - completed => completed += count,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "local positioned reader exceeded its buffer",
                )
                .into());
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn write_exact_positioned(
    offset: u64,
    bytes: &[u8],
    context: &QueryContext,
    mut write: impl FnMut(u64, &[u8]) -> io::Result<usize>,
) -> Result<()> {
    range_end(offset, bytes.len())?;
    context.check()?;
    let mut completed = 0;
    while completed < bytes.len() {
        context.check()?;
        let end = completed.saturating_add(CHUNK_BYTES).min(bytes.len());
        match write(offset + completed as u64, &bytes[completed..end]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "local positioned write made no progress",
                )
                .into());
            }
            Ok(count) if count <= end - completed => completed += count,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "local positioned writer exceeded its buffer",
                )
                .into());
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_exact_at(
    file: &mut std::fs::File,
    offset: u64,
    bytes: &mut [u8],
    context: &QueryContext,
) -> Result<()> {
    use std::os::unix::fs::FileExt;
    read_exact_positioned(offset, bytes, context, |position, buffer| {
        file.read_at(buffer, position)
    })
}

#[cfg(unix)]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write_exact_at(
    file: &mut std::fs::File,
    offset: u64,
    bytes: &[u8],
    context: &QueryContext,
) -> Result<()> {
    use std::os::unix::fs::FileExt;
    write_exact_positioned(offset, bytes, context, |position, buffer| {
        file.write_at(buffer, position)
    })
}

#[cfg(windows)]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read_exact_at(
    file: &mut std::fs::File,
    offset: u64,
    bytes: &mut [u8],
    context: &QueryContext,
) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    use std::os::windows::fs::FileExt;
    range_end(offset, bytes.len())?;
    context.check()?;
    // Windows seek_read changes the cursor: exclusive borrowing and restoring
    // it on every exit preserve the positioned contract. Restoration must not
    // be skipped merely because the query was cancelled.
    let cursor = file.stream_position()?;
    let result = read_exact_positioned(offset, bytes, context, |position, buffer| {
        file.seek_read(buffer, position)
    });
    file.seek(SeekFrom::Start(cursor))?;
    result
}

#[cfg(windows)]
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write_exact_at(
    file: &mut std::fs::File,
    offset: u64,
    bytes: &[u8],
    context: &QueryContext,
) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    use std::os::windows::fs::FileExt;
    range_end(offset, bytes.len())?;
    context.check()?;
    let cursor = file.stream_position()?;
    let result = write_exact_positioned(offset, bytes, context, |position, buffer| {
        file.seek_write(buffer, position)
    });
    file.seek(SeekFrom::Start(cursor))?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parallel::InterruptHandle;
    use std::io::{Cursor, Seek, SeekFrom};

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn sequential_read_stops_at_eof_and_reads_bounded_chunks() -> Result<()> {
        let mut reader = Cursor::new(vec![7; CHUNK_BYTES + 3]);
        assert_eq!(
            read_to_end(&mut reader, CHUNK_BYTES + 4, &QueryContext::background())?,
            vec![7; CHUNK_BYTES + 3]
        );
        Ok(())
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn sequential_read_rejects_bytes_past_limit() {
        assert!(matches!(
            read_to_end(&mut Cursor::new(vec![7; 2]), 1, &QueryContext::background()),
            Err(Error::Resource(_))
        ));
    }
    struct ZeroWriter;
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl Write for ZeroWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn bounded_write_rejects_zero_progress() {
        assert!(
            matches!(write_all(&mut ZeroWriter, b"x", &QueryContext::background()), Err(Error::Io(error)) if error.kind() == io::ErrorKind::WriteZero)
        );
    }
    struct InterruptingWriter(InterruptHandle);
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl Write for InterruptingWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.interrupt();
            Ok(bytes.len().min(1))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn cancellation_between_partial_write_chunks_stops_later_io() -> Result<()> {
        let interrupt = InterruptHandle::default();
        let context = QueryContext::new(interrupt.clone(), None, 1, 1)?;
        assert!(matches!(
            write_all(&mut InterruptingWriter(interrupt), b"ab", &context),
            Err(Error::Interrupted)
        ));
        Ok(())
    }
    struct InterruptingReader(InterruptHandle);
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl Read for InterruptingReader {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.0.interrupt();
            bytes[0] = 7;
            Ok(1)
        }
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn cancellation_between_partial_read_chunks_stops_later_io() -> Result<()> {
        let interrupt = InterruptHandle::default();
        let context = QueryContext::new(interrupt.clone(), None, 1, 1)?;
        assert!(matches!(
            read_to_end(&mut InterruptingReader(interrupt), 2, &context),
            Err(Error::Interrupted)
        ));
        Ok(())
    }
    #[cfg(any(unix, windows))]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn positioned_io_is_exact_and_preserves_sequential_cursor() -> Result<()> {
        use std::io::Read;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("positioned");
        std::fs::write(&path, b"abcdef")?;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        file.seek(SeekFrom::Start(2))?;
        let mut bytes = [0; 2];
        read_exact_at(&mut file, 4, &mut bytes, &QueryContext::background())?;
        assert_eq!(bytes, *b"ef");
        write_exact_at(&mut file, 0, b"XY", &QueryContext::background())?;
        let mut next = [0; 1];
        file.read_exact(&mut next)?;
        assert_eq!(next, *b"c");
        assert!(matches!(
            read_exact_at(
                &mut file,
                u64::MAX,
                &mut [0; 1],
                &QueryContext::background()
            ),
            Err(Error::OutOfRange(_))
        ));
        assert!(
            matches!(read_exact_at(&mut file, 6, &mut [0; 1], &QueryContext::background()), Err(Error::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof)
        );
        Ok(())
    }

    struct ShortIo {
        cursor: Cursor<Vec<u8>>,
        first: bool,
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl Read for ShortIo {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if std::mem::take(&mut self.first) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let count = bytes.len().min(3);
            self.cursor.read(&mut bytes[..count])
        }
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl Write for ShortIo {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if std::mem::take(&mut self.first) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            self.cursor.write(&bytes[..bytes.len().min(2)])
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn sequential_short_reads_writes_and_eintr_make_progress() -> Result<()> {
        let context = QueryContext::background();
        let mut reader = ShortIo {
            cursor: Cursor::new(b"abcdefgh".to_vec()),
            first: true,
        };
        assert_eq!(read_to_end(&mut reader, 8, &context)?, b"abcdefgh");
        let mut writer = ShortIo {
            cursor: Cursor::new(Vec::new()),
            first: true,
        };
        write_all(&mut writer, b"abcdefgh", &context)?;
        assert_eq!(writer.cursor.into_inner(), b"abcdefgh");
        Ok(())
    }

    struct CancelOnEintr {
        interrupt: InterruptHandle,
        calls: usize,
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl Read for CancelOnEintr {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            self.interrupt.interrupt();
            Err(io::ErrorKind::Interrupted.into())
        }
    }
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    impl Write for CancelOnEintr {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            self.interrupt.interrupt();
            Err(io::ErrorKind::Interrupted.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn interrupted_syscall_checks_query_before_retry() -> Result<()> {
        for write in [false, true] {
            let interrupt = InterruptHandle::default();
            let context = QueryContext::new(interrupt.clone(), None, 1, 1)?;
            let mut device = CancelOnEintr {
                interrupt,
                calls: 0,
            };
            let result = if write {
                write_all(&mut device, b"x", &context)
            } else {
                read_to_end(&mut device, 1, &context).map(|_| ())
            };
            assert!(matches!(result, Err(Error::Interrupted)));
            assert_eq!(device.calls, 1);
        }
        for write in [false, true] {
            let interrupt = InterruptHandle::default();
            let context = QueryContext::new(interrupt.clone(), None, 1, 1)?;
            let mut calls = 0;
            let result = if write {
                write_exact_positioned(0, b"x", &context, |_, _| {
                    calls += 1;
                    interrupt.interrupt();
                    Err(io::ErrorKind::Interrupted.into())
                })
            } else {
                read_exact_positioned(0, &mut [0], &context, |_, _| {
                    calls += 1;
                    interrupt.interrupt();
                    Err(io::ErrorKind::Interrupted.into())
                })
            };
            assert!(matches!(result, Err(Error::Interrupted)));
            assert_eq!(calls, 1);
        }
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn positioned_short_io_advances_offsets_and_bounds_each_request() -> Result<()> {
        let source = (0..CHUNK_BYTES + 7)
            .map(|i| (i % 251) as u8)
            .collect::<Vec<_>>();
        let context = QueryContext::background();
        let mut output = vec![0; source.len()];
        let mut completed = 0;
        read_exact_positioned(9, &mut output, &context, |offset, bytes| {
            assert_eq!(offset, 9 + completed as u64);
            assert!(bytes.len() <= CHUNK_BYTES);
            let count = bytes.len().min(31);
            bytes[..count].copy_from_slice(&source[completed..completed + count]);
            completed += count;
            Ok(count)
        })?;
        assert_eq!(output, source);
        let mut written = Vec::new();
        write_exact_positioned(13, &source, &context, |offset, bytes| {
            assert_eq!(offset, 13 + written.len() as u64);
            assert!(bytes.len() <= CHUNK_BYTES);
            let count = bytes.len().min(29);
            written.extend_from_slice(&bytes[..count]);
            Ok(count)
        })?;
        assert_eq!(written, source);
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn positioned_eintr_retries_same_range_without_losing_bytes() -> Result<()> {
        let context = QueryContext::background();
        let mut positions = Vec::new();
        let mut output = [0; 4];
        read_exact_positioned(5, &mut output, &context, |offset, bytes| {
            positions.push(offset);
            if positions.len() == 1 {
                return Err(io::ErrorKind::Interrupted.into());
            }
            bytes[..2].copy_from_slice(if offset == 5 { b"ab" } else { b"cd" });
            Ok(2)
        })?;
        assert_eq!(positions, [5, 5, 7]);
        assert_eq!(output, *b"abcd");
        positions.clear();
        let mut output = Vec::new();
        write_exact_positioned(5, b"abcd", &context, |offset, bytes| {
            positions.push(offset);
            if positions.len() == 1 {
                return Err(io::ErrorKind::Interrupted.into());
            }
            output.extend_from_slice(&bytes[..2]);
            Ok(2)
        })?;
        assert_eq!(positions, [5, 5, 7]);
        assert_eq!(output, b"abcd");
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn positioned_partial_progress_then_cancel_does_not_issue_another_syscall() -> Result<()> {
        for write in [false, true] {
            let interrupt = InterruptHandle::default();
            let context = QueryContext::new(interrupt.clone(), None, 1, 1)?;
            let mut calls = 0;
            let result = if write {
                write_exact_positioned(0, b"ab", &context, |_, _| {
                    calls += 1;
                    interrupt.interrupt();
                    Ok(1)
                })
            } else {
                read_exact_positioned(0, &mut [0; 2], &context, |_, bytes| {
                    calls += 1;
                    bytes[0] = 1;
                    interrupt.interrupt();
                    Ok(1)
                })
            };
            assert!(matches!(result, Err(Error::Interrupted)));
            assert_eq!(calls, 1);
        }
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn positioned_zero_progress_and_empty_cancelled_ranges_are_rejected() -> Result<()> {
        let context = QueryContext::background();
        assert!(
            matches!(read_exact_positioned(0, &mut [0], &context, |_, _| Ok(0)), Err(Error::Io(error)) if error.kind() == io::ErrorKind::UnexpectedEof)
        );
        assert!(
            matches!(write_exact_positioned(0, b"x", &context, |_, _| Ok(0)), Err(Error::Io(error)) if error.kind() == io::ErrorKind::WriteZero)
        );
        let interrupt = InterruptHandle::default();
        let context = QueryContext::new(interrupt.clone(), None, 1, 1)?;
        interrupt.interrupt();
        assert!(matches!(
            read_exact_positioned(0, &mut [], &context, |_, _| unreachable!()),
            Err(Error::Interrupted)
        ));
        assert!(matches!(
            write_exact_positioned(0, b"", &context, |_, _| unreachable!()),
            Err(Error::Interrupted)
        ));
        assert!(matches!(
            write_all(&mut ZeroWriter, b"", &context),
            Err(Error::Interrupted)
        ));
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn local_file_reader_preserves_cursor_and_checks_length_limit() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("native-blocks");
        let expected = vec![7; CHUNK_BYTES * 2 + 3];
        std::fs::write(&path, &expected)?;
        let mut file = std::fs::File::open(path)?;
        file.seek(SeekFrom::Start(11))?;
        let context = QueryContext::background();
        assert!(matches!(
            LocalFileReader::new(&mut file, &context).read_all(expected.len() - 1),
            Err(Error::Resource(_))
        ));
        assert_eq!(
            LocalFileReader::new(&mut file, &context).read_all(expected.len())?,
            expected
        );
        assert_eq!(file.stream_position()?, 11);
        Ok(())
    }
}
