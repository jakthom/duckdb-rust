//! Native ALTER_INFO records. Encoding and decoding share catalog semantics;
//! neither path invokes the SQL frontend or publishes transaction state.
use super::super::{
    binary::{Encoder, Reader, corrupt},
    catalog, writer,
};
use crate::{
    Error, Result,
    catalog::{TableAlteration, TableDefinition, TableName},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write(
    output: &mut Encoder,
    before: &TableDefinition,
    alteration: &TableAlteration,
    version: u64,
    context: &crate::parallel::QueryContext,
) -> Result<()> {
    output.field(101);
    output.boolean(true);
    output.property(100, 0); // ParseInfoType::ALTER_INFO
    output.property(200, 1); // AlterType::ALTER_TABLE
    output.field(202);
    output.string(&before.name.schema)?;
    output.field(203);
    output.string(&before.name.name)?;
    output.property(204, 0);
    let kind = match alteration {
        TableAlteration::RenameColumn { .. } => 1,
        TableAlteration::RenameTable(_) => 2,
        TableAlteration::AddColumn { .. } => 3,
        TableAlteration::DropColumn { .. } => 4,
        TableAlteration::SetDefault { .. } => 6,
        TableAlteration::SetNullability {
            nullable: false, ..
        } => 8,
        TableAlteration::SetNullability { nullable: true, .. } => 9,
    };
    output.property(300, kind);
    output.field(400);
    match alteration {
        TableAlteration::RenameTable(name) => output.string(name)?,
        TableAlteration::RenameColumn { column, name } => {
            output.string(column)?;
            output.field(401);
            output.string(name)?;
        }
        TableAlteration::AddColumn { column, .. } => writer::column_definition(output, column, version, context)?,
        TableAlteration::DropColumn { column, .. }
        | TableAlteration::SetNullability { column, .. } => output.string(column)?,
        TableAlteration::SetDefault { column, expression } => {
            output.string(column)?;
            if let Some(expression) = expression {
                let (_, value) = expression.as_literal().ok_or_else(|| {
                    Error::Unsupported("native WAL non-literal column default".into())
                })?;
                output.field(401);
                output.boolean(true);
                writer::constant::write(
                    output,
                    value,
                    &before.columns[before.column_index(column)?].data_type,
                )?;
            }
        }
    }
    output.end();
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read(reader: &mut Reader) -> Result<(TableName, TableAlteration)> {
    reader.field(101)?;
    if !reader.boolean()? {
        return Err(corrupt("NULL WAL ALTER"));
    }
    reader.field(100)?;
    if reader.unsigned()? != 0 {
        return Err(corrupt("WAL ALTER parse-info kind"));
    }
    reader.field(200)?;
    if reader.unsigned()? != 1 {
        return Err(Error::Unsupported("WAL catalog alteration".into()));
    }
    if reader.optional(201)? {
        reader.string()?;
    }
    let mut schema = if reader.optional(202)? {
        reader.string()?
    } else {
        "main".into()
    };
    let mut name = if reader.optional(203)? {
        reader.string()?
    } else {
        String::new()
    };
    reader.field(204)?;
    if reader.unsigned()? > 1 {
        return Err(corrupt("WAL ALTER missing-entry policy"));
    }
    if reader.optional(205)? && reader.boolean()? {
        return Err(Error::Unsupported("WAL internal catalog alteration".into()));
    }
    if reader.optional(206)? {
        let mut path = Vec::new();
        if reader.optional(100)? {
            let count = reader.length()?;
            if !(1..=3).contains(&count) {
                return Err(Error::Unsupported(
                    "WAL nested catalog qualification".into(),
                ));
            }
            for _ in 0..count {
                path.push(reader.string()?);
            }
        }
        reader.end()?;
        if let Some(last) = path.pop() {
            name = last;
        }
        if let Some(last) = path.pop() {
            schema = last;
        }
    }
    if name.is_empty() || schema.is_empty() {
        return Err(corrupt("empty WAL ALTER table name"));
    }
    reader.field(300)?;
    let kind = reader.unsigned()?;
    if !matches!(kind, 1..=4 | 6 | 8 | 9) {
        return Err(Error::Unsupported(format!("WAL table alteration {kind}")));
    }
    reader.field(400)?;
    let alteration = match kind {
        1 => {
            let column = reader.string()?;
            reader.field(401)?;
            TableAlteration::RenameColumn {
                column,
                name: reader.string()?,
            }
        }
        2 => TableAlteration::RenameTable(reader.string()?),
        3 => {
            let column = catalog::column(reader)?;
            let if_not_exists = reader.optional(401)? && reader.boolean()?;
            TableAlteration::AddColumn {
                column,
                if_not_exists,
            }
        }
        4 => {
            let column = reader.string()?;
            let if_exists = reader.optional(401)? && reader.boolean()?;
            if reader.optional(402)? && reader.boolean()? {
                return Err(Error::Unsupported("WAL cascading column drop".into()));
            }
            TableAlteration::DropColumn { column, if_exists }
        }
        6 => {
            let column = reader.string()?;
            let expression = if reader.optional(401)? && reader.boolean()? {
                let value = catalog::constant_expression(reader)?;
                Some(crate::catalog::expression::StoredExpression::literal(
                    value.data_type(),
                    value,
                ))
            } else {
                None
            };
            TableAlteration::SetDefault { column, expression }
        }
        8 | 9 => TableAlteration::SetNullability {
            column: reader.string()?,
            nullable: kind == 9,
        },
        _ => unreachable!(),
    };
    reader.end()?;
    Ok((TableName::new(schema, name), alteration))
}
