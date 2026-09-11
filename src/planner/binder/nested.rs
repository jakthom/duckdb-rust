use super::*;
use crate::common::NestedType;
// The catalog's shared capture entrypoint is not wired yet. Keep this helper
// test-only until that caller supplies whole-root budgets and cancellation.
#[cfg(test)]
mod capture;

pub(super) struct ParsedScalarArguments<'a> {
    pub names: Vec<Option<String>>,
    pub aliases: Vec<Option<String>>,
    pub expressions: Vec<&'a ast::Expr>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn scalar_arguments(function: &ast::Function) -> Result<ParsedScalarArguments<'_>> {
    if !matches!(function.parameters, ast::FunctionArguments::None) {
        return Err(unsupported("parametric function"));
    }
    let ast::FunctionArguments::List(list) = &function.args else {
        return Err(unsupported("function arguments"));
    };
    if !list.clauses.is_empty() {
        return Err(unsupported("function argument clauses"));
    }
    let mut result = ParsedScalarArguments {
        names: Vec::new(),
        aliases: Vec::new(),
        expressions: Vec::new(),
    };
    for argument in &list.args {
        let (name, expression) = match argument {
            ast::FunctionArg::Named {
                name,
                arg: ast::FunctionArgExpr::Expr(expression),
                ..
            } => (Some(name.value.clone()), expression),
            ast::FunctionArg::Unnamed(ast::FunctionArgExpr::Expr(expression)) => (None, expression),
            _ => return Err(unsupported("scalar function argument")),
        };
        let alias = name.clone().or_else(|| match expression {
            ast::Expr::Identifier(identifier) => Some(identifier.value.clone()),
            ast::Expr::CompoundIdentifier(identifiers) => {
                identifiers.last().map(|id| id.value.clone())
            }
            _ => None,
        });
        result.names.push(name);
        result.aliases.push(alias);
        result.expressions.push(expression);
    }
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn map_constructor(
        &self,
        entries: Vec<(BoundExpr, BoundExpr)>,
    ) -> Result<BoundExpr> {
        let (keys, values) = entries.into_iter().unzip();
        let keys = self.nested_constructor(keys, None)?;
        let values = self.nested_constructor(values, None)?;
        self.scalar_call("map", vec![keys, values])
    }
    pub(super) fn nested_access(&self, value: BoundExpr, key: BoundExpr) -> Result<BoundExpr> {
        let DataType::Nested(metadata) = &value.data_type else {
            return Err(Error::Bind(
                "nested accessor requires a nested value".into(),
            ));
        };
        let name = match metadata.as_ref() {
            NestedType::Map { .. } => "map_extract_value",
            NestedType::List(_) | NestedType::Array { .. } => "list_extract",
            NestedType::Struct(_) | NestedType::Tuple(_) => "struct_extract",
            NestedType::Union(_) => "union_extract",
            NestedType::Variant => "variant_extract",
            NestedType::Object(_) => {
                return Err(Error::Unsupported(
                    "direct access to internal OBJECT metadata".into(),
                ));
            }
        };
        self.scalar_call(name, vec![value, key])
    }
    pub(super) fn nested_constructor(
        &self,
        arguments: Vec<BoundExpr>,
        names: Option<Vec<String>>,
    ) -> Result<BoundExpr> {
        if let Some(names) = names {
            let function = self.context.functions.scalar("struct_pack")?;
            let names = names.into_iter().map(Some).collect::<Vec<_>>();
            self.scalar_call_selected_named(function, arguments, Some(&names), Some(&names))
        } else {
            self.scalar_call("list_value", arguments)
        }
    }
}
