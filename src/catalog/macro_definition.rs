//! Durable scalar-macro metadata. Binding/codec integration is deliberately
//! separate from built-in scalar functions: a macro retains SQL syntax and is
//! expanded in its caller scope.
use serde::{Deserialize, Serialize};

use crate::{common::{Error, Result}, catalog::TableName};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScalarMacroParameter {
    pub name: String,
    /// Parser-rendered unbound default expression. `None` denotes required.
    pub default_sql: Option<String>,
    /// Exact pinned ParsedExpression representation used by native files.
    #[serde(default)]
    pub native_default: Option<crate::catalog::expression::StoredExpression>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScalarMacroDefinition {
    pub name: TableName,
    pub parameters: Vec<ScalarMacroParameter>,
    /// Parser-rendered, unbound scalar expression; it is never evaluated at
    /// CREATE time and must be rebound after syntax substitution at each call.
    pub body_sql: String,
    /// Exact pinned ParsedExpression representation used by native files.
    #[serde(default)]
    pub native_body: Option<crate::catalog::expression::StoredExpression>,
    pub dependencies: Vec<TableName>,
}

impl ScalarMacroDefinition {
    pub fn validate(&self) -> Result<()> {
        if self.body_sql.trim().is_empty() { return Err(Error::Bind("macro body is empty".into())); }
        let mut names = std::collections::BTreeSet::new(); let mut defaulted = false;
        for parameter in &self.parameters {
            if parameter.name.is_empty() || !names.insert(parameter.name.to_ascii_lowercase()) { return Err(Error::Bind("duplicate macro parameter".into())); }
            defaulted |= parameter.default_sql.is_some();
            if defaulted && parameter.default_sql.is_none() { return Err(Error::Bind("required macro parameter follows a default".into())); }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scalar_macro_parameter_contract_rejects_ambiguous_signatures() {
        let definition = ScalarMacroDefinition { name: TableName::main("m"), body_sql: "x".into(), native_body: None, dependencies: vec![], parameters: vec![ScalarMacroParameter { name: "x".into(), default_sql: Some("1".into()), native_default: None }, ScalarMacroParameter { name: "y".into(), default_sql: None, native_default: None }] };
        assert!(definition.validate().is_err());
    }
}
