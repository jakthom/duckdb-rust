//! Immutable equality indexes. The transaction owner publishes an index and
//! its rows together; an index never consults mutable catalog or table state.
mod btree;
mod hash;

pub use btree::BTreeIndexFactory;
pub use hash::HashIndexFactory;

use std::{collections::HashSet, fmt::Debug, sync::Arc};

use crate::{
    common::{DataType, Error, Result, Row, Value},
    parallel::QueryContext,
    storage::RowId,
};

#[derive(Debug, Clone)]
pub struct IndexSpec {
    pub key_types: Vec<DataType>,
    pub unique: bool,
}

/// Keys have exactly the declared physical types; callers perform SQL coercion
/// before this boundary. NULL keys do not match and do not conflict. NaNs match
/// one another, as do signed zeros. Results contain distinct ascending row IDs.
/// Adapters own all retained data and support concurrent readers. Errors expose
/// no partial index. Work cooperatively observes cancellation and row limits.
pub trait KeyIndex: Debug + Send + Sync {
    fn lookup(&self, key: &Row, context: &QueryContext) -> Result<Vec<RowId>>;
}

/// Each input contains an already projected key and its unique row identity.
/// Building is synchronous; no references into the input survive this call.
pub trait IndexFactory: Send + Sync {
    fn name(&self) -> &'static str;
    fn build(
        &self,
        spec: IndexSpec,
        entries: &mut dyn Iterator<Item = (RowId, Row)>,
        context: &QueryContext,
    ) -> Result<Arc<dyn KeyIndex>>;
}

impl IndexSpec {
    fn validate(&self) -> Result<()> {
        if self.key_types.is_empty() {
            return Err(Error::Bind("index requires at least one key column".into()));
        }
        Ok(())
    }
    fn bind(&self, context: &QueryContext) -> Result<BoundIndexSpec> {
        self.validate()?;
        Ok(BoundIndexSpec {
            key_types: self
                .key_types
                .iter()
                .map(|t| context.types().bind(t))
                .collect::<Result<_>>()?,
            unique: self.unique,
        })
    }
}

#[derive(Debug)]
struct BoundIndexSpec {
    key_types: Vec<crate::common::type_registry::BoundType>,
    unique: bool,
}
impl BoundIndexSpec {
    fn key(&self, values: &Row, context: &QueryContext) -> Result<Option<Vec<u8>>> {
        if values.len() != self.key_types.len() {
            return Err(Error::Bind(
                "index key width differs from its declaration".into(),
            ));
        }
        let mut bytes = Vec::new();
        for (value, data_type) in values.iter().zip(&self.key_types) {
            if !value.fits_type(data_type.data_type()) {
                return Err(Error::Bind("index key has the wrong physical type".into()));
            }
            data_type.append_key(value, &mut bytes, context)?;
        }
        Ok((!values.iter().any(Value::is_null)).then_some(bytes))
    }
}

fn build_entries(
    spec: &BoundIndexSpec,
    entries: &mut dyn Iterator<Item = (RowId, Row)>,
    context: &QueryContext,
    mut insert: impl FnMut(Vec<u8>, RowId) -> Result<()>,
) -> Result<()> {
    context.check()?;
    let mut seen = HashSet::new();
    for (id, key) in entries {
        context.check_rows(seen.len().saturating_add(1))?;
        if !seen.insert(id) {
            return Err(Error::Bind("duplicate row identity in index input".into()));
        }
        if let Some(key) = spec.key(&key, context)? {
            insert(key, id)?;
        }
    }
    Ok(())
}

fn append(ids: &mut Vec<RowId>, id: RowId, unique: bool) -> Result<()> {
    if unique && !ids.is_empty() {
        return Err(Error::Constraint("duplicate index key".into()));
    }
    ids.push(id);
    Ok(())
}
