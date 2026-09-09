use super::*;

impl Services {
    fn execution_context<'a>(
        &'a self,
        transaction: &'a dyn Transaction,
        query: &'a QueryContext,
        subquery_plans: &'a PreparedSubqueries<'a>,
    ) -> ExecutionContext<'a> {
        ExecutionContext {
            transaction,
            query,
            expressions: self.expressions.as_ref(),
            subquery_plans,
            subqueries: self.subqueries.as_ref(),
            outer: None,
        }
    }
    fn optimize(
        &self,
        plan: LogicalPlan,
        transaction: &dyn Transaction,
        query: &QueryContext,
    ) -> Result<LogicalPlan> {
        let context = OptimizerContext {
            catalog: transaction.catalog(),
            storage: transaction.storage(),
            query,
        };
        self.optimizer
            .optimize(ValidatedPlan::new(plan, &context)?)?
            .into_plan(&context)
    }
    pub(super) fn query(
        &self,
        plan: LogicalPlan,
        transaction: &dyn Transaction,
        query: &QueryContext,
    ) -> Result<QueryResult> {
        let plan = self.optimize(plan, transaction, query)?;
        let plan = self.physical_planner.plan(&plan)?;
        let mut sink = CollectingSink {
            rows: Vec::new(),
            query,
        };
        let subquery_plans = PreparedSubqueries::new(self.physical_planner.as_ref());
        self.executor.execute(
            plan.as_ref(),
            &self.execution_context(transaction, query, &subquery_plans),
            &mut sink,
        )?;
        Ok(DataSet {
            schema: plan.schema().clone(),
            rows: sink.rows,
        }
        .into())
    }

    pub(super) fn query_batches(
        &self,
        plan: LogicalPlan,
        transaction: &dyn Transaction,
        query: &QueryContext,
        consumer: &mut dyn FnMut(&Schema, DataChunk) -> Result<StreamControl>,
    ) -> Result<QuerySummary> {
        let plan = self
            .physical_planner
            .plan(&self.optimize(plan, transaction, query)?)?;
        let subquery_plans = PreparedSubqueries::new(self.physical_planner.as_ref());
        let context = self.execution_context(transaction, query, &subquery_plans);
        let execution = self
            .executor
            .execute(plan.as_ref(), &context, &mut |chunk| {
                consumer(plan.schema(), chunk)
            })?;
        Ok(QuerySummary {
            columns: plan.schema().clone(),
            execution,
        })
    }

    pub(super) fn execute(
        &self,
        statement: BoundStatement,
        transaction: &mut dyn Transaction,
        query: &QueryContext,
    ) -> Result<QueryResult> {
        query.check()?;
        statement.validate(transaction.catalog(), query)?;
        match statement {
            BoundStatement::Query(plan) => self.query(plan, transaction, query),
            BoundStatement::CreateSchema {
                name,
                if_not_exists,
            } => {
                transaction
                    .catalog_mut()?
                    .create_schema(&name, if_not_exists)?;
                Ok(QueryResult::command(0))
            }
            BoundStatement::DropSchema { names, if_exists } => {
                for name in names {
                    transaction.catalog_mut()?.drop_schema(&name, if_exists)?;
                }
                Ok(QueryResult::command(0))
            }
            BoundStatement::CreateTable {
                definition,
                if_not_exists,
                source,
            } => {
                if if_not_exists && transaction.catalog().table(&definition.name).is_ok() {
                    return Ok(QueryResult::command(0));
                }
                let rows = source
                    .map(|plan| self.query(plan, transaction, query).map(|r| r.rows))
                    .transpose()?
                    .unwrap_or_default();
                let name = definition.name.clone();
                transaction
                    .catalog_mut()?
                    .create_table(definition, if_not_exists)?;
                let count = if rows.is_empty() {
                    0
                } else {
                    transaction.storage_mut()?.insert(&name, rows, query)?
                };
                Ok(QueryResult::command(count))
            }
            BoundStatement::DropTable { names, if_exists } => {
                for name in names {
                    transaction.catalog_mut()?.drop_table(&name, if_exists)?;
                }
                Ok(QueryResult::command(0))
            }
            BoundStatement::Insert {
                table,
                columns,
                source,
            } => {
                let definition = transaction.catalog().table(&table)?;
                let input = self.query(source, transaction, query)?;
                let mut rows = Vec::with_capacity(input.rows.len());
                for input in input.rows {
                    query.check()?;
                    let mut row: Row = definition
                        .columns
                        .iter()
                        .map(|c| c.default.clone())
                        .collect();
                    for (&column, value) in columns.iter().zip(input) {
                        row[column] = value;
                    }
                    rows.push(row);
                }
                Ok(QueryResult::command(
                    transaction.storage_mut()?.insert(&table, rows, query)?,
                ))
            }
            BoundStatement::Update {
                table,
                assignments,
                predicate,
            } => {
                let assignments = assignments
                    .iter()
                    .map(|(column, expression)| {
                        (
                            *column,
                            crate::execution::subquery::PreparedExpression::new(expression),
                        )
                    })
                    .collect::<Vec<_>>();
                let predicate = predicate
                    .as_ref()
                    .map(crate::execution::subquery::PreparedExpression::new);
                let mut updates = Vec::new();
                let subquery_plans = PreparedSubqueries::new(self.physical_planner.as_ref());
                let context = self.execution_context(transaction, query, &subquery_plans);
                for (id, row) in transaction.storage().scan(&table, query)? {
                    query.check()?;
                    if let Some(predicate) = &predicate
                        && predicate.evaluate(&row, &context)?.as_bool()? != Some(true)
                    {
                        continue;
                    }
                    let mut updated = row.clone();
                    for (column, expression) in &assignments {
                        updated[*column] = expression.evaluate(&row, &context)?;
                    }
                    updates.push((id, updated));
                }
                Ok(QueryResult::command(
                    transaction.storage_mut()?.update(&table, updates, query)?,
                ))
            }
            BoundStatement::Delete { table, predicate } => {
                let predicate = predicate
                    .as_ref()
                    .map(crate::execution::subquery::PreparedExpression::new);
                let mut ids = Vec::new();
                let subquery_plans = PreparedSubqueries::new(self.physical_planner.as_ref());
                let context = self.execution_context(transaction, query, &subquery_plans);
                for (id, row) in transaction.storage().scan(&table, query)? {
                    query.check()?;
                    if let Some(predicate) = &predicate
                        && predicate.evaluate(&row, &context)?.as_bool()? != Some(true)
                    {
                        continue;
                    }
                    ids.push(id);
                }
                Ok(QueryResult::command(
                    transaction.storage_mut()?.delete(&table, &ids, query)?,
                ))
            }
            BoundStatement::Explain(statement) => {
                let text = if let BoundStatement::Query(plan) = *statement {
                    format!(
                        "{:#?}",
                        self.physical_planner
                            .plan(&self.optimize(plan, transaction, query)?)?
                    )
                } else {
                    format!("{statement:#?}")
                };
                Ok(QueryResult {
                    columns: vec![Field::new("explain_value", DataType::Varchar)],
                    rows: vec![vec![Value::Varchar(text)]],
                    affected_rows: 0,
                })
            }
            _ => Err(Error::Transaction(
                "transaction control belongs to a connection".into(),
            )),
        }
    }
}
