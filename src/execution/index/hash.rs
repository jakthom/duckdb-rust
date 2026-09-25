use std::collections::HashMap;

use super::*;

#[derive(Default)]
pub struct HashIndexFactory;

#[derive(Debug)]
struct HashIndex {
    spec: BoundIndexSpec,
    entries: HashMap<Vec<u8>, Vec<RowId>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl IndexFactory for HashIndexFactory {
    fn name(&self) -> &'static str {
        "hash-equality"
    }
    fn build(
        &self,
        spec: IndexSpec,
        input: &mut dyn Iterator<Item = (RowId, Row)>,
        context: &QueryContext,
    ) -> Result<Arc<dyn KeyIndex>> {
        let spec = spec.bind(context)?;
        let mut entries = HashMap::<_, Vec<_>>::new();
        build_entries(&spec, input, context, |key, id| {
            append(entries.entry(key).or_default(), id, spec.unique)
        })?;
        for ids in entries.values_mut() {
            context.check()?;
            ids.sort_unstable();
        }
        Ok(Arc::new(HashIndex { spec, entries }))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl KeyIndex for HashIndex {
    fn lookup(&self, key: &Row, context: &QueryContext) -> Result<Vec<RowId>> {
        context.check()?;
        let ids = self
            .spec
            .key(key, context)?
            .and_then(|key| self.entries.get(&key))
            .map(Vec::as_slice)
            .unwrap_or_default();
        context.check_rows(ids.len())?;
        Ok(ids.to_vec())
    }
}
