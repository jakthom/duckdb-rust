mod constant;
mod unbound;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn constant_expression(
    reader: &mut super::binary::Reader,
) -> crate::Result<crate::Value> {
    constant::read(reader, 0)
}

use super::{
    binary::{Reader, corrupt},
    columns,
};
use crate::{
    catalog::{CatalogMut, ColumnDefinition, TableDefinition, TableName},
    common::{DataType, Error, Result, Value},
    storage::table::Snapshot,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn load(context: &columns::ReadContext<'_>) -> Result<Snapshot> {
    let blocks = context.blocks;
    context.query.check()?;
    let mut snapshot = Snapshot::new(context.query.type_registry());
    if blocks.root == u64::MAX {
        return Ok(snapshot);
    }
    let mut reader = blocks.metadata((blocks.root, 0))?;
    reader.field(100)?;
    for _ in 0..reader.length()? {
        context.query.check()?;
        reader.field(99)?;
        let kind = reader.unsigned()?;
        reader.field(100)?;
        if !reader.boolean()? {
            return Err(corrupt("null catalog entry"));
        }
        let name = create_base(&mut reader, kind)?;
        match kind {
            2 => {
                snapshot.create_schema(&name.schema, name.schema.eq_ignore_ascii_case("main"))?;
                reader.end()?;
                reader.end()?;
            }
            1 => {
                let definition = table_definition(&mut reader, name)?;
                reader.field(101)?;
                let pointer = reader.pointer()?;
                reader.field(102)?;
                let total = reader.length()?;
                if reader.optional(103)? {
                    for _ in 0..reader.length()? {
                        block_pointer(&mut reader)?;
                    }
                }
                if reader.optional(104)? {
                    indexes(&mut reader)?;
                }
                let next_row_id = reader.optional_unsigned(105, total as u64)?;
                reader.end()?;
                let rows = columns::read_table(context, pointer, &definition, total, next_row_id)?;
                let name = definition.name.clone();
                snapshot.create_table(definition, false)?;
                snapshot.restore_slots(&name, rows, next_row_id, context.query)?;
            }
            _ => {
                return Err(Error::Unsupported(format!(
                    "DuckDB catalog entry type {kind}"
                )));
            }
        }
    }
    reader.end()?;
    context.query.check()?;
    Ok(snapshot)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn column(reader: &mut Reader) -> Result<ColumnDefinition> {
    let name = if reader.optional(100)? {
        reader.string()?
    } else {
        return Err(corrupt("column without a name"));
    };
    reader.field(101)?;
    let data_type = logical_type(reader)?;
    let default = if reader.optional(102)? && reader.boolean()? {
        Some(crate::catalog::expression::StoredExpression::literal(
            data_type.clone(),
            constant::read(reader, 0)?.cast(&data_type)?,
        ))
    } else {
        None
    };
    reader.field(103)?;
    if reader.unsigned()? != 0 {
        return Err(Error::Unsupported("generated DuckDB column".into()));
    }
    reader.field(104)?;
    reader.unsigned()?;
    reader.end()?;
    Ok(ColumnDefinition {
        default,
        ..ColumnDefinition::new(name, data_type)
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn logical_type(reader: &mut Reader) -> Result<DataType> {
    logical_type_at(reader, 0)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn logical_type_at(reader: &mut Reader, depth: usize) -> Result<DataType> {
    if depth > 64 {
        return Err(Error::Resource("native type nesting exceeds 64".into()));
    }
    reader.field(100)?;
    let data_type = match reader.unsigned()? {
        1 => DataType::Null,
        4 => unbound::read(reader)?,
        10 => DataType::Boolean,
        11 => DataType::TinyInt,
        12 => DataType::SmallInt,
        13 => DataType::Integer,
        14 => DataType::BigInt,
        15 => DataType::Date,
        16 => DataType::Time,
        17 => DataType::TimestampS,
        18 => DataType::TimestampMs,
        19 => DataType::Timestamp,
        20 => DataType::TimestampNs,
        27 => DataType::Interval,
        32 => DataType::TimestampTz,
        33 => DataType::TimestampTzNs,
        34 => DataType::TimeTz,
        35 => DataType::TimeNs,
        22 => DataType::Float,
        23 => DataType::Double,
        25 => DataType::Varchar,
        26 => DataType::Blob,
        36 => DataType::Bit,
        39 => DataType::Bignum,
        54 => DataType::Uuid,
        50 => DataType::HugeInt,
        28 => DataType::UTinyInt,
        29 => DataType::USmallInt,
        30 => DataType::UInteger,
        31 => DataType::UBigInt,
        49 => DataType::UHugeInt,
        104 => {
            reader.field(101)?;
            if !reader.boolean()? {
                return Err(corrupt("missing ENUM metadata"));
            }
            reader.field(100)?;
            if reader.unsigned()? != 6 {
                return Err(corrupt("invalid ENUM type info"));
            }
            if reader.optional(101)? && !reader.string()?.is_empty() {
                return Err(Error::Unsupported("aliased ENUM metadata".into()));
            }
            reader.field(200)?;
            let size = reader.length()?;
            reader.field(201)?;
            if reader.length()? != size {
                return Err(corrupt("ENUM dictionary size mismatch"));
            }
            let mut labels = Vec::with_capacity(size);
            let mut remaining = 16_usize * 1024 * 1024;
            for _ in 0..size {
                let label = reader.string()?;
                remaining = remaining
                    .checked_sub(label.len())
                    .ok_or_else(|| Error::Resource("ENUM labels exceed 16 MiB".into()))?;
                labels.push(label);
            }
            reader.end()?;
            DataType::enumeration(labels).map_err(|_| corrupt("invalid ENUM dictionary"))?
        }
        id @ (100 | 101 | 102 | 107 | 108 | 109 | 110) => {
            super::nested::read_type(reader, id, depth)?
        }
        21 => {
            reader.field(101)?;
            if !reader.boolean()? {
                return Err(corrupt("missing decimal metadata"));
            }
            reader.field(100)?;
            if reader.unsigned()? != 2 {
                return Err(corrupt("invalid decimal type info"));
            }
            if reader.optional(101)? && !reader.string()?.is_empty() {
                return Err(Error::Unsupported("aliased decimal metadata".into()));
            }
            let width = u8::try_from(reader.optional_unsigned(200, 0)?)
                .map_err(|_| corrupt("decimal width overflow"))?;
            let scale = u8::try_from(reader.optional_unsigned(201, 0)?)
                .map_err(|_| corrupt("decimal scale overflow"))?;
            reader.end()?;
            let data_type = DataType::Decimal { width, scale };
            crate::common::type_registry::check_metadata(&data_type)
                .map_err(|_| corrupt("invalid decimal metadata"))?;
            data_type
        }
        id => return Err(Error::Unsupported(format!("DuckDB logical type {id}"))),
    };
    reader.end()?;
    Ok(data_type)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn constraints(reader: &mut Reader, table: &mut TableDefinition) -> Result<()> {
    for _ in 0..reader.length()? {
        if !reader.boolean()? {
            return Err(corrupt("null constraint"));
        }
        reader.field(100)?;
        match reader.unsigned()? {
            1 => {
                reader.field(200)?;
                let index = reader.length()?;
                table
                    .columns
                    .get_mut(index)
                    .ok_or_else(|| corrupt("constraint column outside table"))?
                    .nullable = false;
            }
            3 => {
                let primary = reader.optional(200)? && reader.boolean()?;
                reader.field(201)?;
                let index = reader.unsigned()?;
                let mut key = Vec::new();
                if reader.optional(202)? {
                    for _ in 0..reader.length()? {
                        let name = reader.string()?;
                        key.push(
                            table
                                .columns
                                .iter()
                                .position(|c| c.name.eq_ignore_ascii_case(&name))
                                .ok_or_else(|| corrupt("unknown constraint column"))?,
                        );
                    }
                }
                if key.is_empty() {
                    key.push(
                        usize::try_from(index).map_err(|_| corrupt("constraint index overflow"))?,
                    );
                }
                for &index in &key {
                    let column = table
                        .columns
                        .get_mut(index)
                        .ok_or_else(|| corrupt("constraint outside table"))?;
                    if primary {
                        column.nullable = false;
                    }
                }
                table.unique_keys.push(crate::catalog::UniqueKey {
                    columns: key,
                    primary,
                });
            }
            kind => return Err(Error::Unsupported(format!("DuckDB constraint type {kind}"))),
        }
        reader.end()?;
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn block_pointer(reader: &mut Reader) -> Result<(i64, usize)> {
    reader.field(100)?;
    let id = reader.signed()?;
    let offset = reader.optional_unsigned(101, 0)?;
    reader.end()?;
    Ok((
        id,
        usize::try_from(offset).map_err(|_| corrupt("block offset overflow"))?,
    ))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn indexes(reader: &mut Reader) -> Result<()> {
    for _ in 0..reader.length()? {
        if reader.optional(100)? {
            reader.string()?;
        }
        reader.optional_unsigned(101, 0)?;
        if reader.optional(102)? {
            for _ in 0..reader.length()? {
                reader.optional_unsigned(100, 0)?;
                if reader.optional(101)? {
                    for _ in 0..reader.length()? {
                        reader.unsigned()?;
                    }
                }
                if reader.optional(102)? {
                    for _ in 0..reader.length()? {
                        block_pointer(reader)?;
                    }
                }
                for field in [103, 104, 105] {
                    if reader.optional(field)? {
                        for _ in 0..reader.length()? {
                            reader.unsigned()?;
                        }
                    }
                }
                reader.end()?;
            }
        }
        // Index allocator options affect the foreign physical ART only. Rust
        // reconstructs indexes from validated logical rows on load.
        if reader.optional(103)? {
            for _ in 0..reader.length()? {
                reader.field(0)?;
                reader.string()?;
                reader.field(1)?;
                constant::value(reader)?;
                reader.end()?;
            }
        }
        reader.end()?;
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn create_base(reader: &mut Reader, kind: u64) -> Result<CreateName> {
    reader.field(100)?;
    if reader.unsigned()? != kind {
        return Err(corrupt("catalog type mismatch"));
    }
    if reader.optional(101)? {
        reader.string()?;
    }
    let mut schema = if reader.optional(102)? {
        reader.string()?
    } else {
        "main".into()
    };
    for field in [103, 104] {
        if reader.optional(field)? && reader.boolean()? {
            return Err(Error::Unsupported(
                "temporary or internal catalog entry".into(),
            ));
        }
    }
    reader.field(105)?;
    reader.unsigned()?;
    if reader.optional(106)? && !reader.string()?.is_empty() {
        return Err(Error::Unsupported(
            "persisted DuckDB catalog SQL text".into(),
        ));
    }
    let mut name = None;
    if reader.optional(111)? {
        reader.field(100)?;
        let count = reader.length()?;
        if !(2..=3).contains(&count) {
            return Err(Error::Unsupported(
                "DuckDB nested or incomplete catalog qualification".into(),
            ));
        }
        let mut path = (0..count)
            .map(|_| reader.string())
            .collect::<Result<Vec<_>>>()?;
        reader.end()?;
        let last = path
            .pop()
            .ok_or_else(|| corrupt("missing qualified name"))?;
        schema = path
            .pop()
            .ok_or_else(|| corrupt("missing qualified schema"))?;
        if schema.is_empty() || (kind == 2 && !last.is_empty()) || (kind == 1 && last.is_empty()) {
            return Err(corrupt("invalid qualified catalog name"));
        }
        if kind == 1 {
            name = Some(last);
        }
    }
    Ok(CreateName { schema, name })
}

pub(super) struct CreateName {
    schema: String,
    name: Option<String>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn table_definition(
    reader: &mut Reader,
    qualified: CreateName,
) -> Result<TableDefinition> {
    let name = if reader.optional(200)? {
        reader.string()?
    } else {
        return Err(corrupt("table without a name"));
    };
    if qualified
        .name
        .as_ref()
        .is_some_and(|qualified| qualified != &name)
    {
        return Err(corrupt("qualified and legacy table names disagree"));
    }
    reader.field(201)?;
    reader.field(100)?;
    let mut definitions = Vec::new();
    for _ in 0..reader.length()? {
        definitions.push(column(reader)?);
    }
    reader.end()?;
    let mut definition = TableDefinition {
        name: TableName::new(qualified.schema, name),
        columns: definitions,
        unique_keys: Vec::new(),
    };
    if reader.optional(202)? {
        constraints(reader, &mut definition)?;
    }
    reader.end()?;
    Ok(definition)
}
