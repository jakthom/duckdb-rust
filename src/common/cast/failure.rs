//! Cast failure provenance is independent of the public SQL error category.
//! Composite adapters propagate this result without erasing child failures.
use crate::common::Error;

pub type CastResult<T> = std::result::Result<T, CastFailure>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastBehavior {
    Strict,
    Try,
}

#[derive(Debug)]
pub struct CastFailure {
    error: Error,
    invalid_input: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFailure {
    /// A conversion rejected valid source input. Only data-error categories
    /// can be recovered; cancellation, infrastructure and internal errors
    /// remain fatal even if an adapter incorrectly passes one here.
    pub fn invalid_input(error: Error) -> Self {
        let invalid_input = matches!(
            error,
            Error::Conversion(_) | Error::InvalidInput(_) | Error::OutOfRange(_)
        );
        Self {
            error,
            invalid_input,
        }
    }
    pub fn fatal(error: Error) -> Self {
        Self {
            error,
            invalid_input: false,
        }
    }
    pub fn is_invalid_input(&self) -> bool {
        self.invalid_input
    }
    pub fn into_error(self) -> Error {
        self.error
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl From<Error> for CastFailure {
    /// Compatibility for leaf casts: ordinary Conversion means rejected input.
    /// Other public categories require an explicit local classification.
    fn from(error: Error) -> Self {
        if matches!(error, Error::Conversion(_)) {
            Self::invalid_input(error)
        } else {
            Self::fatal(error)
        }
    }
}
