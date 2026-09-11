use super::*;
use crate::catalog::SearchPath;
use crate::main::settings::SettingScope;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn setting(
        &self,
        name: &ast::ObjectName,
        scope: Option<ast::ContextModifier>,
        expression: Option<&ast::Expr>,
    ) -> Result<BoundStatement> {
        let [part] = name.0.as_slice() else {
            return Err(Error::Bind(
                "configuration names must be unqualified".into(),
            ));
        };
        let name = &part
            .as_ident()
            .ok_or_else(|| unsupported("configuration name expression"))?
            .value;
        let scope = match scope {
            Some(ast::ContextModifier::Global) => Some(SettingScope::Global),
            Some(ast::ContextModifier::Session) => Some(SettingScope::Session),
            Some(ast::ContextModifier::Local) => {
                return Err(unsupported("SET LOCAL is not implemented."));
            }
            None => None,
        };
        let registry = self.context.query.settings().registry();
        let definition = registry.definition(name)?;
        let constant = State {
            context: self.context,
            parameters_allowed: false,
            ctes: BTreeMap::new(),
            outer: Vec::new(),
        };
        let value = expression
            .map(|expression| -> Result<Value> {
                let value = match expression {
                    ast::Expr::Identifier(id) => Value::Varchar(id.value.clone()),
                    _ => constant.literal(expression)?,
                };
                let cast = BoundExpr::literal(value).cast(
                    definition.data_type.clone(),
                    CastMode::Explicit,
                    self.context.casts,
                    self.context.query.types(),
                )?;
                let value =
                    self.context
                        .expressions
                        .evaluate(&cast, &Vec::new(), self.context.query)?;
                self.context
                    .query
                    .types()
                    .bind(&definition.data_type)?
                    .validate(&value, self.context.query)
                    .map_err(|error| match error {
                        Error::Conversion(_) => Error::Internal(
                            "constant evaluator returned an invalid logical value".into(),
                        ),
                        other => other,
                    })?;
                Ok(value)
            })
            .transpose()?;
        let change = registry.bind(name, scope, value, self.context.query)?;
        if change.name() == "search_path"
            && let Some(value) = change.value()
        {
            self.validate_search_path(value)?;
        }
        Ok(BoundStatement::Configure(change))
    }

    fn validate_search_path(&self, value: &Value) -> Result<()> {
        let Value::Varchar(value) = value else {
            return Err(Error::Internal(
                "normalized search_path is not VARCHAR".into(),
            ));
        };
        let path = SearchPath::from_setting(value)?;
        let schemas = self.context.catalog.schemas()?;
        for entry in path.entries() {
            self.context.query.check()?;
            if entry.catalog().is_some() {
                return Err(unsupported(
                    "catalog-qualified search_path entries require attached catalog routing",
                ));
            }
            if !schemas
                .iter()
                .any(|schema| schema.eq_ignore_ascii_case(entry.schema_name()))
            {
                return Err(Error::Catalog(format!(
                    "SET search_path: No catalog + schema named \"{}\" found.",
                    entry
                )));
            }
        }
        Ok(())
    }
}
