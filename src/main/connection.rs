use super::*;

pub(super) enum Session {
    Idle,
    Active(ActiveTransaction),
    Failed,
}

pub(super) struct ActiveTransaction {
    transaction: Box<dyn Transaction>,
    timestamp_micros: i64,
}

struct BoundWork {
    statement: BoundStatement,
    transaction: Box<dyn Transaction>,
    explicit: bool,
    context: QueryContext,
    timestamp_micros: i64,
}

pub struct PreparedStatement {
    syntax: crate::parser::Statement,
}

/// Connections are exclusively borrowed for execution. Independent connections
/// share the database; a result owns its data and survives its connection.
pub struct Connection {
    pub(super) services: Arc<Services>,
    pub(super) session: Session,
    pub(super) interrupt: InterruptHandle,
    pub(super) timeout: Option<Duration>,
    pub(super) configuration: Box<dyn settings::ConfigurationSession>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Connection {
    /// Checkpoint committed state while other connections retain their owned
    /// snapshots. An explicit or failed transaction on this connection must be
    /// completed first; this operation never commits its uncommitted changes.
    pub fn checkpoint(&mut self) -> Result<()> {
        if !matches!(self.session, Session::Idle) {
            return Err(Error::Transaction(
                "CHECKPOINT requires an idle connection".into(),
            ));
        }
        self.services.transactions.checkpoint(&self.context()?)
    }
    pub fn interrupt_handle(&self) -> InterruptHandle {
        self.interrupt.clone()
    }
    pub fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout;
    }
    /// Resolve a table into the owned definition and strongest runtime binding
    /// available to this connection's visible catalog. Alternative frontends
    /// use the binding when constructing shared logical DDL or data plans.
    pub fn resolve_table(
        &self,
        name: &crate::catalog::TableName,
    ) -> Result<crate::catalog::ResolvedTable> {
        match &self.session {
            Session::Failed => Err(Error::Transaction(
                "transaction is aborted; ROLLBACK is required".into(),
            )),
            Session::Active(active) => active.transaction.catalog().table_entry(name),
            Session::Idle => {
                let transaction = self.services.transactions.begin()?;
                transaction.catalog().table_entry(name)
            }
        }
    }
    pub fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        self.execute_params(sql, &[])
    }
    #[cfg_attr(feature = "dev", duckdb_dev::statement(sql, "request"))]
    pub fn execute_params(&mut self, sql: &str, parameters: &[Value]) -> Result<Vec<QueryResult>> {
        let statements = self.services.parser.parse(sql)?;
        statements
            .iter()
            .map(|s| self.execute_statement(s, parameters))
            .collect()
    }
    pub fn query(&mut self, sql: &str) -> Result<QueryResult> {
        self.execute(sql)?
            .pop()
            .ok_or_else(|| Error::Parse("query has no statements".into()))
    }
    #[cfg_attr(feature = "dev", duckdb_dev::statement(sql, "prepare"))]
    pub fn prepare(&self, sql: &str) -> Result<PreparedStatement> {
        let mut statements = self.services.parser.parse(sql)?;
        if statements.len() != 1 {
            return Err(Error::Parse("prepare requires one statement".into()));
        }
        Ok(PreparedStatement {
            syntax: statements.remove(0),
        })
    }
    pub fn execute_prepared(
        &mut self,
        statement: &PreparedStatement,
        parameters: &[Value],
    ) -> Result<QueryResult> {
        self.execute_statement(&statement.syntax, parameters)
    }
    fn context(&self) -> Result<QueryContext> {
        self.interrupt.reset();
        let context = QueryContext::new(
            self.interrupt.clone(),
            self.timeout,
            self.services.batch_size,
            self.services.max_intermediate_rows,
        )?;
        let context = context
            .with_types(self.services.transactions.types())
            .with_stored_expressions(self.services.stored_expressions.clone());
        let settings = self.configuration.snapshot(&context)?;
        Ok(context.with_settings(settings))
    }
    #[cfg_attr(feature = "dev", duckdb_dev::statement(syntax.dev_sql(), "execute", QueryResult::trace_output))]
    fn execute_statement(
        &mut self,
        syntax: &crate::parser::Statement,
        parameters: &[Value],
    ) -> Result<QueryResult> {
        #[cfg(feature = "dev")]
        duckdb_dev::statement::parameters(&parameters);
        if matches!(self.session, Session::Failed) {
            if matches!(
                syntax,
                crate::parser::Statement::Sql(statement) if matches!(**statement, ast::Statement::Rollback { chain: false, savepoint: None })
            ) {
                self.session = Session::Idle;
                return Ok(QueryResult::command(0));
            }
            return Err(Error::Transaction(
                "transaction is aborted; ROLLBACK is required".into(),
            ));
        }
        let work = self.bind_statement(syntax, parameters)?;
        let BoundWork {
            statement,
            transaction,
            explicit,
            context,
            timestamp_micros,
        } = work;
        match statement {
            BoundStatement::Configure(change) => {
                self.configure(change, transaction, explicit, context, timestamp_micros)
            }
            BoundStatement::Checkpoint => {
                if explicit {
                    self.session = Session::Active(ActiveTransaction {
                        transaction,
                        timestamp_micros,
                    });
                    return Err(Error::Transaction(
                        "CHECKPOINT requires an idle connection".into(),
                    ));
                }
                drop(transaction);
                self.services.transactions.checkpoint(&context)?;
                Ok(QueryResult::command(0))
            }
            BoundStatement::Begin => {
                self.session = Session::Active(ActiveTransaction {
                    transaction,
                    timestamp_micros,
                });
                if explicit {
                    return Err(Error::Transaction("a transaction is already active".into()));
                }
                Ok(QueryResult::command(0))
            }
            BoundStatement::Commit => {
                if !explicit {
                    return Err(Error::Transaction("no active transaction".into()));
                }
                transaction.commit()?;
                Ok(QueryResult::command(0))
            }
            BoundStatement::Rollback => {
                if !explicit {
                    return Err(Error::Transaction("no active transaction".into()));
                }
                Ok(QueryResult::command(0))
            }
            statement => self.run(
                BoundWork {
                    statement,
                    transaction,
                    explicit,
                    context,
                    timestamp_micros,
                },
                |services, statement, transaction, context| {
                    services.execute(statement, transaction, context)
                },
            ),
        }
    }

    fn configure(
        &mut self,
        change: settings::SettingChange,
        transaction: Box<dyn Transaction>,
        explicit: bool,
        context: QueryContext,
        timestamp_micros: i64,
    ) -> Result<QueryResult> {
        // Validate under the selected scheduler, then publish only after it
        // successfully executed the task exactly once. Configuration is not
        // rolled back with SQL data and must not leak through scheduler failure.
        let mut pending = Some(change);
        let mut validated = None;
        let result = self
            .services
            .scheduler
            .run(&context, &mut || {
                let change = pending.take().ok_or_else(|| {
                    Error::Internal("scheduler executed a task more than once".into())
                })?;
                change.validate(context.settings().registry(), &context)?;
                validated = Some(change);
                Ok(())
            })
            .and_then(|()| {
                let change = validated
                    .ok_or_else(|| Error::Internal("scheduler did not execute the task".into()))?;
                self.configuration.apply(&change, &context)
            });
        match result {
            Ok(()) => {
                if explicit {
                    self.session = Session::Active(ActiveTransaction {
                        transaction,
                        timestamp_micros,
                    });
                }
                Ok(QueryResult::command(0))
            }
            Err(error) => {
                if explicit {
                    self.session = Session::Failed;
                }
                Err(error)
            }
        }
    }

    fn bind_statement(
        &mut self,
        syntax: &crate::parser::Statement,
        parameters: &[Value],
    ) -> Result<BoundWork> {
        if matches!(self.session, Session::Failed) {
            return Err(Error::Transaction(
                "transaction is aborted; ROLLBACK is required".into(),
            ));
        }
        let context = self.context()?;
        let previous = std::mem::replace(&mut self.session, Session::Idle);
        let explicit = matches!(previous, Session::Active(_));
        let (transaction, timestamp_micros) = match previous {
            Session::Active(active) => (active.transaction, active.timestamp_micros),
            _ => {
                let transaction = self.services.transactions.begin()?;
                context.check()?;
                let timestamp_micros =
                    self.services.transaction_clock.timestamp_micros(&context)?;
                context.check()?;
                (transaction, timestamp_micros)
            }
        };
        let context = context.with_transaction_timestamp(timestamp_micros);
        match self.services.binder.bind(
            syntax,
            &BindContext {
                casts: &self.services.casts,
                operators: &self.services.operators,
                query: &context,
                catalog: transaction.catalog(),
                functions: &self.services.functions,
                expressions: self.services.expressions.as_ref(),
                parameters,
            },
        ) {
            Ok(statement) => Ok(BoundWork {
                statement,
                transaction,
                explicit,
                context,
                timestamp_micros,
            }),
            Err(error) => {
                if explicit {
                    self.session = Session::Active(ActiveTransaction {
                        transaction,
                        timestamp_micros,
                    });
                }
                Err(error)
            }
        }
    }

    fn run<T>(
        &mut self,
        work: BoundWork,
        task: impl FnOnce(&Services, BoundStatement, &mut dyn Transaction, &QueryContext) -> Result<T>,
    ) -> Result<T> {
        let BoundWork {
            statement,
            mut transaction,
            explicit,
            context,
            timestamp_micros,
        } = work;
        let mut task = Some((task, statement));
        let mut output = None;
        let result = self
            .services
            .scheduler
            .run(&context, &mut || {
                let (task, statement) = task.take().ok_or_else(|| {
                    Error::Internal("scheduler executed a task more than once".into())
                })?;
                output = Some(task(
                    &self.services,
                    statement,
                    transaction.as_mut(),
                    &context,
                )?);
                Ok(())
            })
            .and_then(|()| {
                output.ok_or_else(|| Error::Internal("scheduler did not execute the task".into()))
            });
        match result {
            Ok(output) => {
                if explicit {
                    self.session = Session::Active(ActiveTransaction {
                        transaction,
                        timestamp_micros,
                    });
                } else {
                    transaction.commit()?;
                }
                Ok(output)
            }
            Err(error) => {
                if explicit {
                    self.session = Session::Failed;
                }
                Err(error)
            }
        }
    }

    /// Consume one SELECT in owned batches without collecting its whole result.
    /// Stop is successful early termination. Consumer/evaluation errors abort an
    /// explicit transaction; preceding batches do not imply query success.
    pub fn query_batches(
        &mut self,
        sql: &str,
        consumer: impl FnMut(&Schema, DataChunk) -> Result<StreamControl>,
    ) -> Result<QuerySummary> {
        self.execute_prepared_batches(&self.prepare(sql)?, &[], consumer)
    }

    #[cfg_attr(feature = "dev", duckdb_dev::statement(statement.syntax.dev_sql(), "execute", QuerySummary::trace_output))]
    pub fn execute_prepared_batches(
        &mut self,
        statement: &PreparedStatement,
        parameters: &[Value],
        mut consumer: impl FnMut(&Schema, DataChunk) -> Result<StreamControl>,
    ) -> Result<QuerySummary> {
        #[cfg(feature = "dev")]
        duckdb_dev::statement::parameters(&parameters);
        let work = self.bind_statement(&statement.syntax, parameters)?;
        if !matches!(work.statement, BoundStatement::Query(_)) {
            if work.explicit {
                self.session = Session::Active(ActiveTransaction {
                    transaction: work.transaction,
                    timestamp_micros: work.timestamp_micros,
                });
            }
            return Err(Error::Bind(
                "batch consumption requires a SELECT query".into(),
            ));
        }
        self.run(work, |services, statement, transaction, context| {
            let BoundStatement::Query(plan) = statement else {
                unreachable!("validated batch query")
            };
            services.query_batches(plan, transaction, context, &mut consumer)
        })
    }

    /// Frontends other than SQL may submit the shared logical statement contract.
    pub fn execute_plan(&mut self, statement: BoundStatement) -> Result<QueryResult> {
        if matches!(statement, BoundStatement::Checkpoint) {
            self.checkpoint()?;
            return Ok(QueryResult::command(0));
        }
        if !matches!(self.session, Session::Idle) {
            return Err(Error::Transaction(
                "plan API requires an idle connection".into(),
            ));
        }
        let transaction = self.services.transactions.begin()?;
        let context = self.context()?;
        context.check()?;
        let timestamp_micros = self.services.transaction_clock.timestamp_micros(&context)?;
        context.check()?;
        let context = context.with_transaction_timestamp(timestamp_micros);
        if let BoundStatement::Configure(change) = statement {
            return self.configure(change, transaction, false, context, timestamp_micros);
        }
        self.run(
            BoundWork {
                statement,
                transaction,
                explicit: false,
                context,
                timestamp_micros,
            },
            |services, statement, transaction, context| {
                services.execute(statement, transaction, context)
            },
        )
    }
}
