//! Native v2 transaction encoding with private physical row-ID translation.
use super::super::{
    binary::Encoder,
    primitive,
    writer::{table_definition, type_definition},
};
use super::{MAX_ENTRIES, append_frame};
use crate::{
    catalog::{
        Catalog, CreateConflictPolicy, TableDefinition, TableName, TypeDefinition, TypeName,
        ViewDefinition,
    },
    common::{DataType, Error, Result, Row, Value},
    parallel::QueryContext,
    storage::{
        RowId, UpdateMode,
        format::{DUCKDB_FORMAT, FormatId, StorageVersion},
        log::{LogAppend, LogCheckpoint, LogSession, LogStart, TransactionChange, TransactionLog},
        table::Snapshot,
    },
};
use std::collections::{BTreeMap, BTreeSet};

pub struct DuckDbTransactionLog;

#[derive(Clone)]
struct TableState {
    definition: TableDefinition,
    logical_next: RowId,
    physical_next: RowId,
    // Unmapped IDs retain their original checkpoint identity.
    remapped: BTreeMap<RowId, RowId>,
}

#[derive(Clone, Default)]
struct Session {
    tables: BTreeMap<TableName, TableState>,
    views: BTreeMap<TableName, ViewDefinition>,
    types: BTreeMap<TypeName, TypeDefinition>,
    entries: usize,
    storage_version: Option<u64>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TransactionLog for DuckDbTransactionLog {
    fn name(&self) -> &'static str {
        "duckdb-wal-v2-writer"
    }
    fn format_id(&self) -> FormatId {
        DUCKDB_FORMAT
    }
    fn start(&self, snapshot: &Snapshot, context: &QueryContext) -> Result<LogStart> {
        Self::start_session(snapshot, None, context)
    }
    fn start_at(
        &self,
        snapshot: &Snapshot,
        version: Option<StorageVersion>,
        context: &QueryContext,
    ) -> Result<LogStart> {
        context.check()?;
        if let Some(version) = version {
            if version.format != DUCKDB_FORMAT {
                return Err(Error::Unsupported(
                    "checkpoint and transaction log format families differ".into(),
                ));
            }
            super::super::write_support::new_headers(version.version)?;
        }
        Self::start_session(snapshot, version.map(|version| version.version), context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl DuckDbTransactionLog {
    fn start_session(
        snapshot: &Snapshot,
        version: Option<u64>,
        context: &QueryContext,
    ) -> Result<LogStart> {
        context.check()?;
        let mut session = Session {
            storage_version: version,
            ..Session::default()
        };
        for definition in snapshot.named_types()? {
            definition.validate()?;
            if version.is_none() {
                super::super::write_support::wal_type(&definition.data_type)?;
            } else {
                super::super::write_support::wal_type_at(&definition.data_type, version)?;
            }
            session.types.insert(definition.name.clone(), definition);
        }
        for definition in snapshot.tables()? {
            for column in &definition.columns {
                if version.is_none() {
                    super::super::write_support::wal_type(&column.data_type)?;
                } else {
                    super::super::write_support::wal_type_at(&column.data_type, version)?;
                }
            }
            let next = snapshot.next_row_id(&definition.name)?;
            session.tables.insert(
                definition.name.clone(),
                TableState {
                    definition,
                    logical_next: next,
                    physical_next: next,
                    remapped: BTreeMap::new(),
                },
            );
        }
        for definition in snapshot.views()? {
            session.views.insert(definition.name.clone(), definition);
        }
        Ok(LogStart {
            header: vec![100, 0, 98, 101, 0, 2, 255, 255],
            session: Box::new(session),
        })
    }
}

#[derive(Default)]
struct Pending {
    deleted: BTreeSet<RowId>,
    rows: BTreeMap<RowId, Row>,
    order: Vec<RowId>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Pending {
    fn insert(&mut self, id: RowId, row: Row) {
        if !self.rows.contains_key(&id) {
            self.order.push(id);
        }
        self.rows.insert(id, row);
    }
    fn relocate(&mut self, id: RowId, row: Row) {
        if let Some(position) = self.order.iter().position(|candidate| *candidate == id) {
            self.order.remove(position);
        }
        self.order.push(id);
        self.rows.insert(id, row);
    }
    fn remove(&mut self, id: &RowId) -> Option<Row> {
        self.rows.remove(id)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl LogSession for Session {
    fn rebase(&self, checkpoint: LogCheckpoint<'_>, context: &QueryContext) -> Result<LogStart> {
        use crate::storage::layout::{CheckpointLayout, TableLayout};
        if checkpoint.format.format_id() != DUCKDB_FORMAT {
            return Err(Error::Unsupported(
                "checkpoint and transaction log format families differ".into(),
            ));
        }
        let identity = super::super::CheckpointIdentity::read(checkpoint.bytes)?;
        let version = identity.storage_version();
        if self
            .storage_version
            .is_some_and(|previous| previous != version)
        {
            return Err(invalid("checkpoint storage compatibility changed"));
        }
        let mut next = self.clone();
        next.storage_version = Some(version);
        let logical_types: BTreeMap<_, _> = checkpoint
            .logical
            .named_types()?
            .into_iter()
            .map(|definition| (definition.name.clone(), definition))
            .collect();
        let physical_types: BTreeMap<_, _> = checkpoint
            .physical
            .named_types()?
            .into_iter()
            .map(|definition| (definition.name.clone(), definition))
            .collect();
        if logical_types != next.types || physical_types != logical_types {
            return Err(invalid("checkpoint named-type catalog changed"));
        }
        let logical_views: BTreeMap<_, _> = checkpoint
            .logical
            .views()?
            .into_iter()
            .map(|definition| (definition.name.clone(), definition))
            .collect();
        let physical_views: BTreeMap<_, _> = checkpoint
            .physical
            .views()?
            .into_iter()
            .map(|definition| (definition.name.clone(), definition))
            .collect();
        if !view_catalogs_match(&logical_views, &next.views)
            || !view_catalogs_match(&physical_views, &logical_views)
        {
            return Err(invalid("checkpoint view catalog changed"));
        }
        let mut logical_layout = CheckpointLayout::default();
        if checkpoint.logical.tables()?.len() != next.tables.len()
            || checkpoint.layout.tables.len() != next.tables.len()
        {
            return Err(invalid("checkpoint catalog cardinality"));
        }
        for (name, table) in &mut next.tables {
            context.check()?;
            if table.logical_next != checkpoint.logical.next_row_id(name)? {
                return Err(invalid("logical append identity changed"));
            }
            let layout = checkpoint
                .layout
                .tables
                .get(name)
                .ok_or_else(|| invalid("missing checkpoint table layout"))?;
            let ids = checkpoint.logical.row_ids(name)?;
            if ids.len() != layout.rows.len() {
                return Err(invalid("checkpoint row cardinality"));
            }
            let mut rows = BTreeMap::new();
            let mut remapped = BTreeMap::new();
            for id in ids {
                context.check()?;
                let old = table.physical(id)?;
                let new = *layout
                    .rows
                    .get(&old)
                    .ok_or_else(|| invalid("checkpoint omits a live physical row"))?;
                rows.insert(id, new);
                if id != new {
                    remapped.insert(id, new);
                }
            }
            table.remapped = remapped;
            table.physical_next = layout.next_row_id;
            logical_layout.tables.insert(
                name.clone(),
                TableLayout {
                    rows,
                    next_row_id: layout.next_row_id,
                },
            );
        }
        checkpoint.logical.validate_checkpoint_layout_for(
            checkpoint.physical,
            &logical_layout,
            checkpoint.format,
            context,
        )?;
        next.entries = 0;
        Ok(LogStart {
            header: vec![100, 0, 98, 101, 0, 2, 255, 255],
            session: Box::new(next),
        })
    }
    fn prepare(&self, changes: &[TransactionChange], context: &QueryContext) -> Result<LogAppend> {
        let mut next = self.clone();
        let mut pending = BTreeMap::<TableName, Pending>::new();
        let mut output = Records {
            bytes: Vec::new(),
            entries: self.entries,
        };
        for change in changes {
            context.check()?;
            match change {
                TransactionChange::CreateView {
                    definition,
                    conflict,
                } => {
                    let existing = next.views.contains_key(&definition.name);
                    match (existing, conflict) {
                        (true, CreateConflictPolicy::Error) => {
                            return Err(invalid("duplicate view"));
                        }
                        (true, CreateConflictPolicy::Ignore) => {
                            return Err(invalid("no-op view creation in journal"));
                        }
                        (true, CreateConflictPolicy::Replace) => {
                            output.push(named(6, &definition.name)?)?;
                        }
                        (false, _) => {}
                    }
                    let mut entry = record(5);
                    entry.field(101);
                    entry.boolean(true);
                    super::super::view::write(
                        &mut entry,
                        definition,
                        next.storage_version.unwrap_or(64),
                        context,
                    )?;
                    output.push(entry)?;
                    next.views
                        .insert(definition.name.clone(), definition.clone());
                }
                TransactionChange::DropView(name) => {
                    if next.views.remove(name).is_none() {
                        return Err(invalid("missing dropped view"));
                    }
                    output.push(named(6, name)?)?;
                }
                TransactionChange::CreateType {
                    definition,
                    conflict,
                } => {
                    definition.validate()?;
                    super::super::write_support::wal_type_at(
                        &definition.data_type,
                        next.storage_version,
                    )?;
                    let existing = next.types.contains_key(&definition.name);
                    match (existing, conflict) {
                        (true, CreateConflictPolicy::Error) => {
                            return Err(invalid("duplicate type"));
                        }
                        (true, CreateConflictPolicy::Ignore) => {
                            return Err(invalid("no-op type creation in journal"));
                        }
                        (true, CreateConflictPolicy::Replace) => {
                            output.push(named_type(14, &definition.name, next.storage_version)?)?;
                        }
                        (false, _) => {}
                    }
                    let mut entry = record(13);
                    entry.field(101);
                    entry.boolean(true);
                    type_definition(&mut entry, definition, next.storage_version.unwrap_or(64))?;
                    output.push(entry)?;
                    next.types
                        .insert(definition.name.clone(), definition.clone());
                }
                TransactionChange::DropType(name) => {
                    if next.types.remove(name).is_none() {
                        return Err(invalid("missing dropped type"));
                    }
                    output.push(named_type(14, name, next.storage_version)?)?;
                }
                TransactionChange::CreateSchema(name) | TransactionChange::DropSchema(name) => {
                    output.push(schema(
                        if matches!(change, TransactionChange::CreateSchema(_)) {
                            3
                        } else {
                            4
                        },
                        name,
                        next.storage_version,
                    )?)?;
                }
                TransactionChange::CreateTable(definition) => {
                    for column in &definition.columns {
                        super::super::write_support::wal_type_at(
                            &column.data_type,
                            next.storage_version,
                        )?;
                    }
                    if next.tables.contains_key(&definition.name) {
                        return Err(invalid("duplicate table"));
                    }
                    let mut record = record(1);
                    record.field(101);
                    record.boolean(true);
                    table_definition(
                        &mut record,
                        definition,
                        next.storage_version.unwrap_or(64),
                        context,
                    )?;
                    output.push(record)?;
                    next.tables.insert(
                        definition.name.clone(),
                        TableState {
                            definition: definition.clone(),
                            logical_next: 0,
                            physical_next: 0,
                            remapped: BTreeMap::new(),
                        },
                    );
                }
                TransactionChange::DropTable(name) => {
                    if next.tables.remove(name).is_none() {
                        return Err(invalid("missing dropped table"));
                    }
                    pending.remove(name);
                    output.push(named(2, name)?)?;
                }
                TransactionChange::AlterTable {
                    table,
                    alteration,
                    materialized_rows,
                } => {
                    let mut state = next
                        .tables
                        .remove(table)
                        .ok_or_else(|| invalid("missing altered table"))?;
                    let Some(definition) = alteration.definition(&state.definition)? else {
                        return Err(invalid("no-op alteration in journal"));
                    };
                    for column in &definition.columns {
                        super::super::write_support::wal_type_at(
                            &column.data_type,
                            next.storage_version,
                        )?;
                    }
                    if next.tables.contains_key(&definition.name) {
                        return Err(invalid("altered table collision"));
                    }
                    let mut changes = pending.remove(table).unwrap_or_default();
                    if let crate::catalog::TableAlteration::AddColumn { .. } = alteration {
                        let rows = materialized_rows
                            .as_ref()
                            .ok_or_else(|| invalid("ADD COLUMN journal omits materialized rows"))?;
                        for (id, row) in rows {
                            context.check()?;
                            if !changes.rows.contains_key(id) {
                                changes.deleted.insert(state.physical(*id)?);
                            }
                            changes.insert(*id, row.clone());
                        }
                    } else if !changes.rows.is_empty() {
                        // Native undo entries retain a table version. Emit DML
                        // only after its final catalog version, and never delete
                        // a staged insertion. Constraint validation also checks
                        // the transaction's committed catalog basis.
                        for row in changes.rows.values_mut() {
                            context.check()?;
                            if let crate::catalog::TableAlteration::DropColumn { column, .. } =
                                alteration
                            {
                                row.remove(state.definition.column_index(column)?);
                            }
                        }
                    }
                    if !changes.rows.is_empty() || !changes.deleted.is_empty() {
                        pending.insert(definition.name.clone(), changes);
                    }
                    let mut entry = record(20);
                    let mut encoded = alteration.clone();
                    let retained_add_default = match &mut encoded {
                        crate::catalog::TableAlteration::AddColumn { column, .. } => {
                            column.default.take()
                        }
                        _ => None,
                    };
                    super::alter::write(
                        &mut entry,
                        &state.definition,
                        &encoded,
                        next.storage_version.unwrap_or(64),
                        context,
                    )?;
                    output.push(entry)?;
                    if let Some(expression) = retained_add_default {
                        let crate::catalog::TableAlteration::AddColumn { column, .. } = alteration
                        else {
                            unreachable!("retained ADD default")
                        };
                        let mut entry = record(20);
                        super::alter::write(
                            &mut entry,
                            &definition,
                            &crate::catalog::TableAlteration::SetDefault {
                                column: column.name.clone(),
                                expression: Some(expression),
                            },
                            next.storage_version.unwrap_or(64),
                            context,
                        )?;
                        output.push(entry)?;
                    }
                    state.definition = definition;
                    next.tables.insert(state.definition.name.clone(), state);
                }
                TransactionChange::Insert { table, rows } => {
                    let state = next
                        .tables
                        .get_mut(table)
                        .ok_or_else(|| invalid("missing insert table"))?;
                    let pending = pending.entry(table.clone()).or_default();
                    for row in rows {
                        context.check()?;
                        pending.insert(advance(&mut state.logical_next)?, row.clone());
                    }
                }
                TransactionChange::Update {
                    table,
                    metadata,
                    rows,
                } => {
                    let state = next
                        .tables
                        .get_mut(table)
                        .ok_or_else(|| invalid("missing update table"))?;
                    metadata.validate_for(&state.definition)?;
                    let pending = pending.entry(table.clone()).or_default();
                    if metadata.mode == UpdateMode::Regular {
                        let mut persisted = Vec::new();
                        for (id, row) in rows {
                            context.check()?;
                            if let Some(staged) = pending.rows.get_mut(id) {
                                *staged = row.clone();
                            } else {
                                let physical = state.physical(*id)?;
                                persisted.push((physical, row));
                            }
                        }
                        if !persisted.is_empty() {
                            output.push(named(25, table)?)?;
                            for &column in &metadata.columns {
                                output.update(&state.definition, column, &persisted, context)?;
                            }
                        }
                    } else {
                        for (id, row) in rows {
                            context.check()?;
                            if !pending.rows.contains_key(id) {
                                pending.deleted.insert(state.physical(*id)?);
                            }
                            pending.relocate(*id, row.clone());
                        }
                    }
                }
                TransactionChange::Delete { table, ids } => {
                    let state = next
                        .tables
                        .get_mut(table)
                        .ok_or_else(|| invalid("missing delete table"))?;
                    let pending = pending.entry(table.clone()).or_default();
                    for id in ids {
                        context.check()?;
                        if pending.remove(id).is_none() {
                            pending.deleted.insert(state.physical(*id)?);
                        }
                        state.remapped.remove(id);
                    }
                }
            }
        }
        // Native replay may stage inserts until FLUSH. Refer only to committed
        // rows in deletes and append each surviving changed row once. This also
        // handles indexed key swaps and transient inserts within a transaction.
        for (name, pending) in pending {
            context.check()?;
            let state = next
                .tables
                .get_mut(&name)
                .ok_or_else(|| invalid("missing changed table"))?;
            let Pending {
                deleted,
                rows: mut pending_rows,
                order,
            } = pending;
            output.push(named(25, &name)?)?;
            output.delete(&deleted.into_iter().collect::<Vec<_>>(), context)?;
            let mut rows = Vec::with_capacity(pending_rows.len());
            for logical in order {
                let Some(row) = pending_rows.remove(&logical) else {
                    continue;
                };
                let physical = advance(&mut state.physical_next)?;
                if physical == logical {
                    state.remapped.remove(&logical);
                } else {
                    state.remapped.insert(logical, physical);
                }
                rows.push(row);
            }
            output.insert(&state.definition, &rows, context)?;
        }
        output.push(record(100))?;
        context.check()?;
        next.entries = output.entries;
        Ok(LogAppend {
            bytes: output.bytes,
            next: Box::new(next),
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TableState {
    fn physical(&self, id: RowId) -> Result<RowId> {
        if id >= self.logical_next {
            return Err(invalid("row ID exceeds logical high-water mark"));
        }
        Ok(self.remapped.get(&id).copied().unwrap_or(id))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn invalid(message: &str) -> Error {
    Error::Internal(format!("invalid transaction journal: {message}"))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn view_catalogs_match(
    left: &BTreeMap<TableName, ViewDefinition>,
    right: &BTreeMap<TableName, ViewDefinition>,
) -> bool {
    left.len() == right.len()
        && left.iter().all(|(name, definition)| {
            right.get(name).is_some_and(|other| {
                definition.name == other.name
                    && definition.aliases == other.aliases
                    && definition.names == other.names
                    && definition.types == other.types
                    && definition.query_shape == other.query_shape
            })
        })
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn advance(next: &mut RowId) -> Result<RowId> {
    let id = *next;
    if id >= i64::MAX as u64 {
        return Err(Error::Resource("native WAL row identity exhausted".into()));
    }
    *next += 1;
    Ok(id)
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn record(kind: u64) -> Encoder {
    let mut e = Encoder::default();
    e.property(100, kind);
    e
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn named(kind: u64, name: &TableName) -> Result<Encoder> {
    let mut e = record(kind);
    e.field(101);
    e.string(&name.schema)?;
    e.field(102);
    e.string(&name.name)?;
    Ok(e)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn named_type(kind: u64, name: &TypeName, version: Option<u64>) -> Result<Encoder> {
    let mut e = record(kind);
    if version.is_some_and(|version| version >= 69) {
        e.field(103);
        e.property(100, 2);
        e.string(&name.schema)?;
        e.string(&name.name)?;
        e.end();
    } else {
        e.field(101);
        e.string(&name.schema)?;
        e.field(102);
        e.string(&name.name)?;
    }
    Ok(e)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn schema(kind: u64, name: &str, version: Option<u64>) -> Result<Encoder> {
    if name.is_empty() {
        return Err(invalid("empty schema name"));
    }
    let mut e = record(kind);
    if version.is_some_and(|version| version >= 69) {
        e.field(102);
        e.property(100, 1);
        e.string(name)?;
        e.end();
    } else {
        e.field(101);
        e.string(name)?;
    }
    Ok(e)
}

struct Records {
    bytes: Vec<u8>,
    entries: usize,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Records {
    fn push(&mut self, mut e: Encoder) -> Result<()> {
        if self.entries >= MAX_ENTRIES {
            return Err(Error::Resource("WAL exceeds one million entries".into()));
        }
        e.end();
        append_frame(&e.0, &mut self.bytes)?;
        self.entries += 1;
        Ok(())
    }
    fn insert(
        &mut self,
        definition: &TableDefinition,
        rows: &[Row],
        context: &QueryContext,
    ) -> Result<()> {
        let types: Vec<_> = definition
            .columns
            .iter()
            .map(|c| c.data_type.clone())
            .collect();
        if types.is_empty() || types.len() > 16384 {
            return Err(Error::Resource("WAL column count out of range".into()));
        }
        for rows in rows.chunks(2048.min(16_777_216 / types.len())) {
            let mut e = record(26);
            e.field(101);
            chunk(&mut e, &types, rows, context)?;
            self.push(e)?;
        }
        Ok(())
    }
    fn delete(&mut self, ids: &[RowId], context: &QueryContext) -> Result<()> {
        for ids in ids.chunks(2048) {
            let rows: Vec<_> = ids
                .iter()
                .map(|&id| vec![Value::Integer(i128::from(id))])
                .collect();
            let mut e = record(27);
            e.field(101);
            chunk(&mut e, &[DataType::BigInt], &rows, context)?;
            self.push(e)?;
        }
        Ok(())
    }
    fn update(
        &mut self,
        definition: &TableDefinition,
        column: usize,
        rows: &[(RowId, &Row)],
        context: &QueryContext,
    ) -> Result<()> {
        let data_type = &definition
            .columns
            .get(column)
            .ok_or_else(|| invalid("update column"))?
            .data_type;
        let values = rows
            .iter()
            .map(|(id, row)| {
                Ok((
                    *id,
                    row.get(column)
                        .ok_or_else(|| invalid("update row width"))?
                        .clone(),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        self.update_node(data_type, vec![column], &values, context)
    }
    fn update_node(
        &mut self,
        data_type: &DataType,
        path: Vec<usize>,
        values: &[(RowId, Value)],
        context: &QueryContext,
    ) -> Result<()> {
        use crate::common::{NestedPayload, NestedType};
        let fields: Option<Vec<&DataType>> = match data_type {
            DataType::Nested(metadata) => match metadata.as_ref() {
                NestedType::Struct(fields) => Some(fields.iter().map(|(_, ty)| ty).collect()),
                NestedType::Tuple(fields) => Some(fields.iter().collect()),
                _ => return Err(invalid("regular update nested type")),
            },
            _ => None,
        };
        if let Some(fields) = fields {
            for (index, child_type) in fields.into_iter().enumerate() {
                let children = values
                    .iter()
                    .map(|(id, value)| {
                        let child = match value {
                            Value::Null => Value::Null,
                            Value::Nested(value) => match &value.payload {
                                NestedPayload::Struct(values) => values
                                    .get(index)
                                    .ok_or_else(|| invalid("regular update STRUCT arity"))?
                                    .clone(),
                                _ => return Err(invalid("regular update nested payload")),
                            },
                            _ => return Err(invalid("regular update nested value")),
                        };
                        Ok((*id, child))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let mut child_path = path.clone();
                child_path.push(index + 1);
                self.update_node(child_type, child_path, &children, context)?;
            }
        } else {
            self.update_values(data_type, &path, values, false, context)?;
        }
        let mut validity_path = path;
        validity_path.push(0);
        self.update_values(&DataType::Boolean, &validity_path, values, true, context)
    }
    fn update_values(
        &mut self,
        data_type: &DataType,
        path: &[usize],
        values: &[(RowId, Value)],
        validity: bool,
        context: &QueryContext,
    ) -> Result<()> {
        for values in values.chunks(2048) {
            let rows = values
                .iter()
                .map(|(id, value)| {
                    vec![
                        if validity {
                            if value.is_null() {
                                Value::Null
                            } else {
                                Value::Boolean(true)
                            }
                        } else {
                            value.clone()
                        },
                        Value::Integer(i128::from(*id)),
                    ]
                })
                .collect::<Vec<_>>();
            let mut e = record(28);
            e.property(101, path.len() as u64);
            for &index in path {
                e.unsigned(index as u64);
            }
            e.field(102);
            chunk(
                &mut e,
                &[data_type.clone(), DataType::BigInt],
                &rows,
                context,
            )?;
            self.push(e)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // Exercises private session framing beside `Records`.
mod type_tests {
    use super::*;
    use crate::storage::duckdb::binary::{Reader, u64_at};

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn type_(label: &str) -> TypeDefinition {
        TypeDefinition::enumeration(TypeName::main("mood"), vec![label.into()]).unwrap()
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn session(version: u64, definitions: Vec<TypeDefinition>) -> Session {
        Session {
            types: definitions
                .into_iter()
                .map(|definition| (definition.name.clone(), definition))
                .collect(),
            storage_version: Some(version),
            ..Session::default()
        }
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn payloads(bytes: &[u8]) -> Result<Vec<&[u8]>> {
        let mut position = 0;
        let mut result = Vec::new();
        while position < bytes.len() {
            let size = usize::try_from(u64_at(bytes, position)?)
                .map_err(|_| Error::Corrupt("test WAL frame size".into()))?;
            let begin = position + 16;
            let end = begin + size;
            result.push(
                bytes
                    .get(begin..end)
                    .ok_or_else(|| Error::Corrupt("test WAL frame truncation".into()))?,
            );
            position = end;
        }
        Ok(result)
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn kind(payload: &[u8]) -> Result<u64> {
        let mut reader = Reader::new(payload.to_vec());
        reader.field(100)?;
        reader.unsigned()
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn named_enum_wal_create_drop_and_replace_follow_native_record_order() -> Result<()> {
        let query = QueryContext::background();
        let created = session(69, vec![]).prepare(
            &[TransactionChange::CreateType {
                definition: type_("new"),
                conflict: CreateConflictPolicy::Error,
            }],
            &query,
        )?;
        assert_eq!(
            payloads(&created.bytes)?
                .into_iter()
                .map(kind)
                .collect::<Result<Vec<_>>>()?,
            [13, 100]
        );

        let replaced = session(69, vec![type_("old")]).prepare(
            &[TransactionChange::CreateType {
                definition: type_("new"),
                conflict: CreateConflictPolicy::Replace,
            }],
            &query,
        )?;
        assert_eq!(
            payloads(&replaced.bytes)?
                .into_iter()
                .map(kind)
                .collect::<Result<Vec<_>>>()?,
            [14, 13, 100]
        );

        let dropped = session(68, vec![type_("old")]).prepare(
            &[TransactionChange::DropType(TypeName::main("mood"))],
            &query,
        )?;
        assert_eq!(
            payloads(&dropped.bytes)?
                .into_iter()
                .map(kind)
                .collect::<Result<Vec<_>>>()?,
            [14, 100]
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn named_enum_wal_drop_names_switch_at_storage_69() -> Result<()> {
        for version in [68, 69] {
            let append = session(version, vec![type_("old")]).prepare(
                &[TransactionChange::DropType(TypeName::main("mood"))],
                &QueryContext::background(),
            )?;
            let payload = payloads(&append.bytes)?[0];
            let mut reader = Reader::new(payload.to_vec());
            reader.field(100)?;
            assert_eq!(reader.unsigned()?, 14);
            if version < 69 {
                reader.field(101)?;
                assert_eq!(reader.string()?, "main");
                reader.field(102)?;
                assert_eq!(reader.string()?, "mood");
            } else {
                reader.field(103)?;
                reader.field(100)?;
                assert_eq!(reader.length()?, 2);
                assert_eq!(reader.string()?, "main");
                assert_eq!(reader.string()?, "mood");
                reader.end()?;
            }
            reader.end()?;
            assert!(reader.finished());
        }
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn named_enum_wal_rejects_impossible_journal_transitions() {
        let query = QueryContext::background();
        assert!(
            session(69, vec![type_("old")])
                .prepare(
                    &[TransactionChange::CreateType {
                        definition: type_("new"),
                        conflict: CreateConflictPolicy::Ignore,
                    }],
                    &query,
                )
                .is_err()
        );
        assert!(
            session(69, vec![])
                .prepare(
                    &[TransactionChange::DropType(TypeName::main("missing"))],
                    &query,
                )
                .is_err()
        );
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn schema_wal_names_switch_at_storage_69() -> Result<()> {
        for (version, expected) in [
            (68, vec![100, 0, 3, 101, 0, 3, b'a', b'p', b'p', 255, 255]),
            (
                69,
                vec![
                    100, 0, 3, 102, 0, 100, 0, 1, 3, b'a', b'p', b'p', 255, 255, 255, 255,
                ],
            ),
        ] {
            for kind in [3, 4] {
                let mut encoded = schema(kind, "app", Some(version))?;
                encoded.end();
                let mut expected = expected.clone();
                expected[2] = kind as u8;
                assert_eq!(encoded.0, expected, "kind {kind} v{version}");
            }
        }
        assert!(schema(3, "", Some(69)).is_err());
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn chunk(e: &mut Encoder, types: &[DataType], rows: &[Row], context: &QueryContext) -> Result<()> {
    context.check_rows(rows.len())?;
    if rows.iter().any(|row| row.len() != types.len()) {
        return Err(invalid("row width"));
    }
    e.property(100, rows.len() as u64);
    e.property(101, types.len() as u64);
    for data_type in types {
        primitive::write_type(e, data_type)?;
    }
    e.property(102, types.len() as u64);
    let mut remaining = super::nested::MAX_CELLS;
    for (column, data_type) in types.iter().enumerate() {
        vector(
            e,
            data_type,
            rows.iter().map(|row| &row[column]),
            0,
            &mut remaining,
            context,
        )?;
        e.end();
    }
    e.end();
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn vector<'a>(
    e: &mut Encoder,
    data_type: &DataType,
    values: impl ExactSizeIterator<Item = &'a Value> + Clone,
    depth: usize,
    remaining: &mut usize,
    context: &QueryContext,
) -> Result<()> {
    if depth > 64 {
        return Err(Error::Resource("WAL vector nesting exceeds 64".into()));
    }
    super::nested::charge(remaining, values.len(), context)?;
    let bound = context.types().bind(data_type)?;
    for value in values.clone() {
        bound.validate(value, context)?;
    }
    let nullable = values.clone().any(Value::is_null);
    e.field(100);
    e.boolean(nullable);
    if nullable {
        let mut mask = vec![255; values.len().div_ceil(64) * 8];
        for (i, value) in values.clone().enumerate() {
            if value.is_null() {
                mask[i / 8] &= !(1 << (i % 8));
            }
        }
        e.field(101);
        e.blob(&mask);
    }
    if let DataType::Nested(metadata) = data_type {
        // Nested values share their payload behind Arc. This temporary column
        // therefore clones only outer handles; scalar VARCHAR/BLOB columns
        // remain borrowed and are copied directly into the serialized stream.
        let values = values.cloned().collect::<Vec<_>>();
        return super::nested::write(e, metadata, &values, depth, remaining, context);
    }
    e.field(102);
    if matches!(
        data_type,
        DataType::Varchar | DataType::Blob | DataType::Bit | DataType::Bignum
    ) {
        e.unsigned(values.len() as u64);
        for value in values {
            context.check()?;
            match value {
                Value::Varchar(text) => e.string(text)?,
                Value::Blob(bytes) => e.blob(bytes),
                Value::Bit(value) => e.blob(&value.to_native(|| context.check())?),
                Value::Bignum(value) => e.blob(&value.to_native(|| context.check())?),
                Value::Null => e.string("")?,
                _ => return Err(invalid("string physical type")),
            }
        }
    } else {
        let width = primitive::width(data_type)?;
        let mut bytes = Vec::with_capacity(values.len() * width);
        for value in values {
            context.check()?;
            match value {
                Value::Null => bytes.resize(bytes.len() + width, 0),
                Value::Boolean(value) => bytes.push(u8::from(*value)),
                Value::Integer(value) => bytes.extend(&value.to_le_bytes()[..width]),
                Value::Unsigned(value) => bytes.extend(&value.to_le_bytes()[..width]),
                Value::Uuid(value) => bytes.extend((*value ^ (1_u128 << 127)).to_le_bytes()),
                Value::Enum(value) => bytes.extend(&value.ordinal.to_le_bytes()[..width]),
                Value::Decimal { value, .. } => bytes.extend(&value.to_le_bytes()[..width]),
                Value::Float(value) => bytes.extend(value.to_le_bytes()),
                Value::Double(value) => bytes.extend(value.to_le_bytes()),
                Value::Date(value) => bytes.extend(value.days().to_le_bytes()),
                Value::Temporal(value) => value.append_storage(&mut bytes)?,
                _ => return Err(invalid("fixed-width physical type")),
            }
        }
        e.blob(&bytes);
    }
    Ok(())
}
