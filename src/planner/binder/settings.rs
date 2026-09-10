use super::*;
use crate::main::settings::SettingScope;

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
        Ok(BoundStatement::Configure(registry.bind(
            name,
            scope,
            value,
            self.context.query,
        )?))
    }
}
