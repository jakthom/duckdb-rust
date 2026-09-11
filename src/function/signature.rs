//! Owned advertised overload metadata, independent of an implementation or SQL.
use crate::{
    common::{DataType, Error, Result},
    parallel::QueryContext,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScalarSignature {
    pub arguments: Vec<DataType>,
    /// Advertised argument labels, not named-call binding instructions.
    /// None retains the native unnamed-signature colN diagnostic fallback.
    pub argument_names: Option<Vec<String>>,
    pub return_type: DataType,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarSignature {
    /// Validate every advertised candidate, including those with another arity.
    /// A signature describes metadata, not implementation/cast availability.
    pub fn validate_candidates(
        name: &str,
        candidates: &[Self],
        query: &QueryContext,
    ) -> Result<()> {
        query.check()?;
        if name.is_empty() || candidates.is_empty() {
            return Err(Error::Internal(
                "empty scalar overload identity or candidates".into(),
            ));
        }
        if name.len() > 4096 || candidates.len() > 4096 {
            return Err(Error::Resource("scalar overload metadata limit".into()));
        }
        let mut count = 0_usize;
        let mut name_bytes = 0_usize;
        for candidate in candidates {
            query.check()?;
            count = count
                .checked_add(candidate.arguments.len() + 1)
                .ok_or_else(|| Error::Resource("scalar overload metadata size".into()))?;
            if count > 65_536 {
                return Err(Error::Resource("scalar overload metadata limit".into()));
            }
            if let Some(names) = &candidate.argument_names {
                if names.len() != candidate.arguments.len() {
                    return Err(Error::Internal(
                        "scalar overload argument label count".into(),
                    ));
                }
                for name in names {
                    query.check()?;
                    name_bytes = name_bytes.checked_add(name.len()).ok_or_else(|| {
                        Error::Resource("scalar overload argument label size".into())
                    })?;
                    if name.len() > 4096 || name_bytes > 65_536 {
                        return Err(Error::Resource(
                            "scalar overload argument label limit".into(),
                        ));
                    }
                    if name.is_empty() || name.contains('\0') {
                        return Err(Error::Internal(
                            "invalid scalar overload argument label".into(),
                        ));
                    }
                }
            }
            for kind in candidate
                .arguments
                .iter()
                .chain(std::iter::once(&candidate.return_type))
            {
                query.check()?;
                query.types().bind(kind)?;
            }
        }
        Ok(())
    }

    /// Frontend-selected indices are untrusted metadata at the family boundary.
    pub fn selected(candidates: &[Self], index: usize) -> Result<&Self> {
        candidates
            .get(index)
            .ok_or_else(|| Error::Internal("selected scalar overload outside candidates".into()))
    }
}
