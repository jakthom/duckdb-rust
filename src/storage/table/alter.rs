use super::*;
use crate::{
    catalog::{TableAlteration, expression::StoredDefaultValues},
    common::Value,
};
use std::collections::BTreeMap;

pub(crate) struct PreparedTableAlteration {
    definition: Option<TableDefinition>,
    add_values: Option<PreparedAddValues>,
}

enum PreparedAddValues {
    /// Literal defaults use DuckDB's constant-vector path. Retain the source
    /// slots so applying the same preparation to the transaction's catalog
    /// basis still verifies its corresponding physical prefix.
    Constant {
        source_slots: PreparedSlots,
        value: Value,
    },
    /// Non-literal defaults retain one result per current physical slot,
    /// including deleted slots. The expression is never evaluated again when
    /// applying this preparation to catalog-basis, current, or WAL state.
    Materialized(Vec<(PhysicalSlot, Value)>),
}

/// The append-only physical stream is just `0..next_id`.  Retaining it as a
/// vector while preparing a literal ADD COLUMN turns a metadata-only constant
/// append into two large allocations (and the transaction applies it to both
/// catalog bases).  Preserve the same identity contract without materializing
/// that stream; non-contiguous layouts still retain their explicit slots.
enum PreparedSlots {
    ImplicitAppend { next_id: RowId },
    Explicit(Arc<[PhysicalSlot]>),
}

impl PreparedSlots {
    fn from_table(table: &TableData) -> Self {
        match &table.physical_order {
            PhysicalOrder::ImplicitAppend => Self::ImplicitAppend {
                next_id: table.next_id,
            },
            PhysicalOrder::Explicit(slots) => Self::Explicit(Arc::from(slots.as_slice())),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::ImplicitAppend { next_id } => usize::try_from(*next_id).unwrap_or(usize::MAX),
            Self::Explicit(slots) => slots.len(),
        }
    }

    fn collect(&self) -> Vec<PhysicalSlot> {
        match self {
            Self::ImplicitAppend { next_id } => (0..*next_id)
                .map(|id| PhysicalSlot::present(id).expect("next ID is representable"))
                .collect(),
            Self::Explicit(slots) => slots.to_vec(),
        }
    }

