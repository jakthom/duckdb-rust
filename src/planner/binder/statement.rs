use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl State<'_, '_> {
    pub(super) fn statement(&mut self, statement: &ast::Statement) -> Result<BoundStatement> {
        use ast::Statement as S;
        match statement {
            S::Set(ast::Set::SingleAssignment {
                scope,
                hivevar: false,
                variable,
                values,
            }) if values.len() == 1 => {
                let value = match &values[0] {
                    ast::Expr::Identifier(id)
                        if id.quote_style.is_none() && id.value.eq_ignore_ascii_case("default") =>
                    {
                        None
                    }
                    expression => Some(expression),
                };
                self.setting(variable, *scope, value)
            }
            S::Reset(ast::ResetStatement {
                reset: ast::Reset::ConfigurationParameter(name),
            }) => self.setting(name, None, None),
            S::Pragma { name, value, .. } => self.pragma(name, value.as_ref()),
            S::Query(query) => Ok(BoundStatement::Query(self.query(query)?)),
            S::CreateView(view) => {
                if view.or_alter
                    || view.materialized
                    || view.secure
                    || view.temporary
                    || view.copy_grants
                    || view.with_no_schema_binding
                    || view.to.is_some()
                    || view.params.is_some()
                    || view.comment.is_some()
                    || !view.cluster_by.is_empty()
                    || !matches!(view.options, ast::CreateTableOptions::None)
                {
                    return Err(unsupported("CREATE VIEW modifiers"));
                }
                if view.or_replace && view.if_not_exists {
                    return Err(Error::Bind(
                        "CREATE VIEW cannot combine OR REPLACE and IF NOT EXISTS".into(),
                    ));
                }
                if view
                    .columns
                    .iter()
                    .any(|column| column.data_type.is_some() || column.options.is_some())
                {
                    return Err(unsupported("typed or optioned view columns"));
                }
                let name = self.resolve_create_target(&view.name)?;
                self.view_stack.push(name.clone());
                let previous_schema = self.view_schema.replace(name.schema.clone());
                self.view_dependencies.borrow_mut().clear();
                let plan = self.query(&view.query);
                self.view_schema = previous_schema;
                self.view_stack.pop();
                let plan = plan?;
                let aliases = view
                    .columns
                    .iter()
                    .map(|column| column.name.value.clone())
                    .collect::<Vec<_>>();
                if aliases.len() > plan.schema.len() {
                    return Err(Error::Bind(format!(
                        "view {} has more aliases ({}) than query columns ({})",
                        name,
                        aliases.len(),
                        plan.schema.len()
                    )));
                }
                let mut output_names = plan
                    .schema
                    .iter()
                    .map(|field| field.name.clone())
                    .collect::<Vec<_>>();
                for (name, alias) in output_names.iter_mut().zip(&aliases) {
                    *name = alias.clone();
                }
                deduplicate_names(&mut output_names);
                let conflict = if view.or_replace {
                    CreateConflictPolicy::Replace
                } else if view.if_not_exists {
                    CreateConflictPolicy::Ignore
                } else {
                    CreateConflictPolicy::Error
                };
                let dependencies = self
                    .view_dependencies
                    .borrow()
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>();
                let query_shape = view_query_shape(&view.query, &name, &dependencies).ok();
                Ok(BoundStatement::CreateView {
                    definition: crate::catalog::ViewDefinition {
                        name,
                        query: view.query.to_string(),
                        aliases,
                        names: output_names,
                        types: plan
                            .schema
                            .iter()
                            .map(|field| field.data_type.clone())
                            .collect(),
                        query_shape,
                        dependencies,
                    },
                    conflict,
                })
            }
            S::Call(function)
                if matches!(function.parameters, ast::FunctionArguments::None)
                    && function.over.is_none()
                    && function.filter.is_none()
                    && function.null_treatment.is_none()
                    && function.within_group.is_empty() =>
            {
                let ast::FunctionArguments::List(arguments) = &function.args else {
                    return Err(unsupported("CALL without table-function arguments"));
                };
                if arguments.duplicate_treatment.is_some() || !arguments.clauses.is_empty() {
                    return Err(unsupported("CALL argument modifiers"));
                }
                Ok(BoundStatement::Query(
                    self.table_function(&function.name, &arguments.args)?,
                ))
            }
            S::CreateType {
                or_replace,
                if_not_exists,
                name,
                representation: Some(ast::UserDefinedTypeRepresentation::Enum { labels }),
            } => {
                let labels = labels
                    .iter()
                    .map(|label| {
                        if label.quote_style != Some('\'') {
                            return Err(Error::Bind("ENUM labels must be string literals".into()));
                        }
                        Ok(label.value.clone())
                    })
                    .collect::<Result<Vec<_>>>()?;
                let conflict = match (*or_replace, *if_not_exists) {
                    (false, false) => CreateConflictPolicy::Error,
                    (false, true) => CreateConflictPolicy::Ignore,
                    (true, false) => CreateConflictPolicy::Replace,
                    (true, true) => {
                        return Err(Error::Bind(
                            "CREATE TYPE cannot combine OR REPLACE and IF NOT EXISTS".into(),
                        ));
                    }
                };
                Ok(BoundStatement::CreateType {
                    definition: TypeDefinition::enumeration(
                        self.resolve_create_type_target(name)?,
                        labels,
                    )?,
                    conflict,
                })
            }
            S::CreateType { .. } => Err(unsupported(
                "only CREATE TYPE name AS ENUM ('label', ...) is supported",
            )),
            S::CreateTable(table) => self.create(table),
            S::AlterTable(table) => self.alter(table),
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
                tables: names
                    .iter()
                    .map(|name| self.resolve_existing_table(name, *if_exists))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .flatten()
                    .map(|resolved| resolved.binding().clone())
                    .collect(),
                if_exists: *if_exists,
            }),
            S::Drop {
                object_type: ast::ObjectType::View,
                names,
                if_exists,
                cascade,
                purge: false,
                temporary: false,
                table: None,
                ..
            } => Ok(BoundStatement::DropView {
                views: names
                    .iter()
                    .map(|name| self.resolve_existing_view(name, *if_exists))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .flatten()
                    .map(|resolved| resolved.binding().clone())
                    .collect(),
                if_exists: *if_exists,
                behavior: if *cascade {
                    DropBehavior::Cascade
                } else {
                    DropBehavior::Restrict
                },
            }),
            S::Drop {
                object_type: ast::ObjectType::Type,
                names,
                if_exists,
                cascade: false,
                purge: false,
                temporary: false,
                table: None,
                ..
            } => {
                if names.len() != 1 {
                    return Err(unsupported("dropping multiple types"));
                }
                Ok(BoundStatement::DropType {
                    types: names
                        .iter()
                        .map(|name| self.resolve_existing_type(name, *if_exists))
                        .collect::<Result<Vec<_>>>()?
                        .into_iter()
                        .flatten()
                        .map(|resolved| resolved.binding().clone())
                        .collect(),
                    if_exists: *if_exists,
                    behavior: DropBehavior::Restrict,
                })
            }
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
                let resolved = self
                    .resolve_existing_table(name, false)?
                    .ok_or_else(|| Error::Internal("required table resolution is absent".into()))?;
                let (table, definition) = resolved.into_parts();
                let fields = schema(&definition);
                let default_values = insert.source.is_none();
                let columns = if default_values {
                    if !insert.columns.is_empty() {
                        return Err(unsupported("DEFAULT VALUES with an INSERT column list"));
                    }
                    Vec::new()
                } else if insert.columns.is_empty() {
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
                let types = columns
                    .iter()
                    .map(|&i| fields[i].data_type.clone())
                    .collect::<Vec<_>>();
                let source = if let Some(source) = &insert.source {
                    self.query_with_value_types(source, Some(&types))?
                } else {
                    LogicalPlan {
                        schema: Vec::new(),
                        node: PlanNode::Values(vec![Vec::new()]),
                    }
                };
                if source.schema.len() != columns.len() {
                    return Err(Error::Bind(
                        "INSERT column count does not match source".into(),
                    ));
                }
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
                let metadata = crate::storage::UpdateMetadata::for_table(
                    self.context
                        .catalog
                        .resolve_table_binding(&table)?
                        .definition(),
                    assignments.iter().map(|(column, _)| *column).collect(),
                )?;
                let predicate = update
                    .selection
                    .as_ref()
                    .map(|e| self.expr(e, &fields, None).and_then(|e| self.boolean(e)))
                    .transpose()?;
                Ok(BoundStatement::Update {
                    table,
                    assignments,
                    metadata,
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
    ) -> Result<(TableBinding, Scope)> {
        if !table.joins.is_empty() {
            return Err(unsupported("joined mutation target"));
        }
        let ast::TableFactor::Table {
            name,
            alias: table_alias,
            args: None,
            version: None,
            with_ordinality: false,
            sample: None,
            partitions,
            ..
        } = &table.relation
        else {
            return Err(unsupported("mutation target"));
        };
        if !partitions.is_empty() {
            return Err(unsupported("partitioned mutation target"));
        }
        let resolved = self
            .resolve_existing_table(name, false)?
            .ok_or_else(|| Error::Internal("required table resolution is absent".into()))?;
        let (binding, definition) = resolved.into_parts();
        let mut plan = LogicalPlan {
            schema: schema(&definition),
            node: PlanNode::Scan(binding.clone()),
        };
        if let Some(table_alias) = table_alias {
            alias(&mut plan, table_alias)?;
        }
        let mut fields: Scope = plan.schema.into();
        if table_alias.is_none() {
            fields.qualify_table(&definition.name);
        }
        Ok((binding, fields))
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
        let name = self.resolve_create_target(&create.name)?;
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
                        definition.default =
                            Some(self.capture_column_default(expr, &definition.data_type)?);
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
                    .map(|f| {
                        Ok(ColumnDefinition::new(
                            &f.name,
                            crate::common::nested::normalize_storage_type(&f.data_type)?,
                        ))
                    })
                    .collect::<Result<_>>()?;
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

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn view_query_shape(
    query: &ast::Query,
    view: &TableName,
    dependencies: &[ViewDependency],
) -> Result<crate::catalog::ViewQueryShape> {
    use crate::catalog::{ViewProjection, ViewQueryShape};
    if query.with.is_some()
        || query.order_by.is_some()
        || query.limit_clause.is_some()
        || query.fetch.is_some()
        || !query.locks.is_empty()
        || query.for_clause.is_some()
        || query.settings.is_some()
        || query.format_clause.is_some()
        || !query.pipe_operators.is_empty()
    {
        return Err(unsupported("native view query shape"));
    }
    let ast::SetExpr::Select(select) = query.body.as_ref() else {
        return Err(unsupported("native view set operation"));
    };
    if select.projection.is_empty()
        || select.from.len() != 1
        || !select.from[0].joins.is_empty()
        || !select.optimizer_hints.is_empty()
        || select.distinct.is_some()
        || select.select_modifiers.is_some()
        || select.top.is_some()
        || select.exclude.is_some()
        || select.into.is_some()
        || !select.lateral_views.is_empty()
        || select.prewhere.is_some()
        || !select.connect_by.is_empty()
        || select.group_by != ast::GroupByExpr::Expressions(Vec::new(), Vec::new())
        || !select.cluster_by.is_empty()
        || !select.distribute_by.is_empty()
        || !select.sort_by.is_empty()
        || select.having.is_some()
        || !select.named_window.is_empty()
        || select.qualify.is_some()
        || select.value_table_mode.is_some()
        || select.flavor != ast::SelectFlavor::Standard
    {
        return Err(unsupported("native view projection/filter shape"));
    }
    let ast::TableFactor::Table {
        name,
        alias: None,
        args: None,
        with_hints,
        version: None,
        with_ordinality: false,
        partitions,
        json_path: None,
        sample: None,
        index_hints,
    } = &select.from[0].relation
    else {
        return Err(unsupported("native view source relation"));
    };
    if !with_hints.is_empty() || !partitions.is_empty() || !index_hints.is_empty() {
        return Err(unsupported("native view source modifiers"));
    }
    let unresolved = super::table_name::UnresolvedTableName::parse(name)?;
    let mut source = TableName::new(
        if unresolved.is_unqualified() {
            &view.schema
        } else {
            name.0[0].as_ident().unwrap().value.as_str()
        },
        unresolved.table(),
    );
    if let Some(resolved) = dependencies.iter().find_map(|dependency| {
        let candidate = match dependency {
            ViewDependency::Table(name) | ViewDependency::View(name) => name,
        };
        (candidate == &source).then(|| candidate.clone())
    }) {
        source = resolved;
    }
    let projection = if select.projection.len() == 1
        && matches!(select.projection[0], ast::SelectItem::Wildcard(_))
    {
        ViewProjection::Star
    } else {
        ViewProjection::Expressions(
            select
                .projection
                .iter()
                .map(|item| {
                    let (expression, alias) = match item {
                        ast::SelectItem::UnnamedExpr(expression) => (expression, None),
                        ast::SelectItem::ExprWithAlias { expr, alias } => {
                            (expr, Some(alias.value.clone()))
                        }
                        _ => return Err(unsupported("native view projection item")),
                    };
                    let mut expression = retained_view_expression(expression)?;
                    expression.alias = alias;
                    Ok(expression)
                })
                .collect::<Result<Vec<_>>>()?,
        )
    };
    Ok(ViewQueryShape {
        source,
        projection,
        filter: select
            .selection
            .as_ref()
            .map(retained_view_expression)
            .transpose()?,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn retained_view_expression(
    expression: &ast::Expr,
) -> Result<crate::catalog::expression::StoredExpression> {
    use crate::catalog::expression::{
        StoredArgument, StoredArgumentStyle, StoredComparison, StoredConjunction, StoredExpression,
        StoredExpressionKind,
    };
    let kind = match expression {
        ast::Expr::Identifier(id) => StoredExpressionKind::ColumnReference(vec![id.value.clone()]),
        ast::Expr::CompoundIdentifier(ids) => {
            StoredExpressionKind::ColumnReference(ids.iter().map(|id| id.value.clone()).collect())
        }
        ast::Expr::Value(value) => match &value.value {
            ast::Value::Number(value, _) => {
                let value = Value::Integer(
                    value
                        .parse::<i128>()
                        .map_err(|_| unsupported("native view numeric literal"))?,
                );
                StoredExpressionKind::Literal {
                    data_type: value.data_type(),
                    value,
                }
            }
            ast::Value::SingleQuotedString(value) => StoredExpressionKind::Literal {
                data_type: DataType::Varchar,
                value: Value::Varchar(value.clone()),
            },
            _ => return Err(unsupported("native view literal")),
        },
        ast::Expr::BinaryOp { left, op, right } => {
            let left = Box::new(retained_view_expression(left)?);
            let right = Box::new(retained_view_expression(right)?);
            match op {
                ast::BinaryOperator::Eq => StoredExpressionKind::Comparison {
                    kind: StoredComparison::Equal,
                    left,
                    right,
                },
                ast::BinaryOperator::NotEq => StoredExpressionKind::Comparison {
                    kind: StoredComparison::NotEqual,
                    left,
                    right,
                },
                ast::BinaryOperator::Lt => StoredExpressionKind::Comparison {
                    kind: StoredComparison::LessThan,
                    left,
                    right,
                },
                ast::BinaryOperator::Gt => StoredExpressionKind::Comparison {
                    kind: StoredComparison::GreaterThan,
                    left,
                    right,
                },
                ast::BinaryOperator::LtEq => StoredExpressionKind::Comparison {
                    kind: StoredComparison::LessThanOrEqual,
                    left,
                    right,
                },
                ast::BinaryOperator::GtEq => StoredExpressionKind::Comparison {
                    kind: StoredComparison::GreaterThanOrEqual,
                    left,
                    right,
                },
                ast::BinaryOperator::And | ast::BinaryOperator::Or => {
                    StoredExpressionKind::Conjunction {
                        kind: if matches!(op, ast::BinaryOperator::And) {
                            StoredConjunction::And
                        } else {
                            StoredConjunction::Or
                        },
                        children: vec![*left, *right],
                    }
                }
                ast::BinaryOperator::Plus
                | ast::BinaryOperator::Minus
                | ast::BinaryOperator::Multiply
                | ast::BinaryOperator::Divide => StoredExpressionKind::Function {
                    name: vec![op.to_string()],
                    arguments: vec![
                        StoredArgument {
                            name: None,
                            expression: *left,
                        },
                        StoredArgument {
                            name: None,
                            expression: *right,
                        },
                    ],
                    is_operator: true,
                    argument_style: StoredArgumentStyle::LegacyAliases,
                },
                _ => return Err(unsupported("native view binary operator")),
            }
        }
        ast::Expr::Nested(inner) => return retained_view_expression(inner),
        _ => return Err(unsupported("native view expression")),
    };
    Ok(StoredExpression {
        alias: None,
        source_span: None,
        kind,
    })
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn schema_name(name: &ast::ObjectName) -> Result<String> {
    if name.0.len() != 1 {
        return Err(unsupported("qualified schema names"));
    }
    name.0[0]
        .as_ident()
        .map(|i| i.value.to_ascii_lowercase())
        .ok_or_else(|| unsupported(name))
}
