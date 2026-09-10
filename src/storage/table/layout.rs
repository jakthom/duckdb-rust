use super::*;
use crate::{common::Value, storage::layout::CheckpointLayout};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Snapshot {
    /// Check a format's complete row-ID mapping against its decoded output.
    /// This includes exact floating-point bits, defaults, schemas and append
    /// high-water marks. Invalid adapter output fails before file publication.
    pub fn validate_checkpoint_layout(
        &self,
        target: &Snapshot,
        layout: &CheckpointLayout,
        context: &QueryContext,
    ) -> Result<()> {
        let invalid = || {
            Error::Internal(
                "checkpoint layout does not preserve snapshot identity and values".into(),
            )
        };
        if self.schemas != target.schemas
            || self.tables.len() != target.tables.len()
            || self.tables.len() != layout.tables.len()
        {
            return Err(invalid());
        }
        for source in self.tables.values() {
            context.check_rows(source.rows.len())?;
            let name = &source.definition.name;
            let destination = target.get(name).map_err(|_| invalid())?;
            let mapping = layout.tables.get(name).ok_or_else(invalid)?;
            if serde_json::to_vec(&source.definition).map_err(|_| invalid())?
                != serde_json::to_vec(&destination.definition).map_err(|_| invalid())?
                || mapping.next_row_id != destination.next_id
                || !source.rows.keys().eq(mapping.rows.keys())
                || source.rows.len() != destination.rows.len()
            {
                return Err(invalid());
            }
            let mut seen = BTreeSet::new();
            for (&old, &new) in &mapping.rows {
                context.check()?;
                let row = destination.rows.get(&new).ok_or_else(invalid)?;
                let source = source.rows.get(&old).ok_or_else(invalid)?;
                if !seen.insert(new)
                    || source.len() != row.len()
                    || !source.iter().zip(row.iter()).all(|(a, b)| match (a, b) {
                        (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
                        (Value::Double(a), Value::Double(b)) => a.to_bits() == b.to_bits(),
                        _ => a == b,
                    })
                {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}
