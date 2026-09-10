use super::{
    super::{
        binary::{Reader, corrupt},
        catalog,
    },
    chunk,
};
use crate::{
    catalog::{Catalog, TableDefinition, TableName},
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
    storage::{RowId, recovery::RecoveredChange as Change},
};
use std::collections::BTreeMap;

pub(super) struct RecordState {
    tables: BTreeMap<TableName, TableDefinition>,
    selected: Option<TableName>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl RecordState {
    pub fn new(catalog: &dyn Catalog) -> Result<Self> {
        Ok(Self {
            tables: catalog
                .tables()?
                .into_iter()
                .map(|t| (t.name.clone(), t))
                .collect(),
            selected: None,
        })
    }
    pub fn read(
        &mut self,
        kind: u64,
        reader: &mut Reader,
        context: &QueryContext,
    ) -> Result<Option<Change>> {
        let change = match kind {
            1 => {
                reader.field(101)?;
                if !reader.boolean()? {
                    return Err(corrupt("NULL WAL create table"));
                }
                let schema = catalog::create_base(reader, 1)?;
                let table = catalog::table_definition(reader, schema)?;
                if self
                    .tables
                    .insert(table.name.clone(), table.clone())
                    .is_some()
                {
                    return Err(corrupt("duplicate WAL table"));
                }
                Some(Change::CreateTable(table))
            }
            2 => {
                let table = name(reader)?;
                if self.tables.remove(&table).is_none() {
                    return Err(corrupt("WAL drops missing table"));
                }
                if self.selected.as_ref() == Some(&table) {
                    self.selected = None;
                }
                Some(Change::DropTable(table))
            }
            3 | 4 => {
                reader.field(101)?;
                let schema = reader.string()?;
                Some(if kind == 3 {
                    Change::CreateSchema(schema)
                } else {
                    Change::DropSchema(schema)
                })
            }
            25 => {
                let table = name(reader)?;
                if !self.tables.contains_key(&table) {
                    return Err(corrupt("WAL selects missing table"));
                }
                self.selected = Some(table);
                None
            }
            20 => {
                let (table, mut alteration) = super::alter::read(reader)?;
                let before = self
                    .tables
                    .get(&table)
                    .ok_or_else(|| corrupt("WAL alters missing table"))?;
                if let crate::catalog::TableAlteration::SetDefault { column, value } =
                    &mut alteration
                {
                    *value = value.cast(&before.columns[before.column_index(column)?].data_type)?;
                }
                if let Some(definition) = alteration.definition(before)? {
                    if definition.name != table && self.tables.contains_key(&definition.name) {
                        return Err(corrupt("WAL altered table collision"));
                    }
                    if self.selected.as_ref() == Some(&table) {
                        self.selected = Some(definition.name.clone());
                    }
                    self.tables.remove(&table);
                    self.tables.insert(definition.name.clone(), definition);
                }
                Some(Change::AlterTable { table, alteration })
            }
            26..=28 => {
                let table = self
                    .selected
                    .clone()
                    .ok_or_else(|| corrupt("WAL data without a selected table"))?;
                let definition = self
                    .tables
                    .get(&table)
                    .ok_or_else(|| corrupt("WAL table missing"))?;
                let path = if kind == 28 {
                    reader.field(101)?;
                    let length = reader.length()?;
                    if length == 0 || length > 2 {
                        return Err(Error::Unsupported("WAL nested column update".into()));
                    }
                    (0..length)
                        .map(|_| reader.length())
                        .collect::<Result<Vec<_>>>()?
                } else {
                    Vec::new()
                };
                reader.field(if kind == 28 { 102 } else { 101 })?;
                let chunk = chunk::read(reader, context)?;
                Some(match kind {
                    26 => {
                        if !chunk
                            .types
                            .iter()
                            .eq(definition.columns.iter().map(|c| &c.data_type))
                        {
                            return Err(corrupt("WAL insert types differ from table"));
                        }
                        Change::Insert {
                            table,
                            rows: chunk.rows,
                        }
                    }
                    27 => {
                        if chunk.types != [DataType::BigInt] {
                            return Err(corrupt("WAL delete row-ID type"));
                        }
                        Change::Delete {
                            table,
                            ids: chunk
                                .rows
                                .iter()
                                .map(|r| row_id(&r[0]))
                                .collect::<Result<_>>()?,
                        }
                    }
                    28 => {
                        let column = path[0];
                        let expected = &definition
                            .columns
                            .get(column)
                            .ok_or_else(|| corrupt("WAL column index out of bounds"))?
                            .data_type;
                        let validity = path.len() == 2;
                        if validity && path[1] != 0 {
                            return Err(Error::Unsupported("WAL nested column update".into()));
                        }
                        if chunk.types.len() != 2
                            || chunk.types[1] != DataType::BigInt
                            || chunk.types[0]
                                != if validity {
                                    DataType::Boolean
                                } else {
                                    expected.clone()
                                }
                        {
                            return Err(corrupt("WAL update types differ from table"));
                        }
                        if validity {
                            Change::Validity {
                                table,
                                column,
                                values: chunk
                                    .rows
                                    .iter()
                                    .map(|r| Ok((row_id(&r[1])?, !r[0].is_null())))
                                    .collect::<Result<_>>()?,
                            }
                        } else {
                            Change::Update {
                                table,
                                column,
                                values: chunk
                                    .rows
                                    .into_iter()
                                    .map(|mut r| Ok((row_id(&r[1])?, r.remove(0))))
                                    .collect::<Result<_>>()?,
                            }
                        }
                    }
                    _ => unreachable!(),
                })
            }
            other => return Err(Error::Unsupported(format!("DuckDB WAL record {other}"))),
        };
        reader.end()?;
        Ok(change)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn name(reader: &mut Reader) -> Result<TableName> {
    reader.field(101)?;
    let schema = reader.string()?;
    reader.field(102)?;
    let table = reader.string()?;
    if schema.is_empty() || table.is_empty() {
        return Err(corrupt("empty WAL table identity"));
    }
    Ok(TableName::new(schema, table))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn row_id(value: &Value) -> Result<RowId> {
    match value {
        Value::Integer(id) => {
            u64::try_from(*id).map_err(|_| corrupt("negative or overflowing WAL row ID"))
        }
        _ => Err(corrupt("NULL or invalid WAL row ID")),
    }
}
