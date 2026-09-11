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

    fn current_name(&self) -> TableName {
        TableName::new(self.schema.as_deref().unwrap_or("main"), &self.table)
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
        let current = unresolved.current_name();
        if if_exists {
            self.context.catalog.table_entry_if_exists(&current)
        } else {
            self.context.catalog.table_entry(&current).map(Some)
        }
    }

    pub(super) fn resolve_create_target(&self, name: &ast::ObjectName) -> Result<TableName> {
        Ok(self.unresolved_table_name(name)?.current_name())
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
        assert_eq!(unresolved.current_name(), TableName::main("items"));
    }
}
