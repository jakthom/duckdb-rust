use crate::{
    common::{Error, Result},
    parallel::QueryContext,
};
use std::time::{SystemTime, UNIX_EPOCH};

/// Selected source for the microsecond UTC instant attached to a transaction.
/// The runtime samples it once when the transaction starts; scalar functions
/// only read the retained instant from their query context.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub trait TransactionClock: Send + Sync {
    fn name(&self) -> &'static str;
    fn timestamp_micros(&self, query: &QueryContext) -> Result<i64>;
}

#[derive(Debug, Default)]
pub struct SystemTransactionClock;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TransactionClock for SystemTransactionClock {
    fn name(&self) -> &'static str {
        "system-transaction-clock"
    }

    fn timestamp_micros(&self, query: &QueryContext) -> Result<i64> {
        query.check()?;
        let micros = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => i64::try_from(duration.as_micros())
                .map_err(|_| Error::OutOfRange("system transaction timestamp".into()))?,
            Err(error) => i64::try_from(error.duration().as_micros())
                .ok()
                .and_then(i64::checked_neg)
                .ok_or_else(|| Error::OutOfRange("system transaction timestamp".into()))?,
        };
        query.check()?;
        Ok(micros)
    }
}
