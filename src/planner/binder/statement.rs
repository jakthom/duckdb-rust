use super::*;

impl State<'_, '_> {
    pub(super) fn statement(&mut self, statement: &ast::Statement) -> Result<BoundStatement> {
        use ast::Statement as S;
        match statement {
            S::Query(query) => Ok(BoundStatement::Query(self.query(query)?)),
            S::CreateTable(table) => self.create(table),
            S::CreateSchema {
                schema_name: ast::SchemaName::Simple(name),
                if_not_exists,
                with: None,
                options: None,
                default_collate_spec: None,
                clone: None,
            } => Ok(BoundStatement::CreateSchema {
                name: schema_name(name)?,
                if_not_exists: *if_not_exists,
            }),
            S::Drop {
                object_type: ast::ObjectType::Schema,
                names,
                if_exists,
                cascade: false,
                purge: false,
                temporary: false,
                table: None,
                ..
            } => Ok(BoundStatement::DropSchema {
                names: names.iter().map(schema_name).collect::<Result<_>>()?,
                if_exists: *if_exists,
            }),
            S::Drop {
                object_type: ast::ObjectType::Table,
                names,
                if_exists,
                cascade: false,
                purge: false,
                temporary: false,
                table: None,
                ..
            } => Ok(BoundStatement::DropTable {
                names: names.iter().map(table_name).collect::<Result<_>>()?,
                if_exists: *if_exists,
            }),
            S::Insert(insert) => {
                if insert.on.is_some()
                    || insert.returning.is_some()
                    || insert.or.is_some()
                    || insert.ignore
                    || insert.overwrite
                    || insert.replace_into
                    || !insert.assignments.is_empty()
                    || insert.partitioned.is_some()
                    || insert.output.is_some()
                    || insert.priority.is_some()
                    || insert.insert_alias.is_some()
                    || insert.table_alias.is_some()
                    || !insert.after_columns.is_empty()
                    || !insert.optimizer_hints.is_empty()
                    || insert.settings.is_some()
                    || insert.format_clause.is_some()
                    || insert.multi_table_insert_type.is_some()
                    || !insert.multi_table_into_clauses.is_empty()
                    || !insert.multi_table_when_clauses.is_empty()
                    || insert.multi_table_else_clause.is_some()
                {
                    return Err(unsupported("INSERT modifiers"));
                }
                let ast::TableObject::TableName(name) = &insert.table else {
                    return Err(unsupported("INSERT table function"));
                };
                let table = table_name(name)?;
                let definition = self.context.catalog.table(&table)?;
                let fields = schema(&definition);
                let columns = if insert.columns.is_empty() {
                    (0..fields.len()).collect()
                } else {
                    insert
                        .columns
                        .iter()
                        .map(|name| {
                            let parts = name
                                .0
                                .iter()
                                .map(|p| {
                                    p.as_ident()
                                        .map(|i| i.value.clone())
                                        .ok_or_else(|| unsupported(name))
                                })
                                .collect::<Result<Vec<_>>>()?;
                            resolve(&fields, &parts)
                        })
                        .collect::<Result<Vec<_>>>()?
                };
                if columns.iter().collect::<HashSet<_>>().len() != columns.len() {
                    return Err(Error::Bind("duplicate INSERT column".into()));
                }
                let source = if let Some(source) = &insert.source {
                    self.query(source)?
                } else {
                    LogicalPlan {
                        schema: columns.iter().map(|&i| fields[i].clone()).collect(),
                        node: PlanNode::Values(vec![
                            columns
                                .iter()
                                .map(|&i| {
                                    BoundExpr::literal(definition.columns[i].default.clone()).cast(
                                        fields[i].data_type.clone(),
                                        CastMode::Assignment,
                                        self.context.casts,
                                        self.context.query.types(),
                                    )
                                })
                                .collect::<Result<Vec<_>>>()?,
                        ]),
                    }
                };
                if source.schema.len() != columns.len() {
                    return Err(Error::Bind(
                        "INSERT column count does not match source".into(),
                    ));
                }
                let types = columns
                    .iter()
                    .map(|&i| fields[i].data_type.clone())
                    .collect::<Vec<_>>();
                let source = self.coerce_plan(source, &types, CastMode::Assignment)?;
                Ok(BoundStatement::Insert {
                    table,
                    columns,
                    source,
                })
            }
            S::Update(update) => {
                if update.from.is_some()
                    || update.returning.is_some()
                    || update.limit.is_some()
                    || !update.order_by.is_empty()
                    || update.or.is_some()
                    || update.output.is_some()
                    || !update.optimizer_hints.is_empty()
                {
                    return Err(unsupported("UPDATE modifiers"));
                }
                let (table, fields) = self.mutation_table(&update.table)?;
                let assignments = update
                    .assignments
                    .iter()
                    .map(|assignment| {
                        let ast::AssignmentTarget::ColumnName(name) = &assignment.target else {
                            return Err(unsupported("tuple assignment"));
                        };
                        let parts = name
                            .0
                            .iter()
                            .map(|p| {
                                p.as_ident()
                                    .map(|i| i.value.clone())
                                    .ok_or_else(|| unsupported(name))
                            })
                            .collect::<Result<Vec<_>>>()?;
                        let index = resolve(&fields, &parts)?;
                        Ok((
                            index,
                            self.expr(&assignment.value, &fields, None)?.cast(
                                fields[index].data_type.clone(),
                                CastMode::Assignment,
                                self.context.casts,
                                self.context.query.types(),
                            )?,
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?;
                if assignments
                    .iter()
                    .map(|(i, _)| i)
                    .collect::<HashSet<_>>()
                    .len()
                    != assignments.len()
                {
                    return Err(Error::Bind("duplicate UPDATE assignment".into()));
                }
                let predicate = update
                    .selection
                    .as_ref()
                    .map(|e| self.expr(e, &fields, None).and_then(|e| self.boolean(e)))
                    .transpose()?;
                Ok(BoundStatement::Update {
                    table,
                    assignments,
                    predicate,
                })
            }
            S::Delete(delete) => {
                if delete.using.is_some()
                    || delete.returning.is_some()
                    || delete.limit.is_some()
                    || !delete.order_by.is_empty()
                    || !delete.tables.is_empty()
                    || delete.output.is_some()
                    || !delete.optimizer_hints.is_empty()
                {
                    return Err(unsupported("DELETE modifiers"));
                }
                let (ast::FromTable::WithFromKeyword(from) | ast::FromTable::WithoutKeyword(from)) =
                    &delete.from;
                if from.len() != 1 {
                    return Err(unsupported("multi-table DELETE"));
                }
                let (table, fields) = self.mutation_table(&from[0])?;
                let predicate = delete
                    .selection
                    .as_ref()
                    .map(|e| self.expr(e, &fields, None).and_then(|e| self.boolean(e)))
                    .transpose()?;
                Ok(BoundStatement::Delete { table, predicate })
            }
            S::StartTransaction {
                modes,
                statements,
                modifier: None,
                ..
            } if modes.is_empty() && statements.is_empty() => Ok(BoundStatement::Begin),
            S::Commit {
                chain: false,
                modifier: None,
                ..
            } => Ok(BoundStatement::Commit),
            S::Rollback {
                chain: false,
                savepoint: None,
            } => Ok(BoundStatement::Rollback),
            S::Explain {
                statement,
                analyze: false,
                verbose: false,
                query_plan: false,
                estimate: false,
                format: None,
                options: None,
                ..
            } => Ok(BoundStatement::Explain(Box::new(
                self.statement(statement)?,
            ))),
            _ => Err(unsupported(statement)),
        }
    }

    pub(super) fn mutation_table(
        &mut self,
        table: &ast::TableWithJoins,
    ) -> Result<(TableName, Schema)> {
        if !table.joins.is_empty() {
            return Err(unsupported("joined mutation target"));
        }
        let ast::TableFactor::Table {
            name, args: None, ..
        } = &table.relation
        else {
            return Err(unsupported("mutation target"));
        };
        let name = table_name(name)?;
        let fields = self.factor(&table.relation)?.schema;
        Ok((name, fields))
    }

    pub(super) fn create(&mut self, create: &ast::CreateTable) -> Result<BoundStatement> {
        let supported =
            ast::helpers::stmt_create_table::CreateTableBuilder::new(create.name.clone())
                .columns(create.columns.clone())
                .constraints(create.constraints.clone())
                .if_not_exists(create.if_not_exists)
                .query(create.query.clone())
                .build();
        if *create != supported {
            return Err(unsupported("CREATE TABLE modifiers"));
        }
        let name = table_name(&create.name)?;
        let source = create.query.as_ref().map(|q| self.query(q)).transpose()?;
        let mut columns = Vec::new();
        let mut unique_keys = Vec::new();
        let mut primary_key = false;
        for (index, column) in create.columns.iter().enumerate() {
            let mut definition = ColumnDefinition::new(
                column.name.value.clone(),
                self.data_type(&column.data_type)?,
            );
            for option in &column.options {
                match &option.option {
                    ast::ColumnOption::Null => {}
                    ast::ColumnOption::NotNull => definition.nullable = false,
                    ast::ColumnOption::Default(expr) => {
                        let value = self.literal(expr)?;
                        definition.default = self
                            .context
                            .casts
                            .bind(
                                &value.data_type(),
                                &definition.data_type,
                                CastMode::Assignment,
                                self.context.query.types(),
                            )?
                            .apply(&value, self.context.query)?
                    }
                    ast::ColumnOption::Unique(_) => unique_keys.push(crate::catalog::UniqueKey {
                        columns: vec![index],
                        primary: false,
                    }),
                    ast::ColumnOption::PrimaryKey(_) => {
                        if primary_key {
                            return Err(Error::Bind("multiple primary keys".into()));
                        }
                        primary_key = true;
                        definition.nullable = false;
                        unique_keys.push(crate::catalog::UniqueKey {
                            columns: vec![index],
                            primary: true,
                        });
                    }
                    _ => return Err(unsupported(&option.option)),
                }
            }
            columns.push(definition);
        }
        for constraint in &create.constraints {
            let (key_columns, primary) = match constraint {
                ast::TableConstraint::Unique(key) => (&key.columns, false),
                ast::TableConstraint::PrimaryKey(key) => (&key.columns, true),
                _ => return Err(unsupported(constraint)),
            };
            if primary && primary_key {
                return Err(Error::Bind("multiple primary keys".into()));
            }
            primary_key |= primary;
            let fields: Schema = columns
                .iter()
                .map(|c| Field::new(&c.name, c.data_type.clone()))
                .collect();
            let key = key_columns
                .iter()
                .map(|c| {
                    let expression = &c.column.expr;
                    let ast::Expr::Identifier(name) = expression else {
                        return Err(unsupported("expression constraint"));
                    };
                    resolve(&fields, std::slice::from_ref(&name.value))
                })
                .collect::<Result<Vec<_>>>()?;
            if primary {
                for &i in &key {
                    columns[i].nullable = false;
                }
            }
            unique_keys.push(crate::catalog::UniqueKey {
                columns: key,
                primary,
            });
        }
        if let Some(source) = &source {
            if columns.is_empty() {
                columns = source
                    .schema
                    .iter()
                    .map(|f| ColumnDefinition::new(&f.name, f.data_type.clone()))
                    .collect();
            } else if columns.len() != source.schema.len() {
                return Err(Error::Bind("CREATE TABLE AS column count mismatch".into()));
            }
        }
        let source = source
            .map(|plan| {
                let types = columns
                    .iter()
                    .map(|c| c.data_type.clone())
                    .collect::<Vec<_>>();
                self.coerce_plan(plan, &types, CastMode::Assignment)
            })
            .transpose()?;
        Ok(BoundStatement::CreateTable {
            definition: TableDefinition {
                name,
                columns,
                unique_keys,
            },
            if_not_exists: create.if_not_exists,
            source,
        })
    }
}

fn schema_name(name: &ast::ObjectName) -> Result<String> {
    if name.0.len() != 1 {
        return Err(unsupported("qualified schema names"));
    }
    name.0[0]
        .as_ident()
        .map(|i| i.value.to_ascii_lowercase())
        .ok_or_else(|| unsupported(name))
}
