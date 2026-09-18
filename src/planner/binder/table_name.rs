//! Binder-private table-name shape. Retaining absent schema provenance leaves
//! one deliberate seam for later catalog-aware search-path resolution.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct UnresolvedTableName {
    schema: Option<String>,
    table: String,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl UnresolvedTableName {
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
            [table] => Ok(Self {
                schema: None,
                table: table.to_ascii_lowercase(),
            }),
            [schema, table] => Ok(Self {
                schema: Some(schema.to_ascii_lowercase()),
                table: table.to_ascii_lowercase(),
            }),
            _ => Err(unsupported("cross-database names")),
        }
    }

    pub(super) const fn is_unqualified(&self) -> bool {
        self.schema.is_none()
    }

    pub(super) fn table(&self) -> &str {
        &self.table
    }

    fn explicit_name(&self) -> Option<TableName> {
        self.schema
            .as_deref()
            .map(|schema| TableName::new(schema, &self.table))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn unresolved_table_name(
        &self,
        name: &ast::ObjectName,
    ) -> Result<UnresolvedTableName> {
        UnresolvedTableName::parse(name)
    }

    pub(super) fn resolve_existing_table(
        &self,
        name: &ast::ObjectName,
        if_exists: bool,
    ) -> Result<Option<ResolvedTable>> {
        let unresolved = self.unresolved_table_name(name)?;
        if if_exists {
            self.resolve_optional_table(&unresolved)
        } else if let Some(resolved) = self.resolve_optional_table(&unresolved)? {
            Ok(Some(resolved))
        } else {
            // Preserve the catalog adapter's ordinary missing-table error
            // contract after exhausting the configured search path.
            self.context
                .catalog
                .table_entry(&TableName::main(unresolved.table()))
                .map(Some)
        }
    }

    pub(super) fn resolve_create_target(&self, name: &ast::ObjectName) -> Result<TableName> {
        let unresolved = self.unresolved_table_name(name)?;
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
        Ok(TableName::new(path.current_schema(), unresolved.table()))
    }

    fn resolve_optional_table(
        &self,
        unresolved: &UnresolvedTableName,
    ) -> Result<Option<ResolvedTable>> {
        if let Some(name) = unresolved.explicit_name() {
            return self.context.catalog.table_entry_if_exists(&name);
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
            let candidate = TableName::new(entry.schema_name(), unresolved.table());
            if let Some(resolved) = self.context.catalog.table_entry_if_exists(&candidate)? {
                return Ok(Some(resolved));
            }
        }
        self.context
            .catalog
            .table_entry_if_exists(&TableName::main(unresolved.table()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;

    #[test]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn parsing_retains_unqualified_provenance() {
        let parsed = crate::parser::DuckDbParser
            .parse("SELECT * FROM items")
            .unwrap();
        let crate::parser::Statement::Sql(statement) = &parsed[0] else {
            panic!("expected SQL statement")
        };
        let ast::Statement::Query(query) = statement.as_ref() else {
            panic!("expected query")
        };
        let ast::SetExpr::Select(select) = query.body.as_ref() else {
            panic!("expected select")
        };
        let ast::TableFactor::Table { name, .. } = &select.from[0].relation else {
            panic!("expected table")
        };
        let unresolved = UnresolvedTableName::parse(name).unwrap();
        assert!(unresolved.is_unqualified());
        assert_eq!(unresolved.table(), "items");
        assert_eq!(unresolved.explicit_name(), None);
    }
}
