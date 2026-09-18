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
                Ok(match expression {
                    ast::Expr::Identifier(id) => Value::Varchar(id.value.clone()),
                    _ => constant.literal(expression)?,
                })
            })
            .transpose()?;
        // DuckDB's profiling settings are callbacks over one client-config
        // state. RESET enable_profiling therefore disables profiling instead
        // of uncovering an earlier profiling_mode compatibility value.
        let value = if expression.is_none()
            && matches!(
                name.to_ascii_lowercase().as_str(),
                "enable_profiling" | "enable_profile"
            ) {
            Some(Value::Null)
        } else {
            value
        };
        self.setting_value(name, scope, value, definition)
    }

    pub(super) fn pragma(
        &self,
        name: &ast::ObjectName,
        value: Option<&ast::ValueWithSpan>,
    ) -> Result<BoundStatement> {
        let [part] = name.0.as_slice() else {
            return Err(Error::Bind("pragma names must be unqualified".into()));
        };
        let name = part
            .as_ident()
            .ok_or_else(|| unsupported("pragma name expression"))?
            .value
            .to_ascii_lowercase();
        let literal = value
            .map(|value| self.literal(&ast::Expr::Value(value.clone())))
            .transpose()?;
        let (setting, value) = match (name.as_str(), literal) {
            ("enable_verification", None) => ("enable_verification", Some(Value::Boolean(true))),
            ("disable_verification", None) => ("enable_verification", Some(Value::Boolean(false))),
            ("enable_profiling" | "enable_profile", None) => (
                "enable_profiling",
                Some(Value::Varchar("query_tree".into())),
            ),
            ("enable_profiling" | "enable_profile", Some(value)) => {
                ("enable_profiling", Some(value))
            }
            ("disable_profiling" | "disable_profile", None) => {
                ("enable_profiling", Some(Value::Null))
            }
            ("profiling_output" | "profile_output", Some(value)) => {
                ("profiling_output", Some(value))
            }
            ("profiling_mode", Some(value)) => ("profiling_mode", Some(value)),
            ("debug_force_external", Some(value)) => ("debug_force_external", Some(value)),
            (
                "enable_verification"
                | "disable_verification"
                | "disable_profiling"
                | "disable_profile",
                Some(_),
            )
            | ("profiling_output" | "profile_output" | "profiling_mode", None) => {
                return Err(Error::Parse(format!(
                    "pragma {name} does not accept this argument form"
                )));
            }
            _ => return Err(unsupported(format!("PRAGMA {name}"))),
        };
        let definition = self
            .context
            .query
            .settings()
            .registry()
            .definition(setting)?;
        self.setting_value(setting, Some(SettingScope::Session), value, definition)
    }

    fn setting_value(
        &self,
        name: &str,
        scope: Option<SettingScope>,
        value: Option<Value>,
        definition: &crate::main::settings::SettingDefinition,
    ) -> Result<BoundStatement> {
        let value = value
            .map(|value| -> Result<Value> {
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
        let registry = self.context.query.settings().registry();
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
