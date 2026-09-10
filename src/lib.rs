//! A database assembled from explicit compilation, execution and persistence contracts.
#[cfg(all(feature = "dev", not(debug_assertions)))]
compile_error!("dev tracing is excluded from production; use cargo dev or --profile dev-trace");
pub mod catalog;
pub mod common;
pub mod execution;
pub mod function;
#[path = "main/mod.rs"]
pub mod main;
pub mod optimizer;
pub mod parallel;
pub mod parser;
pub mod planner;
pub mod storage;
pub mod transaction;

pub use common::{DataType, Date, Error, Result, TemporalValue, Value};
pub use main::{Connection, Database, DatabaseBuilder, QueryResult, QuerySummary};
