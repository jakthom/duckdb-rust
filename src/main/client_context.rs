use super::*;

const DUCKDB_STANDARD_VECTOR_SIZE: usize = 2048;

#[derive(Debug)]
struct InsertDefaults {
    input: Arc<dyn crate::execution::physical_plan::PhysicalOperator>,
    schema: Schema,
    columns: Vec<crate::catalog::ColumnDefinition>,
    source_ordinals: Vec<Option<usize>>,
}

impl InsertDefaults {
    fn new(
        input: Arc<dyn crate::execution::physical_plan::PhysicalOperator>,
        definition: &crate::catalog::TableDefinition,
        source_columns: &[usize],
    ) -> Result<Self> {
        if input.schema().len() != source_columns.len() {
            return Err(Error::Internal(
                "INSERT source width differs from its column mapping".into(),
            ));
        }
        let mut source_ordinals = vec![None; definition.columns.len()];
        for (source, &target) in source_columns.iter().enumerate() {
            let ordinal = source_ordinals
                .get_mut(target)
                .ok_or_else(|| Error::Internal("INSERT target column is out of range".into()))?;
            if ordinal.replace(source).is_some() {
                return Err(Error::Internal("duplicate INSERT target column".into()));
            }
        }
        Ok(Self {
            input,
            schema: definition
                .columns
                .iter()
                .map(|column| Field::new(column.name.clone(), column.data_type.clone()))
                .collect(),
            columns: definition.columns.clone(),
            source_ordinals,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl crate::execution::physical_plan::PhysicalOperator for InsertDefaults {
    fn schema(&self) -> &Schema {
        &self.schema
    }
    fn delivery(&self) -> crate::execution::physical_plan::DeliveryMode {
        self.input.delivery()
    }
    fn open<'a>(
        &'a self,
        context: &'a ExecutionContext<'a>,
    ) -> Result<crate::execution::stream::Stream<'a>> {
        let mut input = crate::execution::stream::open(self.input.as_ref(), context)?;
        let mut ready = Vec::new();
        let mut ready_offset = 0;
        let mut source_done = false;
        let mut source_rows = 0usize;
        Ok(crate::execution::stream::from_fn(move |max_rows| {
            if ready_offset == ready.len() {
                ready.clear();
                ready_offset = 0;
                // Defaults are a projection above the INSERT source in
                // DuckDB: finish one standard source vector, then evaluate
                // each omitted column over that vector.
                let mut source_vector = Vec::new();
                while source_vector.len() < DUCKDB_STANDARD_VECTOR_SIZE && !source_done {
                    let remaining = DUCKDB_STANDARD_VECTOR_SIZE - source_vector.len();
                    let remaining_limit = context
                        .query
                        .max_intermediate_rows()
                        .saturating_sub(source_rows)
                        .saturating_add(1);
                    let demand = remaining.min(remaining_limit);
                    let Some(batch) = input.next(demand)? else {
                        source_done = true;
                        break;
                    };
                    source_rows = source_rows
                        .checked_add(batch.len())
                        .ok_or_else(|| Error::Resource("result row count overflow".into()))?;
                    context.query.check_rows(source_rows)?;
                    source_vector.extend(batch.rows());
                }
                if source_vector.is_empty() {
                    return Ok(None);
                }

                ready.reserve(source_vector.len());
                for source in source_vector {
                    context.query.check()?;
                    let mut row = vec![Value::Null; self.columns.len()];
                    for (target, source_ordinal) in self.source_ordinals.iter().enumerate() {
                        if let Some(source_ordinal) = source_ordinal {
                            row[target] = source[*source_ordinal].clone();
                        }
                    }
                    ready.push(row);
                }
                for (ordinal, column) in self.columns.iter().enumerate() {
                    if self.source_ordinals[ordinal].is_some() {
                        continue;
                    }
                    for row in &mut ready {
                        context.query.check()?;
                        row[ordinal] =
                            column
                                .default
                                .as_ref()
                                .map_or(Ok(Value::Null), |expression| {
                                    context.query.stored_expressions()?.evaluate(
                                        expression,
                                        &column.data_type,
                                        context.transaction.catalog(),
                                        context.query,
                                    )
                                })?;
                    }
                }
            }

            let end = ready.len().min(ready_offset.saturating_add(max_rows));
            let chunk = crate::execution::stream::chunk(&self.schema, &ready[ready_offset..end])?;
            ready_offset = end;
            Ok(chunk)
        }))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
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
            recursive: None,
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
            rows: RowCollection::new(plan.schema().len()),
            query,
        };
        let subquery_plans = PreparedSubqueries::new(self.physical_planner.as_ref());
        self.executor.execute(
            plan.as_ref(),
            &self.execution_context(transaction, query, &subquery_plans),
            &mut sink,
        )?;
        Ok(QueryResult {
            columns: plan.schema().clone(),
            rows: sink.rows,
            affected_rows: 0,
        })
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
            BoundStatement::Configure(_) => Err(Error::Internal(
                "configuration requires the session runtime".into(),
            )),
            BoundStatement::Noop => Ok(QueryResult::command(0)),
            BoundStatement::AlterTable { table, alteration } => {
                transaction
                    .catalog_mut()?
                    .alter_table(&table, &alteration, query)?;
                Ok(QueryResult::command(0))
            }
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
                    .map(|plan| {
                        self.query(plan, transaction, query)
                            .map(|r| r.rows.into_rows())
                    })
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
                let source = self.optimize(source, transaction, query)?;
                let source = self.physical_planner.plan(&source)?;
                let plan = InsertDefaults::new(source, &definition, &columns)?;
                // Defaults stay inside the physical pipeline so pull and eager
                // executors observe the same effect order. Storage still sees
                // rows only after the complete statement succeeds.
                let mut sink = CollectingSink {
                    rows: RowCollection::new(plan.schema.len()),
                    query,
                };
                let subquery_plans = PreparedSubqueries::new(self.physical_planner.as_ref());
                self.executor.execute(
                    &plan,
                    &self.execution_context(transaction, query, &subquery_plans),
                    &mut sink,
                )?;
                Ok(QueryResult::command(transaction.storage_mut()?.insert(
                    &table,
                    sink.rows.into_rows(),
                    query,
                )?))
            }
            BoundStatement::Update {
                table,
                assignments,
                metadata,
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
                    transaction
                        .storage_mut()?
                        .update(&table, &metadata, updates, query)?,
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
                    rows: RowCollection::from_rows(1, vec![vec![Value::Varchar(text)]])?,
                    affected_rows: 0,
                })
            }
            _ => Err(Error::Transaction(
                "transaction control belongs to a connection".into(),
            )),
        }
    }
}
