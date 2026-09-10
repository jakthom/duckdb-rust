use super::*;

pub(super) enum Session {
    Idle,
    Active(Box<dyn Transaction>),
    Failed,
}

struct BoundWork {
    statement: BoundStatement,
    transaction: Box<dyn Transaction>,
    explicit: bool,
    context: QueryContext,
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
    pub fn execute(&mut self, sql: &str) -> Result<Vec<QueryResult>> {
        self.execute_params(sql, &[])
    }
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
        let context = context.with_types(self.services.transactions.types());
        let settings = self.configuration.snapshot(&context)?;
        Ok(context.with_settings(settings))
    }
    fn execute_statement(
        &mut self,
        syntax: &crate::parser::Statement,
        parameters: &[Value],
    ) -> Result<QueryResult> {
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
        } = work;
        match statement {
            BoundStatement::Configure(change) => {
                self.configure(change, transaction, explicit, context)
            }
            BoundStatement::Checkpoint => {
                if explicit {
                    self.session = Session::Active(transaction);
                    return Err(Error::Transaction(
                        "CHECKPOINT requires an idle connection".into(),
                    ));
                }
                drop(transaction);
                self.services.transactions.checkpoint(&context)?;
                Ok(QueryResult::command(0))
            }
            BoundStatement::Begin => {
                self.session = Session::Active(transaction);
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
                    self.session = Session::Active(transaction);
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
        let transaction = match previous {
            Session::Active(tx) => tx,
            _ => self.services.transactions.begin()?,
        };
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
            }),
            Err(error) => {
                if explicit {
                    self.session = Session::Active(transaction);
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
                    self.session = Session::Active(transaction);
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

    pub fn execute_prepared_batches(
        &mut self,
        statement: &PreparedStatement,
        parameters: &[Value],
        mut consumer: impl FnMut(&Schema, DataChunk) -> Result<StreamControl>,
    ) -> Result<QuerySummary> {
        let work = self.bind_statement(&statement.syntax, parameters)?;
        if !matches!(work.statement, BoundStatement::Query(_)) {
            if work.explicit {
                self.session = Session::Active(work.transaction);
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
        if let BoundStatement::Configure(change) = statement {
            return self.configure(change, transaction, false, self.context()?);
        }
        self.run(
            BoundWork {
                statement,
                transaction,
                explicit: false,
                context: self.context()?,
            },
            |services, statement, transaction, context| {
                services.execute(statement, transaction, context)
            },
        )
    }
}
