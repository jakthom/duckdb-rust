//! Pinned `CreateMacroInfo` / `ScalarMacroFunction` AST codec.
use super::{binary::{Encoder, Reader, corrupt}, parsed};
use crate::{catalog::{TableName, expression::{StoredExpression, StoredExpressionKind}, macro_definition::{ScalarMacroDefinition, ScalarMacroParameter}}, common::{Error, Result}, parallel::QueryContext};

const CATALOG_NAME: &str = "duckdb_rust";

pub(super) fn write(output: &mut Encoder, definition: &ScalarMacroDefinition, version: u64, query: &QueryContext) -> Result<()> {
    definition.validate()?;
    let body = definition.native_body.as_ref().ok_or_else(|| Error::Unsupported("native scalar macro body syntax".into()))?;
    // The same AST must be usable by Rust after a C++ origin/reopen. Validate
    // that before any checkpoint or WAL bytes are staged.
    let _ = render(body)?;
    if definition.parameters.iter().any(|p| p.default_sql.is_some() && p.native_default.is_none()) { return Err(Error::Unsupported("native scalar macro default syntax".into())); }
    for p in &definition.parameters { if let Some(value) = &p.native_default { let _ = render(value)?; } }
    output.property(100, 30); // CatalogType::MACRO_ENTRY
    if version < 69 { output.field(101); output.string(CATALOG_NAME)?; output.field(102); output.string(&definition.name.schema)?; }
    output.property(105, 0); // ERROR_ON_CONFLICT
    if version >= 69 { output.field(111); output.property(100, 3); output.string(CATALOG_NAME)?; output.string(&definition.name.schema)?; output.string(&definition.name.name)?; output.end(); }
    output.field(200); output.string(&definition.name.name)?;
    output.field(201); output.boolean(true); // unique_ptr<MacroFunction>
    output.property(100, 2); // MacroType::SCALAR_MACRO
    output.property(101, definition.parameters.len() as u64);
    for p in &definition.parameters { output.boolean(true); parsed::write(output, &column(&p.name), version, query)?; }
    let defaults: Vec<_> = definition.parameters.iter().filter_map(|p| p.native_default.as_ref().map(|v| (&p.name, v))).collect();
    if !defaults.is_empty() { output.property(102, defaults.len() as u64); for (name, value) in defaults { output.field(0); output.string(name)?; output.field(1); output.boolean(true); parsed::write(output, value, version, query)?; output.end(); } }
    output.field(200); output.boolean(true); parsed::write(output, body, version, query)?;
    output.end(); output.end(); Ok(())
}

pub(super) fn read(reader: &mut Reader, qualified: super::catalog::CreateName, version: u64, query: &QueryContext) -> Result<ScalarMacroDefinition> {
    reader.field(200)?; let name = reader.string()?;
    if qualified.name.as_ref().is_some_and(|v| v != &name) { return Err(corrupt("qualified and legacy macro names disagree")); }
    reader.field(201)?; if !reader.boolean()? { return Err(corrupt("NULL scalar macro function")); }
    reader.field(100)?; if reader.unsigned()? != 2 { return Err(Error::Unsupported("native table macro".into())); }
    reader.field(101)?; let count = reader.length()?; if count > 128 { return Err(Error::Resource("native macro parameter limit".into())); }
    let mut parameters = Vec::with_capacity(count);
    for _ in 0..count { if !reader.boolean()? { return Err(corrupt("NULL macro parameter")); } let v = parsed::read(reader, version, query)?; parameters.push(ScalarMacroParameter { name: parameter_name(&v)?, default_sql: None, native_default: None }); }
    if reader.optional(102)? { for _ in 0..reader.length()? { reader.field(0)?; let name = reader.string()?; reader.field(1)?; if !reader.boolean()? { return Err(corrupt("NULL macro default")); } let v = parsed::read(reader, version, query)?; reader.end()?; let p = parameters.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&name)).ok_or_else(|| corrupt("default for unknown macro parameter"))?; p.default_sql = Some(render(&v)?); p.native_default = Some(v); } }
    reader.field(200)?; if !reader.boolean()? { return Err(corrupt("NULL scalar macro body")); } let body = parsed::read(reader, version, query)?;
    reader.end()?; reader.end()?;
    let definition = ScalarMacroDefinition { name: TableName::new(qualified.schema, name), parameters, body_sql: render(&body)?, native_body: Some(body), dependencies: Vec::new() }; definition.validate()?; Ok(definition)
}
fn column(name: &str) -> StoredExpression { StoredExpression { alias: None, source_span: None, kind: StoredExpressionKind::ColumnReference(vec![name.into()]) } }
fn parameter_name(v: &StoredExpression) -> Result<String> { match &v.kind { StoredExpressionKind::ColumnReference(parts) if parts.len() == 1 => Ok(parts[0].clone()), _ => Err(corrupt("macro parameter is not a column reference")) } }
fn render(v: &StoredExpression) -> Result<String> { match &v.kind { StoredExpressionKind::ColumnReference(parts) => Ok(parts.join(".")), StoredExpressionKind::Literal { value, .. } => Ok(value.to_string()), StoredExpressionKind::Function { name, arguments, is_operator: true, .. } if arguments.len() == 2 => Ok(format!("({} {} {})", render(&arguments[0].expression)?, name.join("."), render(&arguments[1].expression)?)), StoredExpressionKind::Function { name, arguments, .. } => Ok(format!("{}({})", name.join("."), arguments.iter().map(|a| a.name.as_ref().map(|name| Ok(format!("{name} := {}", render(&a.expression)?))).unwrap_or_else(|| render(&a.expression))).collect::<Result<Vec<_>>>()?.join(", "))), StoredExpressionKind::Case { checks, otherwise } => Ok(format!("CASE {} ELSE {} END", checks.iter().map(|check| Ok(format!("WHEN {} THEN {}", render(&check.when_expression)?, render(&check.then_expression)?))).collect::<Result<Vec<_>>>()?.join(" "), render(otherwise)?)), _ => Err(Error::Unsupported("native scalar macro expression rendering".into())) } }
