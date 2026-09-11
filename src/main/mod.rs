mod client_context;
mod clock;
mod connection;
mod database;
mod result;
pub mod settings;

pub use clock::{SystemTransactionClock, TransactionClock};
use connection::Session;
pub use connection::{Connection, PreparedStatement};
pub use database::{Database, DatabaseBuilder};
pub use result::{QueryResult, QuerySummary};

use std::{path::Path, sync::Arc, time::Duration};

use crate::{
    common::{
        DataType, Error, Result, RowCollection, Value, cast::CastRegistry, vector::DataChunk,
    },
    execution::{
        CollectingSink, ExecutionContext, Executor, PullExecutor, StreamControl,
        expression_executor::{BatchedEvaluator, ExpressionEvaluator},
        index::{HashIndexFactory, IndexFactory},
        physical_plan::{NativePhysicalPlanner, PhysicalPlanner},
        subquery::{PreparedSubqueries, StreamingSubqueries, SubqueryExecutor},
    },
    function::{FunctionRegistry, operator::OperatorRegistry},
    optimizer::{Optimizer, OptimizerContext, PipelineOptimizer, ValidatedPlan},
    parallel::{InlineScheduler, InterruptHandle, QueryContext, Scheduler},
    parser::{DuckDbParser, Parser, ast},
    planner::{BindContext, Binder, BoundStatement, Field, LogicalPlan, Schema, SqlBinder},
    storage::{
        checkpoint::{Durability, FileCheckpoint, MemoryDurability},
        duckdb::DuckDbFormat,
        filesystem::OpenMode,
        format::JsonSnapshotFormat,
    },
    transaction::{SnapshotTransactions, Transaction, TransactionManager},
};

struct Services {
    transactions: Arc<dyn TransactionManager>,
    parser: Arc<dyn Parser>,
    binder: Arc<dyn Binder>,
    optimizer: Arc<dyn Optimizer>,
    physical_planner: Arc<dyn PhysicalPlanner>,
    executor: Arc<dyn Executor>,
    expressions: Arc<dyn ExpressionEvaluator>,
    stored_expressions: Arc<dyn crate::catalog::expression::StoredExpressionEvaluator>,
    subqueries: Arc<dyn SubqueryExecutor>,
    scheduler: Arc<dyn Scheduler>,
    transaction_clock: Arc<dyn TransactionClock>,
    configuration: Arc<dyn settings::Configuration>,
    functions: FunctionRegistry,
    casts: CastRegistry,
    operators: OperatorRegistry,
    batch_size: usize,
    max_intermediate_rows: usize,
}