    fn validate_prefix(&self, before: &TableData) -> Result<()> {
        match self {
            Self::ImplicitAppend { next_id } => {
                if matches!(before.physical_order, PhysicalOrder::ImplicitAppend)
                    && before.next_id <= *next_id
                {
                    return Ok(());
                }
                let mut expected = 0;
                for slot in before.physical_order.slots(before.next_id) {
                    if expected == *next_id || slot.row_id() != expected {
                        return Err(Error::Internal(
                            "ADD COLUMN physical slot identity changed after preparation".into(),
                        ));
                    }
                    expected += 1;
                }
                Ok(())
            }
            Self::Explicit(source_slots) => validate_slot_prefix(
                before.physical_order.slots(before.next_id),
                source_slots.iter().copied(),
            ),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PreparedTableAlteration {
    fn validate_add_slots(&self, before: &TableData) -> Result<()> {
        let Some(resolved) = &self.add_values else {
            return Ok(());
        };
        match resolved {
            PreparedAddValues::Constant { source_slots, .. } => {
                source_slots.validate_prefix(before)
            }
            PreparedAddValues::Materialized(values) => validate_slot_prefix(
                before.physical_order.slots(before.next_id),
                values.iter().map(|(slot, _)| *slot),
            ),
        }
    }
}

fn validate_slot_prefix(
    before: impl Iterator<Item = PhysicalSlot>,
    mut source: impl Iterator<Item = PhysicalSlot>,
) -> Result<()> {
    for slot in before {
        let Some(resolved_slot) = source.next() else {
            return Err(Error::Internal(
                "ADD COLUMN preparation omits physical slots".into(),
            ));
        };
        if slot.row_id() != resolved_slot.row_id() {
            return Err(Error::Internal(
                "ADD COLUMN physical slot identity changed after preparation".into(),
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PreparedTableAlteration {
    fn live_add_values(&self, before: &TableData) -> Result<Option<BTreeMap<RowId, Value>>> {
        let Some(PreparedAddValues::Materialized(resolved)) = &self.add_values else {
            return Ok(None);
        };
        self.validate_add_slots(before)?;
        let mut values = BTreeMap::new();
        for (slot, (resolved_slot, value)) in
            before.physical_order.slots(before.next_id).zip(resolved)
        {
            debug_assert_eq!(slot.row_id(), resolved_slot.row_id());
            if let Some(id) = slot.live() {
                values.insert(id, value.clone());
            }
        }
        Ok(Some(values))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Snapshot {
    pub(super) fn alter(
        &mut self,
        name: &TableName,
        alteration: &TableAlteration,
        context: &QueryContext,
    ) -> Result<bool> {
        let prepared = self.prepare_alter(name, alteration, context)?;
        self.apply_prepared_alter(name, alteration, &prepared, context)
    }

    pub(crate) fn prepare_alter(
        &self,
        name: &TableName,
        alteration: &TableAlteration,
        context: &QueryContext,
    ) -> Result<PreparedTableAlteration> {
        let context = &context.clone().with_types(self.types.clone());
        context.check()?;
        let before = self.get(name)?;
        let Some(definition) = alteration.definition(&before.definition)? else {
            return Ok(PreparedTableAlteration {
                definition: None,
                add_values: None,
            });
        };
        validate_definition(&definition, &self.types)?;
        if definition.name != *name && self.tables.contains_key(&definition.name.key()) {
            return Err(Error::Catalog(format!(
                "table {} already exists",
                definition.name
            )));
        }
        let add_values = if let TableAlteration::AddColumn { column, .. } = alteration {
            // Prepared ALTER work must retain a stable owned physical stream.
            // A later COW publication may use a distinct Arc while preserving
            // the same logical slots, so validation compares streams rather
            // than Arc pointer identity.
            let source_slots = PreparedSlots::from_table(before);
            let literal = match &column.default {
                None => Some(Value::Null),
                Some(expression) => expression
                    .as_literal()
                    .filter(|(data_type, _)| *data_type == &column.data_type)
                    .map(|(_, value)| value.clone()),
            };
            if let Some(value) = literal {
                if source_slots.len() != 0 {
                    self.types
                        .bind(&column.data_type)?
                        .validate(&value, context)?;
                    if value.is_null() && !column.nullable {
                        return Err(not_null(name, &column.name));
                    }
                }
                Some(PreparedAddValues::Constant {
                    source_slots,
                    value,
                })
            } else if column
                .default
                .as_ref()
                .is_some_and(|expression| expression.is_simple_default())
            {
                let expression = column.default.as_ref().expect("checked simple default");
                match context.stored_expressions()?.evaluate_simple_default(
                    expression,
                    &column.data_type,
                    self,
                    context,
                    source_slots.len(),
                )? {
                    StoredDefaultValues::Repeated(value) => {
                        self.types
                            .bind(&column.data_type)?
                            .validate(&value, context)?;
                        if value.is_null() && !column.nullable {
                            return Err(not_null(name, &column.name));
                        }
                        Some(PreparedAddValues::Constant {
                            source_slots,
                            value,
                        })
                    }
                    StoredDefaultValues::Materialized(values) => {
                        if values.len() != source_slots.len() {
                            return Err(Error::Internal(
                                "simple ADD default returned the wrong row count".into(),
                            ));
                        }
                        let data_type = self.types.bind(&column.data_type)?;
                        let mut resolved = Vec::with_capacity(values.len());
                        for (slot, value) in source_slots.collect().into_iter().zip(values) {
                            data_type.validate(&value, context)?;
                            if value.is_null() && !column.nullable {
                                return Err(not_null(name, &column.name));
                            }
                            resolved.push((slot, value));
                        }
                        Some(PreparedAddValues::Materialized(resolved))
                    }
                }
            } else {
                let source_slots = source_slots.collect();
                let mut values = Vec::with_capacity(source_slots.len());
                let visible_only = column
                    .default
                    .as_ref()
                    .is_some_and(|expression| !expression.is_simple_default());
                for slot in source_slots {
                    context.check()?;
                    let value = if visible_only && slot.live().is_none() {
                        // DuckDB rewrites non-simple ADD defaults to ADD NULL,
                        // UPDATE visible rows, SET DEFAULT. Deleted physical slots
                        // therefore neither observe effects nor raise failures.
                        Value::Null
                    } else {
                        match &column.default {
                            Some(expression) => match expression.as_literal() {
                                Some((data_type, value)) if data_type == &column.data_type => {
                                    value.clone()
                                }
                                _ => context.stored_expressions()?.evaluate(
                                    expression,
                                    &column.data_type,
                                    self,
                                    context,
                                )?,
                            },
                            None => Value::Null,
                        }
                    };
                    if !(visible_only && slot.live().is_none()) {
                        self.types
                            .bind(&column.data_type)?
                            .validate(&value, context)?;
                        if value.is_null() && !column.nullable {
                            return Err(not_null(name, &column.name));
                        }
                    }
                    values.push((slot, value));
                }
                Some(PreparedAddValues::Materialized(values))
            }
        } else {
            None
        };
        Ok(PreparedTableAlteration {
            definition: Some(definition),
            add_values,
        })
    }

    /// Copy the already-resolved ADD result before catalog mutation. A durability
    /// journal can then encode the exact rows without another default evaluation.
    pub(crate) fn prepared_add_rows(
        &self,
        name: &TableName,
        prepared: &PreparedTableAlteration,
        context: &QueryContext,
    ) -> Result<Option<Vec<(RowId, Row)>>> {
        context.check()?;
        let before = self.get(name)?;
        let Some(values) = &prepared.add_values else {
            return Ok(None);
        };
        prepared.validate_add_slots(before)?;
        let materialized = prepared.live_add_values(before)?;
        let mut rows = Vec::with_capacity(before.rows.len());
        for (&id, row) in before.rows.iter() {
            context.check()?;
            let mut row = row.to_owned();
            row.push(match values {
                PreparedAddValues::Constant { value, .. } => value.clone(),
                PreparedAddValues::Materialized(_) => materialized
                    .as_ref()
                    .and_then(|values| values.get(&id))
                    .cloned()
                    .ok_or_else(|| {
                        Error::Internal("ADD COLUMN preparation omits a live row".into())
                    })?,
            });
            rows.push((id, row));
        }
        Ok(Some(rows))
    }

    pub(crate) fn apply_prepared_alter(
        &mut self,
        name: &TableName,
        alteration: &TableAlteration,
        prepared: &PreparedTableAlteration,
        context: &QueryContext,
    ) -> Result<bool> {
        let context = &context.clone().with_types(self.types.clone());
        context.check()?;
        let before = self.get(name)?;
        let Some(definition) = &prepared.definition else {
            return Ok(false);
        };
        let identity = self
            .registry
            .lookup_table(name)?
            .ok_or_else(|| Error::Internal("runtime registry lost altered table".into()))?;
        let mut registry = self.registry.clone();
        if definition.name != *name {
            registry.rename(
                identity,
                crate::catalog::CatalogObjectName::table(&definition.name)?,
            )?;
        } else {
            registry.alter(identity)?;
        }
        let mut after = before.clone();
        match alteration {
            TableAlteration::AddColumn { column, .. } => {
                prepared.validate_add_slots(before)?;
                match prepared.add_values.as_ref().ok_or_else(|| {
                    Error::Internal("ADD COLUMN has no prepared default values".into())
                })? {
                    PreparedAddValues::Constant { value, .. } => {
                        after
                            .rows
                            .add_column_constant(&column.data_type, value, context)?
                    }
                    PreparedAddValues::Materialized(_) => {
                        let values = prepared.live_add_values(before)?.ok_or_else(|| {
                            Error::Internal("ADD COLUMN has no materialized default values".into())
                        })?;
                        after
                            .rows
                            .add_column_values(&column.data_type, &values, context)?;
                    }
                }
            }
            TableAlteration::DropColumn { column, .. } => {
                after
                    .rows
                    .drop_column(before.definition.column_index(column)?, context)?;
            }
            TableAlteration::SetNullability {
                column,
                nullable: false,
            } => {
                let index = before.definition.column_index(column)?;
                for row in before.rows.values() {
                    context.check()?;
                    if matches!(row.get(index), Some(Value::Null)) {
                        return Err(not_null(name, column));
                    }
                }
            }
            _ => {}
        }
        // These operations preserve every indexed column's ordinal and type.
        // Existing immutable indexes and unaffected vectors can be retained.
        after.definition = definition.clone();
        context.check()?;
        self.tables.remove(&name.key());
        self.tables
            .insert(after.definition.name.key(), Arc::new(after));
        self.registry = registry;
        Ok(true)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn not_null(table: &TableName, column: &str) -> Error {
    Error::Constraint(format!(
        "NOT NULL constraint failed: {}.{column}",
        table.name
    ))
}
