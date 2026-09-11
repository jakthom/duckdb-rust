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
    storage_version: u64,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl RecordState {
    pub fn new(catalog: &dyn Catalog, storage_version: u64) -> Result<Self> {
        Ok(Self {
            tables: catalog
                .tables()?
                .into_iter()
                .map(|t| (t.name.clone(), t))
                .collect(),
            selected: None,
            storage_version,
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
                let table =
                    catalog::table_definition_at(reader, schema, self.storage_version, context)?;
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
                let (table, mut alteration) =
                    super::alter::read(reader, self.storage_version, context)?;
                let before = self
                    .tables
                    .get(&table)
                    .ok_or_else(|| corrupt("WAL alters missing table"))?;
                if let crate::catalog::TableAlteration::SetDefault { column, expression } =
                    &mut alteration
                {
                    if let Some(default) = expression {
                        let target = before.columns[before.column_index(column)?]
                            .data_type
                            .clone();
                        let (_, value) = default.as_literal().ok_or_else(|| {
                            crate::Error::Unsupported(
                                "native WAL non-literal column default".into(),
                            )
                        })?;
                        *default = crate::catalog::expression::StoredExpression::literal(
                            target.clone(),
                            value.cast(&target)?,
                        );
                    }
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
                    if length == 0 || length > 66 {
                        return Err(Error::Unsupported("WAL column update path depth".into()));
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
                        let (child_path, expected, validity) = update_path(expected, &path[1..])?;
                        if chunk.types.len() != 2
                            || chunk.types[1] != DataType::BigInt
                            || chunk.types[0]
                                != if validity {
                                    DataType::Boolean
                                } else {
                                    expected
                                }
                        {
                            return Err(corrupt("WAL update types differ from table"));
                        }
                        if validity {
                            Change::NestedValidity {
                                table,
                                column,
                                path: child_path,
                                values: chunk
                                    .rows
                                    .iter()
                                    .map(|r| Ok((row_id(&r[1])?, !r[0].is_null())))
                                    .collect::<Result<_>>()?,
                            }
                        } else {
                            Change::NestedUpdate {
                                table,
                                column,
                                path: child_path,
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
    let legacy_schema = if reader.optional(101)? {
        Some(reader.string()?)
    } else {
        None
    };
    let legacy_table = if reader.optional(102)? {
        Some(reader.string()?)
    } else {
        None
    };
    let (schema, table) = if reader.optional(103)? {
        reader.field(100)?;
        if reader.length()? != 2 {
            return Err(Error::Unsupported(
                "WAL nested or incomplete schema qualification".into(),
            ));
        }
        let schema = reader.string()?;
        let table = reader.string()?;
        reader.end()?;
        if legacy_schema.as_ref().is_some_and(|old| old != &schema)
            || legacy_table.as_ref().is_some_and(|old| old != &table)
        {
            return Err(corrupt("WAL legacy and qualified names disagree"));
        }
        (schema, table)
    } else {
        (
            legacy_schema.ok_or_else(|| corrupt("missing WAL schema identity"))?,
            legacy_table.ok_or_else(|| corrupt("missing WAL table identity"))?,
        )
    };
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn update_path(data_type: &DataType, path: &[usize]) -> Result<(Vec<usize>, DataType, bool)> {
    use crate::common::NestedType;
    if path.len() > 65 {
        return Err(corrupt("WAL child-update path depth"));
    }
    let mut current = data_type.clone();
    let mut children = Vec::new();
    for (depth, index) in path.iter().enumerate() {
        if *index == 0 {
            if depth + 1 != path.len() {
                return Err(corrupt("WAL validity path has descendants"));
            }
            return Ok((children, current, true));
        }
        let DataType::Nested(metadata) = &current else {
            return Err(corrupt("WAL scalar child index is not validity"));
        };
        let fields = match metadata.as_ref() {
            NestedType::Struct(fields) => {
                fields.iter().map(|(_, ty)| ty.clone()).collect::<Vec<_>>()
            }
            NestedType::Tuple(fields) => fields.clone(),
            NestedType::Union(fields) => std::iter::once(DataType::UTinyInt)
                .chain(fields.iter().map(|(_, ty)| ty.clone()))
                .collect(),
            _ => {
                return Err(Error::Unsupported(
                    "WAL child updates require STRUCT, TUPLE or UNION".into(),
                ));
            }
        };
        let index = index - 1;
        current = fields
            .get(index)
            .ok_or_else(|| corrupt("WAL child update index out of bounds"))?
            .clone();
        children.push(index);
    }
    if matches!(current, DataType::Nested(_)) {
        return Err(Error::Unsupported(
            "WAL direct nested physical update".into(),
        ));
    }
    Ok((children, current, false))
}

#[cfg(test)]
mod tests {
    use super::super::super::binary::Encoder;
    use super::*;

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn physical_child_paths_separate_container_fields_and_validity() -> Result<()> {
        use crate::common::NestedType;
        let ty = NestedType::Struct(vec![
            ("a".into(), DataType::Integer),
            (
                "b".into(),
                NestedType::Struct(vec![("x".into(), DataType::Varchar)]).data_type(),
            ),
        ])
        .data_type();
        assert_eq!(
            update_path(&ty, &[2, 1, 0])?,
            (vec![1, 0], DataType::Varchar, true)
        );
        assert_eq!(
            update_path(&ty, &[2, 1])?,
            (vec![1, 0], DataType::Varchar, false)
        );
        assert_eq!(
            update_path(&DataType::Integer, &[])?,
            (vec![], DataType::Integer, false)
        );
        assert_eq!(
            update_path(&DataType::Integer, &[0])?,
            (vec![], DataType::Integer, true)
        );
        for path in [&[0, 1][..], &[3], &[1, 1], &[2]] {
            assert!(update_path(&ty, path).is_err());
        }
        let union = NestedType::Union(vec![("i".into(), DataType::Integer)]).data_type();
        assert_eq!(
            update_path(&union, &[1])?,
            (vec![0], DataType::UTinyInt, false)
        );
        assert!(update_path(&NestedType::List(DataType::Integer).data_type(), &[1]).is_err());
        Ok(())
    }

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn wal_names_preserve_legacy_and_bounded_qualified_identity() -> Result<()> {
        let encode = |legacy: bool, path: Option<&[&str]>| -> Result<Vec<u8>> {
            let mut e = Encoder::default();
            if legacy {
                e.field(101);
                e.string("main")?;
                e.field(102);
                e.string("t")?;
            }
            if let Some(path) = path {
                e.field(103);
                e.property(100, path.len() as u64);
                for item in path {
                    e.string(item)?;
                }
                e.end();
            }
            e.end();
            Ok(e.0)
        };
        for (legacy, path) in [
            (true, None),
            (false, Some(&["main", "t"][..])),
            (true, Some(&["main", "t"][..])),
        ] {
            assert_eq!(
                name(&mut Reader::new(encode(legacy, path)?))?,
                TableName::main("t")
            );
        }
        for path in [&["t"][..], &["main", "nested", "t"], &["main", ""]] {
            assert!(name(&mut Reader::new(encode(false, Some(path))?)).is_err());
        }
        assert!(name(&mut Reader::new(encode(true, Some(&["other", "t"]))?)).is_err());
        assert!(name(&mut Reader::new(encode(false, None)?)).is_err());
        let bytes = encode(false, Some(&["main", "t"]))?;
        for end in 0..bytes.len() - 2 {
            assert!(name(&mut Reader::new(bytes[..end].to_vec())).is_err());
        }
        Ok(())
    }
}
