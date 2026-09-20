//! Registered table sources keep immutable binding separate from per-open state.
use std::{any::Any, fmt::Debug, sync::Arc};

use crate::{
    common::{Result, Value, vector::DataChunk},
    parallel::QueryContext,
    planner::Schema,
};

mod csv;
mod range;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(crate) fn register(registry: &mut super::FunctionRegistry) {
    registry
        .register_table(Arc::new(csv::ReadCsv))
        .expect("unique read_csv table function");
    registry
        .register_table(Arc::new(range::IntegerRange::new(false)))
        .expect("unique range table function");
    registry
        .register_table(Arc::new(range::IntegerRange::new(true)))
        .expect("unique generate_series table function");
}

/// One evaluated table-function argument. Names retain source identity; an
/// adapter, rather than the SQL binder, owns named/default argument semantics.
#[derive(Clone, Debug, PartialEq)]
pub struct TableFunctionArgument {
    pub name: Option<String>,
    pub data_type: crate::common::DataType,
    pub value: Value,
}

/// Capabilities available while an adapter binds immutable source metadata.
pub struct TableFunctionBindContext<'a> {
    pub query: &'a QueryContext,
    /// Selected conversions; adapters may retain bound casts in immutable data.
    pub casts: &'a crate::common::cast::CastRegistry,
    /// The active binder owns type syntax and catalog/search-path resolution.
    pub resolve_type: &'a dyn Fn(&str) -> Result<crate::common::DataType>,
}

/// Type-erased immutable adapter data retained by logical and prepared plans.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub trait TableFunctionBindData: Debug + Send + Sync {
    fn as_any(&self) -> &dyn Any;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<T: Debug + Send + Sync + 'static> TableFunctionBindData for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl dyn TableFunctionBindData + '_ {
    pub fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        self.as_any().downcast_ref()
    }
}

/// Type-erased mutable state owned by exactly one physical source open.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub trait TableFunctionState: Debug + Send {
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// A planner request is advisory until the adapter explicitly accepts it.  The
/// residual stays in the ordinary plan, so a source can never evaluate a
/// predicate twice by accident.
#[derive(Clone, Debug, Default)]
pub struct TableFunctionScanRequest {
    pub projection: Option<Vec<usize>>,
    pub limit: Option<usize>,
    pub predicates: Vec<ScanPredicate>,
}

/// Deliberately small scan predicate language. The binder/physical bridge may
/// create it only for a pure, total primitive column/literal comparison.
#[derive(Clone, Debug)]
pub struct ScanPredicate {
    pub id: usize,
    pub source_column: usize,
    pub comparison: ScanComparison,
    pub constant: Value,
    pub data_type: crate::common::type_registry::BoundType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanComparison {
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableFunctionScanAcceptance {
    pub projection: Option<Vec<usize>>,
    pub limit: Option<usize>,
    pub predicate_ids: Vec<usize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<T: Debug + Send + 'static> TableFunctionState for T {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl dyn TableFunctionState + '_ {
    pub fn downcast_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.as_any_mut().downcast_mut()
    }
}

/// Immutable result of binding and schema discovery.
#[derive(Clone)]
pub struct TableFunctionBind {
    schema: Schema,
    data: Arc<dyn TableFunctionBindData>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Debug for TableFunctionBind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TableFunctionBind")
            .field("schema", &self.schema)
            .field("data", &self.data)
            .finish()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TableFunctionBind {
    pub fn new(schema: Schema, data: impl TableFunctionBindData + 'static) -> Self {
        Self {
            schema,
            data: Arc::new(data),
        }
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn data(&self) -> &dyn TableFunctionBindData {
        self.data.as_ref()
    }
}

/// Complete table-source lifecycle. Cleanup consumes state, which prevents a
/// successful cleanup from being followed by another scan or cleanup callback.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub trait TableFunction: Send + Sync {
    fn name(&self) -> &str;

    fn bind(
        &self,
        arguments: &[TableFunctionArgument],
        context: &TableFunctionBindContext<'_>,
    ) -> Result<TableFunctionBind>;

    fn init(
        &self,
        bind: &TableFunctionBind,
        context: &QueryContext,
    ) -> Result<Box<dyn TableFunctionState>>;

    /// Negotiate only capabilities the adapter can prove safe. Predicates are
    /// deliberately absent: they remain residual unless a future typed,
    /// effect-aware contract can preserve SQL error and volatility semantics.
    fn negotiate_scan(
        &self,
        _bind: &TableFunctionBind,
        _request: &TableFunctionScanRequest,
    ) -> TableFunctionScanAcceptance {
        TableFunctionScanAcceptance::default()
    }

    fn scan(
        &self,
        bind: &TableFunctionBind,
        state: &mut dyn TableFunctionState,
        max_rows: usize,
        context: &QueryContext,
    ) -> Result<Option<DataChunk>>;

    fn scan_with_request(
        &self,
        bind: &TableFunctionBind,
        state: &mut dyn TableFunctionState,
        request: &TableFunctionScanAcceptance,
        max_rows: usize,
        context: &QueryContext,
    ) -> Result<Option<DataChunk>> {
        let _ = request;
        self.scan(bind, state, max_rows, context)
    }

    fn cleanup(
        &self,
        _bind: &TableFunctionBind,
        _state: Box<dyn TableFunctionState>,
        _context: &QueryContext,
    ) -> Result<()> {
        Ok(())
    }
}

/// A resolved adapter and its immutable bind result, safe to clone into cached
/// plans. Mutable source state is deliberately absent.
#[derive(Clone)]
pub struct BoundTableFunction {
    function: Arc<dyn TableFunction>,
    bind: TableFunctionBind,
    schema: Schema,
    request: TableFunctionScanAcceptance,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Debug for BoundTableFunction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BoundTableFunction")
            .field("name", &self.function.name())
            .field("bind", &self.bind)
            .finish()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BoundTableFunction {
    pub fn new(function: Arc<dyn TableFunction>, bind: TableFunctionBind) -> Self {
        let schema = bind.schema().clone();
        Self {
            function,
            bind,
            schema,
            request: TableFunctionScanAcceptance::default(),
        }
    }

    pub fn name(&self) -> &str {
        self.function.name()
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn function(&self) -> &Arc<dyn TableFunction> {
        &self.function
    }

    pub fn bind(&self) -> &TableFunctionBind {
        &self.bind
    }

    pub fn request(&self) -> &TableFunctionScanAcceptance {
        &self.request
    }

    pub fn with_scan_request(&self, request: TableFunctionScanRequest) -> Self {
        let acceptance = self.function.negotiate_scan(&self.bind, &request);
        let schema = acceptance.projection.as_ref().map_or_else(
            || self.bind.schema().clone(),
            |columns| {
                columns
                    .iter()
                    .map(|&column| self.bind.schema()[column].clone())
                    .collect()
            },
        );
        Self {
            function: self.function.clone(),
            bind: self.bind.clone(),
            schema,
            request: acceptance,
        }
    }
}
