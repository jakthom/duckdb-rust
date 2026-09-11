use super::*;
use crate::catalog::expression::{
    StoredArgument, StoredArgumentStyle, StoredExpression, StoredExpressionKind,
};
use crate::storage::format::{DUCKDB_FORMAT, SnapshotFormat};
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
            for (left, right) in columns.iter().zip(other) {
                if !equal_default(
                    left.default.as_ref(),
                    right.default.as_ref(),
                    &self.types,
                    format,
                    context,
                )? {
                    return Err(invalid());
                }
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

/// Expression syntax and provenance are exact catalog metadata. Literal
/// payloads alone may use the selected checkpoint format's documented value
/// equivalence (for example, DuckDB's NaN canonicalization).
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn equal_default(
    left: Option<&StoredExpression>,
    right: Option<&StoredExpression>,
    types: &crate::common::type_registry::TypeRegistry,
    format: Option<&dyn SnapshotFormat>,
    context: &QueryContext,
) -> Result<bool> {
    let (Some(left), Some(right)) = (left, right) else {
        return Ok(left.is_none() && right.is_none());
    };
    if left.alias != right.alias || left.source_span != right.source_span {
        return Ok(false);
    }
    match (&left.kind, &right.kind) {
        (
            StoredExpressionKind::Literal {
                data_type: left_type,
                value: left,
            },
            StoredExpressionKind::Literal {
                data_type: right_type,
                value: right,
            },
        ) if left_type == right_type => {
            let selected = format
                .map(|_| selected::Column::bind(left_type, types, context))
                .transpose()?;
            equal(
                std::iter::once(left),
                std::iter::once(right),
                selected.as_ref().map(std::slice::from_ref),
                format,
                context,
            )
        }
        (
            StoredExpressionKind::Cast {
                expression: left,
                target: left_target,
                try_cast: left_try,
            },
            StoredExpressionKind::Cast {
                expression: right,
                target: right_target,
                try_cast: right_try,
            },
        ) if left_target == right_target && left_try == right_try => {
            equal_default(Some(left), Some(right), types, format, context)
        }
        (
            StoredExpressionKind::Function {
                name: left_name,
                arguments: left_arguments,
                is_operator: left_operator,
                argument_style: left_style,
            },
            StoredExpressionKind::Function {
                name: right_name,
                arguments: right_arguments,
                is_operator: right_operator,
                argument_style: right_style,
            },
        ) if left_name == right_name
            && left_operator == right_operator
            && (left_style == right_style
                || native_legacy_argument_style_equivalent(
                    *left_style,
                    *right_style,
                    left_arguments,
                    right_arguments,
                    format,
                )) =>
        {
            equal_arguments(left_arguments, right_arguments, types, format, context)
        }
        (
            StoredExpressionKind::Operator {
                kind: left_kind,
                children: left,
            },
            StoredExpressionKind::Operator {
                kind: right_kind,
                children: right,
            },
        ) if left_kind == right_kind && left.len() == right.len() => {
            for (left, right) in left.iter().zip(right) {
                if !equal_default(Some(left), Some(right), types, format, context)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn native_legacy_argument_style_equivalent(
    left: StoredArgumentStyle,
    right: StoredArgumentStyle,
    left_arguments: &[StoredArgument],
    right_arguments: &[StoredArgument],
    format: Option<&dyn SnapshotFormat>,
) -> bool {
    format.is_some_and(|format| format.format_id() == DUCKDB_FORMAT)
        && matches!(
            (left, right),
            (
                StoredArgumentStyle::Named,
                StoredArgumentStyle::LegacyAliases
            ) | (
                StoredArgumentStyle::LegacyAliases,
                StoredArgumentStyle::Named
            )
        )
        && left_arguments
            .iter()
            .chain(right_arguments)
            .all(|argument| argument.name.is_none() && argument.expression.alias.is_none())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn equal_arguments(
    left: &[StoredArgument],
    right: &[StoredArgument],
    types: &crate::common::type_registry::TypeRegistry,
    format: Option<&dyn SnapshotFormat>,
    context: &QueryContext,
) -> Result<bool> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.iter().zip(right) {
        if left.name != right.name
            || !equal_default(
                Some(&left.expression),
                Some(&right.expression),
                types,
                format,
                context,
            )?
        {
            return Ok(false);
        }
    }
    Ok(true)
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
