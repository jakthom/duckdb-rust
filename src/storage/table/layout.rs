use super::*;
use crate::storage::format::SnapshotFormat;
use crate::storage::layout::CheckpointLayout;
mod selected;
mod values;

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
        self.validate_layout(target, layout, None, context)
    }
    /// Preserve exact identity and catalog metadata, while allowing only the
    /// selected format's documented value canonicalization. Child bindings come
    /// from this source snapshot, never the decoded or ambient registry.
    pub fn validate_checkpoint_layout_for(
        &self,
        target: &Snapshot,
        layout: &CheckpointLayout,
        format: &dyn SnapshotFormat,
        context: &QueryContext,
    ) -> Result<()> {
        self.validate_layout(target, layout, Some(format), context)
    }
    fn validate_layout(
        &self,
        target: &Snapshot,
        layout: &CheckpointLayout,
        format: Option<&dyn SnapshotFormat>,
        context: &QueryContext,
    ) -> Result<()> {
        context.check()?;
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
            let columns = &source.definition.columns;
            let other = &destination.definition.columns;
            if source.definition.name != destination.definition.name
                || source.definition.unique_keys != destination.definition.unique_keys
                || columns.len() != other.len()
                || !columns.iter().zip(other).all(|(a, b)| {
                    a.name == b.name && a.data_type == b.data_type && a.nullable == b.nullable
                })
                || mapping.next_row_id != destination.next_id
                || !source.rows.keys().eq(mapping.rows.keys())
                || source.rows.len() != destination.rows.len()
            {
                return Err(invalid());
            }
            let selected = format
                .map(|_| {
                    columns
                        .iter()
                        .map(|column| {
                            selected::Column::bind(&column.data_type, &self.types, context)
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?;
            if !equal(
                columns.iter().map(|column| &column.default),
                other.iter().map(|column| &column.default),
                selected.as_deref(),
                format,
                context,
            )? {
                return Err(invalid());
            }
            let mut seen = BTreeSet::new();
            for (&old, &new) in &mapping.rows {
                context.check()?;
                let row = destination.rows.get(&new).ok_or_else(invalid)?;
                let source = source.rows.get(&old).ok_or_else(invalid)?;
                if !seen.insert(new)
                    || source.len() != row.len()
                    || !equal(
                        source.iter(),
                        row.iter(),
                        selected.as_deref(),
                        format,
                        context,
                    )?
                {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn equal<'a>(
    left: impl Iterator<Item = &'a crate::Value> + Clone,
    right: impl Iterator<Item = &'a crate::Value> + Clone,
    columns: Option<&[selected::Column]>,
    format: Option<&dyn SnapshotFormat>,
    context: &QueryContext,
) -> Result<bool> {
    match (format, columns) {
        (Some(format), Some(columns)) => selected::equal(left, right, columns, format, context),
        _ => values::equal(left, right, context),
    }
}
