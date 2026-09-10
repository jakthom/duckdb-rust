//! Bound diagnostic writes across every trace file in a process.
use std::{
    io::{self, Write},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

static WRITTEN: AtomicU64 = AtomicU64::new(0);
static LIMIT: OnceLock<u64> = OnceLock::new();

fn limit() -> u64 {
    *LIMIT.get_or_init(|| match std::env::var("DUCKDB_DEV_MAX_BYTES") {
        Ok(value) => value
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or_else(|| {
                crate::recorder::fail(io::Error::other(
                    "DUCKDB_DEV_MAX_BYTES must be a positive integer",
                ))
            }),
        Err(std::env::VarError::NotPresent) => 128 * 1024 * 1024,
        Err(error) => crate::recorder::fail(io::Error::other(error)),
    })
}

pub(crate) struct Writer<W>(pub W);
impl<W: Write> Write for Writer<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = bytes.len() as u64;
        WRITTEN
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |written| {
                written.checked_add(count).filter(|next| *next <= limit())
            })
            .map_err(|_| {
                io::Error::other(format!(
                    "dev trace write budget exceeded ({} bytes); narrow the reproduction or explicitly set DUCKDB_DEV_MAX_BYTES",
                    limit()
                ))
            })?;
        let result = self.0.write(bytes);
        let written = result.as_ref().copied().unwrap_or(0) as u64;
        WRITTEN.fetch_sub(count - written, Ordering::Relaxed);
        result
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
