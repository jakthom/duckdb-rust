//! Binder-private named-type resolution. Named types share schema search rules
//! with tables, but intentionally occupy a separate catalog namespace.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct UnresolvedTypeName {
    schema: Option<String>,
    type_: String,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl UnresolvedTypeName {
    pub(super) fn parse(name: &ast::ObjectName) -> Result<Self> {
        let parts = name
            .0
            .iter()
            .map(|part| {
                part.as_ident()
                    .map(|identifier| identifier.value.clone())
                    .ok_or_else(|| unsupported(name))
            })
            .collect::<Result<Vec<_>>>()?;
        match parts.as_slice() {
            [type_] => Ok(Self {
                schema: None,
                type_: type_.to_ascii_lowercase(),
            }),
            [schema, type_] if !schema.eq_ignore_ascii_case("temp") => Ok(Self {
                schema: Some(schema.to_ascii_lowercase()),
                type_: type_.to_ascii_lowercase(),
            }),
            [schema, _] if schema.eq_ignore_ascii_case("temp") => {
                Err(unsupported("temporary named types"))
            }
            _ => Err(unsupported("cross-database type names")),
        }
    }

    fn explicit_name(&self) -> Option<crate::catalog::TypeName> {
        self.schema
            .as_deref()
            .map(|schema| crate::catalog::TypeName::new(schema, &self.type_))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn resolve_existing_type(
        &self,
        name: &ast::ObjectName,
        if_exists: bool,
    ) -> Result<Option<crate::catalog::ResolvedType>> {
        let unresolved = UnresolvedTypeName::parse(name)?;
        if if_exists {
            self.resolve_optional_type(&unresolved)
        } else if let Some(resolved) = self.resolve_optional_type(&unresolved)? {
            Ok(Some(resolved))
        } else {
            let required = unresolved
                .explicit_name()
                .unwrap_or_else(|| crate::catalog::TypeName::main(&unresolved.type_));
            self.context.catalog.type_entry(&required).map(Some)
        }
    }

    pub(super) fn resolve_create_type_target(
        &self,
        name: &ast::ObjectName,
    ) -> Result<crate::catalog::TypeName> {
        let unresolved = UnresolvedTypeName::parse(name)?;
        if let Some(name) = unresolved.explicit_name() {
            return Ok(name);
        }
        let path = self
            .context
            .query
            .settings()
            .search_path(self.context.query)?;
        if path
            .entries()
            .first()
            .and_then(|entry| entry.catalog())
            .is_some()
        {
            return Err(unsupported(
                "catalog-qualified search_path entries require attached catalog routing",
            ));
        }
        Ok(crate::catalog::TypeName::new(
            path.current_schema(),
            &unresolved.type_,
        ))
    }

    pub(super) fn resolve_optional_type_name(
        &self,
        name: &ast::ObjectName,
    ) -> Result<Option<crate::catalog::ResolvedType>> {
        self.resolve_optional_type(&UnresolvedTypeName::parse(name)?)
    }

    fn resolve_optional_type(
        &self,
        unresolved: &UnresolvedTypeName,
    ) -> Result<Option<crate::catalog::ResolvedType>> {
        if let Some(name) = unresolved.explicit_name() {
            return optional_entry(self.context.catalog, &name);
        }
        let path = self
            .context
            .query
            .settings()
            .search_path(self.context.query)?;
        for entry in path.entries() {
            self.context.query.check()?;
            if entry.catalog().is_some() {
                return Err(unsupported(
                    "catalog-qualified search_path entries require attached catalog routing",
                ));
            }
            let candidate = crate::catalog::TypeName::new(entry.schema_name(), &unresolved.type_);
            if let Some(resolved) = optional_entry(self.context.catalog, &candidate)? {
                return Ok(Some(resolved));
            }
        }
        optional_entry(
            self.context.catalog,
            &crate::catalog::TypeName::main(&unresolved.type_),
        )
    }
}

fn optional_entry(
    catalog: &dyn crate::catalog::Catalog,
    name: &crate::catalog::TypeName,
) -> Result<Option<crate::catalog::ResolvedType>> {
    match catalog.type_entry_if_exists(name) {
        // Legacy/alternative catalog adapters predate named types. Preserve
        // their custom-type fallback instead of making every type token fail.
        Err(Error::Unsupported(_)) => Ok(None),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::{Catalog, TableDefinition, TableName, TypeDefinition, TypeName},
        common::cast::CastRegistry,
        execution::expression_executor::ScalarEvaluator,
        function::{FunctionRegistry, operator::OperatorRegistry},
        main::settings::{SettingRegistry, SettingValues, SettingsSnapshot},
        parallel::QueryContext,
        parser::{DuckDbParser, Parser},
        planner::{BindContext, Binder, BoundStatement, SqlBinder},
    };
    use std::{collections::BTreeMap, sync::Arc};

    struct TypesCatalog(Vec<TypeDefinition>);

    impl Catalog for TypesCatalog {
        fn schemas(&self) -> Result<Vec<String>> {
            Ok(vec!["main".into(), "s".into()])
        }
        fn table(&self, name: &TableName) -> Result<TableDefinition> {
            Err(Error::Catalog(format!("table {name} does not exist")))
        }
        fn tables(&self) -> Result<Vec<TableDefinition>> {
            Ok(Vec::new())
        }
        fn named_type(&self, name: &TypeName) -> Result<TypeDefinition> {
            self.0
                .iter()
                .find(|definition| definition.name == *name)
                .cloned()
                .ok_or_else(|| Error::Catalog(format!("type {name} does not exist")))
        }
        fn named_types(&self) -> Result<Vec<TypeDefinition>> {
            Ok(self.0.clone())
        }
    }

    fn bind(catalog: &dyn Catalog, sql: &str, search_path: &str) -> Result<BoundStatement> {
        let registry = Arc::new(SettingRegistry::builtins());
        let session: SettingValues = if search_path.is_empty() {
            BTreeMap::new()
        } else {
            BTreeMap::from([(
                "search_path".into(),
                crate::Value::Varchar(search_path.into()),
            )])
        };
        let base = QueryContext::background();
        let settings = SettingsSnapshot::new(
            registry,
            Arc::new(BTreeMap::new()),
            Arc::new(session),
            &base,
        )?;
        let query = base.with_settings(settings);
        let casts = CastRegistry::builtins();
        let functions = FunctionRegistry::builtins();
        let operators = OperatorRegistry::builtins();
        let mut syntax = DuckDbParser.parse(sql)?;
        SqlBinder.bind(
            &syntax.remove(0),
            &BindContext {
                catalog,
                casts: &casts,
                operators: &operators,
                query: &query,
                functions: &functions,
                expressions: &ScalarEvaluator,
                parameters: &[],
            },
        )
    }

    #[test]
    fn parsing_retains_unqualified_provenance_and_rejects_unrouted_names() {
        let name = ast::ObjectName::from(vec![ast::Ident::new("Mood")]);
        let unresolved = UnresolvedTypeName::parse(&name).unwrap();
        assert_eq!(unresolved.type_, "mood");
        assert_eq!(unresolved.explicit_name(), None);

        let temp = ast::ObjectName::from(vec![ast::Ident::new("temp"), ast::Ident::new("mood")]);
        assert!(UnresolvedTypeName::parse(&temp).is_err());
        let deep = ast::ObjectName::from(vec![
            ast::Ident::new("db"),
            ast::Ident::new("main"),
            ast::Ident::new("mood"),
        ]);
        assert!(UnresolvedTypeName::parse(&deep).is_err());
    }

    #[test]
    fn binder_uses_type_namespace_search_path_and_concrete_dictionary() -> Result<()> {
        let main = TypeDefinition::enumeration(TypeName::main("mood"), vec!["main".into()])?;
        let selected = TypeDefinition::enumeration(
            TypeName::new("s", "mood"),
            vec!["sad".into(), "ok".into()],
        )?;
        let catalog = TypesCatalog(vec![main.clone(), selected.clone()]);

        let BoundStatement::CreateTable { definition, .. } =
            bind(&catalog, "CREATE TABLE t(x mood)", "s")?
        else {
            panic!("expected CREATE TABLE")
        };
        assert_eq!(definition.name, TableName::new("s", "t"));
        assert_eq!(definition.columns[0].data_type, selected.data_type);

        let BoundStatement::CreateType {
            definition,
            conflict,
        } = bind(&catalog, "CREATE TYPE IF NOT EXISTS empty AS ENUM ()", "s")?
        else {
            panic!("expected CREATE TYPE")
        };
        assert_eq!(definition.name, TypeName::new("s", "empty"));
        assert_eq!(definition.data_type, DataType::enumeration(Vec::new())?);
        assert_eq!(conflict, CreateConflictPolicy::Ignore);

        let BoundStatement::DropType {
            types,
            if_exists,
            behavior,
        } = bind(&catalog, "DROP TYPE mood RESTRICT", "s")?
        else {
            panic!("expected DROP TYPE")
        };
        assert_eq!(types.len(), 1);
        assert_eq!(types[0].name(), &TypeName::new("s", "mood"));
        assert!(!if_exists);
        assert_eq!(behavior, DropBehavior::Restrict);
        assert!(bind(&catalog, "DROP TYPE mood CASCADE", "s").is_err());
        assert!(bind(&catalog, "DROP TYPE main.mood, s.mood", "s").is_err());
        Ok(())
    }

    #[test]
    fn required_qualified_drop_never_falls_back_to_main() -> Result<()> {
        let catalog = TypesCatalog(vec![TypeDefinition::enumeration(
            TypeName::main("mood"),
            vec!["main".into()],
        )?]);
        assert!(matches!(
            bind(&catalog, "DROP TYPE s.mood", ""),
            Err(Error::Catalog(_))
        ));
        let BoundStatement::DropType { types, .. } =
            bind(&catalog, "DROP TYPE IF EXISTS s.mood", "")?
        else {
            panic!("expected DROP TYPE")
        };
        assert!(types.is_empty());
        Ok(())
    }

    #[test]
    fn binder_rejects_non_enum_aliases_labels_and_unrouted_qualifiers() -> Result<()> {
        let catalog = TypesCatalog(Vec::new());
        for sql in [
            "CREATE TYPE alias AS INTEGER",
            "CREATE TYPE mood AS ENUM (not_a_literal)",
            "CREATE TYPE temp.mood AS ENUM ('x')",
            "CREATE TYPE db.main.mood AS ENUM ('x')",
            "DROP TYPE temp.mood",
            "DROP TYPE db.main.mood",
        ] {
            assert!(bind(&catalog, sql, "").is_err(), "{sql}");
        }
        Ok(())
    }
}
