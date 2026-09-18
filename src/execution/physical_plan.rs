use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    fmt::Debug,
    sync::Arc,
};

use super::{
    ExecutionContext,
    operator::{
        aggregate::{AggregationAlgorithm, HashAggregation},
        join::{HashJoin, JoinAlgorithm, JoinPlan, NestedLoopJoin},
        order::{RadixSort, SortAlgorithm},
        recursive::{RecursiveAlgorithm, RecursivePlan, StreamingRecursion},
        set::{HashSetOperations, SetAlgorithm, SetPlan},
        window::{PartitionedWindows, WindowAlgorithm, WindowPlan},
    },
    stream::{self, Stream},
    subquery::PreparedExpression,
};
use crate::{
    catalog::TableBinding,
    common::{
        Result, Row,
        type_registry::OrderingRepresentation,
        vector::{DataChunk, Vector},
    },
    planner::{
        BoundExpr, ExprKind, LogicalPlan, PlanNode, Schema,
        aggregation::Aggregation,
        logical::{JoinKind, OrderExpr, SetOperation},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryMode {
    /// Can produce a prefix without consuming the complete input.
    Incremental,
    /// Requires a complete input or build phase before producing rows.
    Blocking,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub trait PhysicalOperator: Debug + Send + Sync {
    fn schema(&self) -> &Schema;
    fn delivery(&self) -> DeliveryMode;
    /// Open independent local execution state without consuming input rows.
    fn open<'a>(&'a self, context: &'a ExecutionContext<'a>) -> Result<Stream<'a>>;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub trait PhysicalPlanner: Send + Sync {
    fn name(&self) -> &'static str;
    fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        vec![("physical_planner", self.name())]
    }
    fn plan(&self, logical: &LogicalPlan) -> Result<Arc<dyn PhysicalOperator>>;
}

pub struct NativePhysicalPlanner {
    joins: Vec<Arc<dyn JoinAlgorithm>>,
    scan_filters: ScanFilterStrategy,
    recursion: Arc<dyn RecursiveAlgorithm>,
    aggregation: Arc<dyn AggregationAlgorithm>,
    sorting: Arc<dyn SortAlgorithm>,
    sets: Arc<dyn SetAlgorithm>,
    windows: Arc<dyn WindowAlgorithm>,
}

/// Both strategies retain scan demand, validation and predicate ordering.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScanFilterStrategy {
    Separate,
    #[default]
    Fused,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScanFilterStrategy {
    fn name(self) -> &'static str {
        match self {
            Self::Separate => "separate-scan-filter",
            Self::Fused => "fused-scan-filter",
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Default for NativePhysicalPlanner {
    fn default() -> Self {
        Self {
            joins: vec![Arc::new(HashJoin), Arc::new(NestedLoopJoin)],
            scan_filters: ScanFilterStrategy::default(),
            recursion: Arc::new(StreamingRecursion),
            aggregation: Arc::new(HashAggregation),
            sorting: Arc::new(RadixSort),
            sets: Arc::new(HashSetOperations),
            windows: Arc::new(PartitionedWindows::default()),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl NativePhysicalPlanner {
    pub fn with_joins(joins: Vec<Arc<dyn JoinAlgorithm>>) -> Self {
        Self {
            joins,
            ..Self::default()
        }
    }
    pub fn with_scan_filters(mut self, strategy: ScanFilterStrategy) -> Self {
        self.scan_filters = strategy;
        self
    }
    pub fn with_recursion(mut self, algorithm: Arc<dyn RecursiveAlgorithm>) -> Self {
        self.recursion = algorithm;
        self
    }
    pub fn with_aggregation(mut self, algorithm: Arc<dyn AggregationAlgorithm>) -> Self {
        self.aggregation = algorithm;
        self
    }
    pub fn with_windows(mut self, algorithm: Arc<dyn WindowAlgorithm>) -> Self {
        self.windows = algorithm;
        self
    }
    pub fn with_sets(mut self, algorithm: Arc<dyn SetAlgorithm>) -> Self {
        self.sets = algorithm;
        self
    }
    pub fn with_sorting(mut self, algorithm: Arc<dyn SortAlgorithm>) -> Self {
        self.sorting = algorithm;
        self
    }
}

#[derive(Debug)]
struct Operator {
    schema: Schema,
    delivery: DeliveryMode,
    node: Node,
}

#[derive(Debug)]
enum Node {
    Values(Vec<Vec<BoundExpr>>),
    Scan(TableBinding),
    RecursiveInput(crate::planner::RecursiveId),
    Recursive {
        id: crate::planner::RecursiveId,
        seed: Arc<dyn PhysicalOperator>,
        step: Arc<dyn PhysicalOperator>,
        all: bool,
        algorithm: Arc<dyn RecursiveAlgorithm>,
    },
    FilteredScan(TableBinding, BoundExpr),
    KeyLookup {
        table: TableBinding,
        columns: Vec<usize>,
        key: Row,
    },
    Range {
        start: i64,
        end: i64,
        step: i64,
    },
    TableFunction(crate::function::table::BoundTableFunction),
    Filter(Arc<dyn PhysicalOperator>, BoundExpr),
    ColumnProjection(Arc<dyn PhysicalOperator>, Vec<usize>),
    Projection(Arc<dyn PhysicalOperator>, Vec<BoundExpr>),
    Join {
        left: Arc<dyn PhysicalOperator>,
        right: Arc<dyn PhysicalOperator>,
        kind: JoinKind,
        condition: BoundExpr,
        algorithm: Arc<dyn JoinAlgorithm>,
    },
    Aggregate {
        input: Arc<dyn PhysicalOperator>,
        aggregation: Aggregation,
        algorithm: Arc<dyn AggregationAlgorithm>,
    },
    Window {
        input: Arc<dyn PhysicalOperator>,
        expressions: Vec<crate::planner::window::WindowExpression>,
        algorithm: Arc<dyn WindowAlgorithm>,
    },
    Sort {
        input: Arc<dyn PhysicalOperator>,
        order: Vec<OrderExpr>,
        algorithm: Arc<dyn SortAlgorithm>,
    },
    Limit(Arc<dyn PhysicalOperator>, Option<usize>, usize),
    Distinct(Arc<dyn PhysicalOperator>),
    DistinctOn {
        input: Arc<dyn PhysicalOperator>,
        targets: Vec<BoundExpr>,
        order: Vec<OrderExpr>,
        algorithm: Arc<dyn SortAlgorithm>,
    },
    SetOperation {
        left: Arc<dyn PhysicalOperator>,
        right: Arc<dyn PhysicalOperator>,
        kind: SetOperation,
        all: bool,
        algorithm: Arc<dyn SetAlgorithm>,
    },
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PhysicalPlanner for NativePhysicalPlanner {
    fn name(&self) -> &'static str {
        "native-physical"
    }
    fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        let mut adapters = vec![
            ("physical_planner", self.name()),
            ("scan_filter", self.scan_filters.name()),
            ("recursion", self.recursion.name()),
            ("aggregation", self.aggregation.name()),
            ("sorting", self.sorting.name()),
            ("set_operations", self.sets.name()),
            ("windows", self.windows.name()),
        ];
        adapters.extend(
            self.joins
                .iter()
                .map(|join| ("join_algorithm", join.name())),
        );
        adapters
    }
    fn plan(&self, logical: &LogicalPlan) -> Result<Arc<dyn PhysicalOperator>> {
        let node = match &logical.node {
            PlanNode::RecursiveInput(id) => Node::RecursiveInput(id.clone()),
            PlanNode::Recursive {
                id,
                seed,
                step,
                all,
            } => Node::Recursive {
                id: id.clone(),
                seed: self.plan(seed)?,
                step: self.plan(step)?,
                all: *all,
                algorithm: self.recursion.clone(),
            },
            PlanNode::Values(rows) => Node::Values(rows.clone()),
            PlanNode::Scan(table) => Node::Scan(table.clone()),
            PlanNode::KeyLookup {
                table,
                columns,
                key,
            } => Node::KeyLookup {
                table: table.clone(),
                columns: columns.clone(),
                key: key.clone(),
            },
            PlanNode::Range { start, end, step } => Node::Range {
                start: *start,
                end: *end,
                step: *step,
            },
            PlanNode::TableFunction(source) => Node::TableFunction(source.clone()),
            PlanNode::Filter { input, predicate } => {
                if self.scan_filters == ScanFilterStrategy::Fused
                    && let PlanNode::Scan(table) = &input.node
                {
                    Node::FilteredScan(table.clone(), predicate.clone())
                } else {
                    Node::Filter(self.plan(input)?, predicate.clone())
                }
            }
            PlanNode::Projection { input, expressions } => {
                let columns = expressions
                    .iter()
                    .map(|expression| match expression.kind {
                        ExprKind::Column(ordinal) => Some(ordinal),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>();
                match columns {
                    Some(columns) => Node::ColumnProjection(self.plan(input)?, columns),
                    None => Node::Projection(self.plan(input)?, expressions.clone()),
                }
            }
            PlanNode::Join {
                left,
                right,
                kind,
                condition,
            } => {
                let algorithm = self
                    .joins
                    .iter()
                    .find(|a| a.supports(condition, left.schema.len()))
                    .cloned()
                    .ok_or_else(|| {
                        crate::Error::Unsupported(
                            "no configured join algorithm accepts this predicate".into(),
                        )
                    })?;
                Node::Join {
                    left: self.plan(left)?,
                    right: self.plan(right)?,
                    kind: *kind,
                    condition: condition.clone(),
                    algorithm,
                }
            }
            PlanNode::Aggregate { input, aggregation } => Node::Aggregate {
                input: self.plan(input)?,
                aggregation: aggregation.clone(),
                algorithm: self.aggregation.clone(),
            },
            PlanNode::Window { input, expressions } => Node::Window {
                input: self.plan(input)?,
                expressions: expressions.clone(),
                algorithm: self.windows.clone(),
            },
            PlanNode::Sort { input, order } => Node::Sort {
                input: self.plan(input)?,
                order: order.clone(),
                algorithm: self.sorting.clone(),
            },
            PlanNode::Limit {
                input,
                limit,
                offset,
            } => Node::Limit(self.plan(input)?, *limit, *offset),
            PlanNode::Distinct(input) => Node::Distinct(self.plan(input)?),
            PlanNode::DistinctOn {
                input,
                targets,
                order,
            } => Node::DistinctOn {
                input: self.plan(input)?,
                targets: targets.clone(),
                order: order.clone(),
                algorithm: self.sorting.clone(),
            },
            PlanNode::SetOperation {
                left,
                right,
                kind,
                all,
            } => Node::SetOperation {
                left: self.plan(left)?,
                right: self.plan(right)?,
                kind: *kind,
                all: *all,
                algorithm: self.sets.clone(),
            },
        };
        Ok(Arc::new(Operator {
            schema: logical.schema.clone(),
            delivery: match &node {
                Node::Recursive { algorithm, .. } => algorithm.delivery(),
                Node::Window {
                    algorithm,
                    expressions,
                    ..
                } => algorithm.delivery(expressions),
                Node::SetOperation {
                    kind: SetOperation::Intersect | SetOperation::Except,
                    ..
                }
                | Node::Join { .. }
                | Node::Aggregate { .. }
                | Node::Sort { .. } => DeliveryMode::Blocking,
                _ => DeliveryMode::Incremental,
            },
            node,
        }))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PhysicalOperator for Operator {
    fn schema(&self) -> &Schema {
        &self.schema
    }
    fn delivery(&self) -> DeliveryMode {
        self.delivery
    }
    fn open<'a>(&'a self, context: &'a ExecutionContext<'a>) -> Result<Stream<'a>> {
        context.query.check()?;
        let schema = &self.schema;
        Ok(match &self.node {
            Node::Recursive {
                id,
                seed,
                step,
                all,
                algorithm,
            } => algorithm.open(
                RecursivePlan {
                    id,
                    seed: seed.as_ref(),
                    step: step.as_ref(),
                    all: *all,
                    schema,
                },
                context,
            )?,
            Node::RecursiveInput(id) => {
                let data = context
                    .recursive
                    .ok_or_else(|| {
                        crate::Error::Internal("recursive input outside execution scope".into())
                    })?
                    .lookup(id)?;
                if !data
                    .schema
                    .iter()
                    .map(|f| &f.data_type)
                    .eq(schema.iter().map(|f| &f.data_type))
                {
                    return Err(crate::Error::Internal(
                        "recursive binding schema mismatch".into(),
                    ));
                }
                let mut position = 0usize;
                stream::from_fn(move |max_rows| data.next_batch(&mut position, max_rows))
            }
            Node::Values(values) => {
                let mut values = values
                    .iter()
                    .map(|row| row.iter().map(PreparedExpression::new).collect::<Vec<_>>())
                    .collect::<Vec<_>>()
                    .into_iter();
                stream::from_fn(move |max_rows| {
                    let rows = values
                        .by_ref()
                        .take(max_rows)
                        .map(|row| {
                            row.iter()
                                .map(|e| e.evaluate(&Vec::new(), context))
                                .collect::<Result<Row>>()
                        })
                        .collect::<Result<Vec<_>>>()?;
                    stream::chunk(schema, &rows)
                })
            }
            Node::Scan(table) => {
                context.transaction.catalog().current_table_binding(table)?;
                let mut scan = context.transaction.storage().open_scan(table.name())?;
                stream::from_fn(move |max_rows| {
                    let Some(batch) =
                        crate::storage::scan::next_batch(scan.as_mut(), max_rows, context.query)?
                    else {
                        return Ok(None);
                    };
                    batch.into_data().map(Some)
                })
            }
            Node::FilteredScan(table, predicate) => {
                context.transaction.catalog().current_table_binding(table)?;
                super::operator::scan::filtered(
                    context.transaction.storage().open_scan(table.name())?,
                    schema,
                    predicate,
                    context,
                )?
            }
            Node::KeyLookup {
                table,
                columns,
                key,
            } => {
                context.transaction.catalog().current_table_binding(table)?;
                stream::deferred(schema, context, move || {
                    context
                        .transaction
                        .storage()
                        .lookup(table.name(), columns, key, context.query)
                        .map(|rows| rows.into_iter().map(|(_, row)| row).collect())
                })
            }
            Node::Range { start, end, step } => {
                if *step == 0 {
                    return Err(crate::Error::Bind("range step is zero".into()));
                }
                let mut current = i128::from(*start);
                stream::from_fn(move |max_rows| {
                    let mut values = Vec::with_capacity(max_rows);
                    while values.len() < max_rows
                        && if *step > 0 {
                            current < i128::from(*end)
                        } else {
                            current > i128::from(*end)
                        }
                    {
                        if values.len() % 1024 == 0 {
                            context.query.check()?;
                        }
                        values.push(current as i64);
                        current += i128::from(*step);
                    }
                    if values.is_empty() {
                        return Ok(None);
                    }
                    let count = values.len();
                    let values =
                        Vector::try_bigints(values.into_iter().map(|value| Ok(Some(value))))?;
                    DataChunk::new(vec![values], count).map(Some)
                })
            }
            Node::TableFunction(source) => {
                let mut scan =
                    crate::storage::table_function::TableFunctionScan::open(source, context.query)?;
                stream::from_fn(move |max_rows| scan.next(max_rows))
            }
            Node::Filter(input, predicate) => {
                let predicate = PreparedExpression::new(predicate);
                let mut input = stream::open(input.as_ref(), context)?;
                stream::from_fn(move |max_rows| {
                    while let Some(batch) = input.next(max_rows)? {
                        if let Some(accepted) = predicate.uniform_selection(&batch, context)? {
                            if accepted {
                                return Ok(Some(batch));
                            }
                            continue;
                        }
                        let selected = predicate.select_batch(&batch, context)?;
                        if !selected.is_empty() {
                            return batch.select(&selected).map(Some);
                        }
                    }
                    Ok(None)
                })
            }
            Node::ColumnProjection(input, columns) => {
                let identity = columns.iter().copied().eq(0..input.schema().len());
                let mut input = stream::open(input.as_ref(), context)?;
                if identity {
                    input
                } else {
                    stream::from_fn(move |max_rows| {
                        input
                            .next(max_rows)?
                            .map(|batch| batch.project(columns))
                            .transpose()
                    })
                }
            }
            Node::Projection(input, expressions) => {
                // One root has no inter-column evaluations to reorder. Pure,
                // total roots can all run by column. A source-defined physical
                // vector callback must likewise retain its batch boundary; an
                // enclosing adapter may still preserve row order internally.
                let batch_safe = expressions.len() == 1
                    || expressions.iter().any(BoundExpr::uses_physical_batch)
                    || expressions.iter().all(BoundExpr::is_pure_and_total);
                // Multiple fallible roots normally preserve row-major first
                // errors. Effect-free roots may first try column execution:
                // successful temporary columns are observable-equivalent; a
                // data error discards them and restores scalar source order.
                let speculative_batch = !batch_safe
                    && expressions.len() > 1
                    && expressions.iter().all(BoundExpr::is_effect_free);
                let expressions = expressions
                    .iter()
                    .map(PreparedExpression::new)
                    .collect::<Vec<_>>();
                let mut input = stream::open(input.as_ref(), context)?;
                stream::from_fn(move |max_rows| {
                    let Some(batch) = input.next(max_rows)? else {
                        return Ok(None);
                    };
                    if batch_safe {
                        let columns = expressions
                            .iter()
                            .map(|expression| expression.evaluate_batch(&batch, context))
                            .collect::<Result<_>>()?;
                        return DataChunk::new(columns, batch.len()).map(Some);
                    }
                    if speculative_batch {
                        let mut columns = Vec::with_capacity(expressions.len());
                        let mut retry_rows = false;
                        for expression in &expressions {
                            match expression.evaluate_batch(&batch, context) {
                                Ok(column) => columns.push(column),
                                Err(
                                    crate::Error::Conversion(_)
                                    | crate::Error::Execution(_)
                                    | crate::Error::OutOfRange(_)
                                    | crate::Error::InvalidInput(_)
                                    | crate::Error::InvalidType(_),
                                ) => {
                                    retry_rows = true;
                                    break;
                                }
                                Err(error) => return Err(error),
                            }
                        }
                        if !retry_rows {
                            return DataChunk::new(columns, batch.len()).map(Some);
                        }
                    }
                    let mut columns = (0..expressions.len())
                        .map(|_| Vec::with_capacity(batch.len()))
                        .collect::<Vec<_>>();
                    for row in batch.rows() {
                        for (expression, values) in expressions.iter().zip(&mut columns) {
                            values
                                .push(expression.evaluate_with_provenance(&row, &batch, context)?);
                        }
                    }
                    let columns = columns
                        .into_iter()
                        .zip(schema)
                        .map(|(values, field)| {
                            super::expression_executor::result_column(
                                field.data_type.clone(),
                                values,
                                context.query,
                            )
                        })
                        .collect::<Result<_>>()?;
                    DataChunk::new(columns, batch.len()).map(Some)
                })
            }
            Node::Limit(input, limit, offset) => {
                let mut remaining = *limit;
                let mut skip = *offset;
                let mut cursor = None;
                stream::from_fn(move |max_rows| {
                    if remaining == Some(0) {
                        return Ok(None);
                    }
                    if cursor.is_none() {
                        cursor = Some(stream::open(input.as_ref(), context)?);
                    }
                    let input = cursor.as_mut().expect("opened limit input");
                    while skip > 0 {
                        let Some(batch) = input.next(skip.min(max_rows))? else {
                            return Ok(None);
                        };
                        skip -= batch.len();
                    }
                    let Some(batch) = input.next(remaining.unwrap_or(max_rows).min(max_rows))?
                    else {
                        return Ok(None);
                    };
                    if let Some(remaining) = &mut remaining {
                        *remaining -= batch.len();
                    }
                    Ok(Some(batch))
                })
            }
            Node::Distinct(input) => {
                let mut input = stream::open(input.as_ref(), context)?;
                let mut seen = HashSet::new();
                stream::from_fn(move |max_rows| {
                    while let Some(batch) = input.next(max_rows)? {
                        if let Some(output) = distinct(batch, &mut seen, context)? {
                            return Ok(Some(output));
                        }
                    }
                    Ok(None)
                })
            }
            Node::DistinctOn {
                input,
                targets,
                order,
                algorithm,
            } => stream::deferred(schema, context, move || {
                let mut input = stream::open(input.as_ref(), context)?;
                if targets.iter().all(BoundExpr::is_pure_and_total)
                    && order.iter().all(|item| item.expression.is_pure_and_total())
                {
                    let rows = grouped_distinct_on(input.as_mut(), targets, order, context)?;
                    if order.is_empty() {
                        return Ok(rows);
                    }
                    let mut survivors = stream::deferred(schema, context, move || Ok(rows));
                    return algorithm.sort(survivors.as_mut(), order, context);
                }
                let rows = if order.is_empty() {
                    let mut rows = Vec::new();
                    while let Some(batch) = input.next(context.query.batch_size())? {
                        context
                            .query
                            .check_rows(rows.len().saturating_add(batch.len()))?;
                        rows.extend(batch.rows());
                    }
                    rows
                } else {
                    algorithm.sort(input.as_mut(), order, context)?
                };
                distinct_on(rows, targets, context)
            }),
            Node::SetOperation {
                left,
                right,
                kind,
                all,
                algorithm,
            } => algorithm.open(
                SetPlan {
                    left: left.as_ref(),
                    right: right.as_ref(),
                    kind: *kind,
                    all: *all,
                    schema,
                },
                context,
            )?,
            Node::Join {
                left,
                right,
                kind,
                condition,
                algorithm,
            } => algorithm.open(
                JoinPlan {
                    left: left.as_ref(),
                    right: right.as_ref(),
                    kind: *kind,
                    condition,
                    schema,
                },
                context,
            )?,
            Node::Aggregate {
                input,
                aggregation,
                algorithm,
            } => stream::deferred(schema, context, move || {
                let mut input = stream::open(input.as_ref(), context)?;
                algorithm.aggregate(input.as_mut(), aggregation, context)
            }),
            Node::Window {
                input,
                expressions,
                algorithm,
            } => algorithm.open(
                WindowPlan {
                    input: input.as_ref(),
                    expressions,
                    schema,
                },
                context,
            )?,
            Node::Sort {
                input,
                order,
                algorithm,
            } => stream::deferred(schema, context, move || {
                let mut input = stream::open(input.as_ref(), context)?;
                algorithm.sort(input.as_mut(), order, context)
            }),
        })
    }
}

/// DuckDB implements DISTINCT ON as hash groups with ordered FIRST aggregates,
/// followed by the ordinary ORDER BY over the surviving groups. Keep only one
/// owned row and order key per group while consuming the input once.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn grouped_distinct_on(
    input: &mut dyn stream::BatchStream,
    targets: &[BoundExpr],
    order: &[OrderExpr],
    context: &ExecutionContext<'_>,
) -> Result<Vec<Row>> {
    let target_types = targets
        .iter()
        .map(|target| context.query.bind_type(&target.data_type))
        .collect::<Result<Vec<_>>>()?;
    let order_types = order
        .iter()
        .map(|item| context.query.bind_type(&item.expression.data_type))
        .collect::<Result<Vec<_>>>()?;
    if let Some(rows) = grouped_distinct_on_signed_columns(
        input,
        targets,
        order,
        &target_types,
        &order_types,
        context,
    )? {
        return Ok(rows);
    }
    let targets = targets
        .iter()
        .map(PreparedExpression::new)
        .collect::<Vec<_>>();
    let orders = order
        .iter()
        .map(|item| PreparedExpression::new(&item.expression))
        .collect::<Vec<_>>();
    let mut groups = HashMap::<Vec<u8>, usize>::new();
    let mut rows = Vec::<(Row, Row)>::new();
    let mut key = Vec::new();
    let mut row = Vec::new();
    while let Some(batch) = input.next(context.query.batch_size())? {
        let target_columns = targets
            .iter()
            .map(|target| target.evaluate_batch(&batch, context))
            .collect::<Result<Vec<_>>>()?;
        let order_columns = orders
            .iter()
            .map(|order| order.evaluate_batch(&batch, context))
            .collect::<Result<Vec<_>>>()?;
        for index in 0..batch.len() {
            if index % 1024 == 0 {
                context.query.check()?;
            }
            key.clear();
            for (column, data_type) in target_columns.iter().zip(&target_types) {
                data_type.append_key(
                    &column.get(index).expect("validated DISTINCT ON target"),
                    &mut key,
                    context.query,
                )?;
            }
            if let Some(&group) = groups.get(key.as_slice()) {
                if compare_order_columns(
                    &order_columns,
                    index,
                    &rows[group].1,
                    order,
                    &order_types,
                    context,
                )? != Ordering::Less
                {
                    continue;
                }
                batch.read_row(index, &mut row)?;
                rows[group].0.clone_from(&row);
                rows[group].1 = order_columns
                    .iter()
                    .map(|column| {
                        column
                            .get(index)
                            .expect("validated DISTINCT ON order")
                            .clone()
                    })
                    .collect();
            } else {
                context.query.check_rows(rows.len().saturating_add(1))?;
                batch.read_row(index, &mut row)?;
                let group = rows.len();
                groups.insert(key.clone(), group);
                rows.push((
                    row.clone(),
                    order_columns
                        .iter()
                        .map(|column| {
                            column
                                .get(index)
                                .expect("validated DISTINCT ON order")
                                .clone()
                        })
                        .collect(),
                ));
            }
        }
    }
    context.query.check()?;
    Ok(rows.into_iter().map(|(row, _)| row).collect())
}

/// Keep row addresses, rather than owned rows and evaluated order keys, for
/// the common single-integer DISTINCT ON shape. Input chunks already own their
/// vector views, so the winning rows are materialized only after grouping.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn grouped_distinct_on_signed_columns(
    input: &mut dyn stream::BatchStream,
    targets: &[BoundExpr],
    order: &[OrderExpr],
    target_types: &[crate::common::type_registry::BoundType],
    order_types: &[crate::common::type_registry::BoundType],
    context: &ExecutionContext<'_>,
) -> Result<Option<Vec<Row>>> {
    let [target] = targets else {
        return Ok(None);
    };
    let [target_type] = target_types else {
        return Ok(None);
    };
    let ExprKind::Column(target_column) = &target.kind else {
        return Ok(None);
    };
    if !target_type.data_type().is_signed_integer() {
        return Ok(None);
    }
    let order_columns = order
        .iter()
        .map(|item| match item.expression.kind {
            ExprKind::Column(column) => Some(column),
            _ => None,
        })
        .collect::<Option<Vec<_>>>();
    let Some(order_columns) = order_columns else {
        return Ok(None);
    };
    if order_types.iter().any(|data_type| {
        data_type.ordering_representation() != OrderingRepresentation::SignedInteger
    }) {
        return Ok(None);
    }
    // A target column is equal within every group and cannot distinguish an
    // ordered FIRST candidate. DuckDB likewise does not need it in the
    // aggregate's per-group ordering state.
    let order_columns = order_columns
        .into_iter()
        .zip(order)
        .filter(|(column, _)| *column != *target_column)
        .collect::<Vec<_>>();

    let mut batches = Vec::<DataChunk>::new();
    let mut row_count = 0usize;
    while let Some(batch) = input.next(context.query.batch_size())? {
        row_count = row_count.checked_add(batch.len()).ok_or_else(|| {
            crate::common::Error::Resource("DISTINCT ON row count overflow".into())
        })?;
        batches.push(batch);
    }

    let mut winners = Vec::<(usize, usize)>::new();
    {
        let mut choose = |group: Option<usize>, current: (usize, usize)| -> Result<Option<usize>> {
            if let Some(group) = group {
                if compare_signed_order_rows(&batches, current, winners[group], &order_columns)
                    == Ordering::Less
                {
                    winners[group] = current;
                }
                return Ok(None);
            }
            context.query.check_rows(winners.len().saturating_add(1))?;
            let group = winners.len();
            winners.push(current);
            Ok(Some(group))
        };

        if let Some(dictionary_len) = shared_dense_signed_dictionary(&batches, *target_column) {
            let mut groups = vec![usize::MAX; dictionary_len];
            for (batch_index, batch) in batches.iter().enumerate() {
                let (_, selected) = batch.columns()[*target_column]
                    .dictionary()
                    .expect("checked shared DISTINCT ON dictionary");
                for (row_index, &source) in selected.iter().enumerate() {
                    if row_index % 1024 == 0 {
                        context.query.check()?;
                    }
                    let group = (groups[source] != usize::MAX).then_some(groups[source]);
                    if let Some(group) = choose(group, (batch_index, row_index))? {
                        groups[source] = group;
                    }
                }
            }
        } else {
            let mut minimum = None::<i128>;
            let mut maximum = None::<i128>;
            for batch in &batches {
                for value in batch.columns()[*target_column].values() {
                    match value {
                        crate::common::Value::Null => {}
                        crate::common::Value::Integer(value) => {
                            minimum = Some(minimum.map_or(value, |minimum| minimum.min(value)));
                            maximum = Some(maximum.map_or(value, |maximum| maximum.max(value)));
                        }
                        _ => unreachable!("validated signed integer DISTINCT ON target"),
                    }
                }
            }
            let dense_width = minimum.zip(maximum).and_then(|(minimum, maximum)| {
                usize::try_from(maximum.abs_diff(minimum).checked_add(1)?).ok()
            });
            let mut groups = if dense_width.is_some_and(|width| width <= row_count / 2) {
                SignedDistinctGroups::Dense {
                    minimum: minimum.expect("nonempty dense DISTINCT ON domain"),
                    groups: vec![usize::MAX; dense_width.expect("checked dense width")],
                    null: usize::MAX,
                }
            } else {
                SignedDistinctGroups::Sparse(HashMap::new())
            };
            for (batch_index, batch) in batches.iter().enumerate() {
                let target_values = &batch.columns()[*target_column];
                for row_index in 0..batch.len() {
                    if row_index % 1024 == 0 {
                        context.query.check()?;
                    }
                    let key = match target_values
                        .get(row_index)
                        .expect("validated DISTINCT ON target")
                    {
                        crate::common::Value::Null => None,
                        crate::common::Value::Integer(value) => Some(value),
                        _ => unreachable!("validated signed integer DISTINCT ON target"),
                    };
                    let group = groups.get(key);
                    if let Some(group) = choose(group, (batch_index, row_index))? {
                        groups.insert(key, group);
                    }
                }
            }
        }
    }
    context.query.check()?;
    let mut rows = Vec::with_capacity(winners.len());
    for (batch_index, row_index) in winners {
        let mut row = Vec::with_capacity(batches[batch_index].columns().len());
        batches[batch_index].read_row(row_index, &mut row)?;
        rows.push(row);
    }
    Ok(Some(rows))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn shared_dense_signed_dictionary(batches: &[DataChunk], column: usize) -> Option<usize> {
    let (parent, _) = batches.first()?.columns().get(column)?.dictionary()?;
    let mut minimum = None::<i128>;
    let mut saw_null = false;
    for (index, value) in parent.values().enumerate() {
        match value {
            crate::common::Value::Integer(value) if !saw_null => {
                let minimum = *minimum.get_or_insert(value);
                if value != minimum.checked_add(index as i128)? {
                    return None;
                }
            }
            crate::common::Value::Null if index + 1 == parent.len() => saw_null = true,
            _ => return None,
        }
    }
    if minimum.is_none() && !saw_null {
        return None;
    }
    batches
        .iter()
        .all(|batch| {
            batch.columns()[column]
                .dictionary()
                .is_some_and(|(other, _)| Arc::ptr_eq(parent, other))
        })
        .then_some(parent.len())
}

enum SignedDistinctGroups {
    Dense {
        minimum: i128,
        groups: Vec<usize>,
        null: usize,
    },
    Sparse(HashMap<Option<i128>, usize>),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SignedDistinctGroups {
    fn get(&self, key: Option<i128>) -> Option<usize> {
        let group = match (self, key) {
            (Self::Dense { null, .. }, None) => *null,
            (
                Self::Dense {
                    minimum, groups, ..
                },
                Some(key),
            ) => groups[(key - minimum) as usize],
            (Self::Sparse(groups), key) => return groups.get(&key).copied(),
        };
        (group != usize::MAX).then_some(group)
    }

    fn insert(&mut self, key: Option<i128>, group: usize) {
        match (self, key) {
            (Self::Dense { null, .. }, None) => *null = group,
            (
                Self::Dense {
                    minimum, groups, ..
                },
                Some(key),
            ) => groups[(key - *minimum) as usize] = group,
            (Self::Sparse(groups), key) => {
                groups.insert(key, group);
            }
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compare_signed_order_rows(
    batches: &[DataChunk],
    left: (usize, usize),
    right: (usize, usize),
    columns: &[(usize, &OrderExpr)],
) -> Ordering {
    let left_batch = &batches[left.0];
    let right_batch = &batches[right.0];
    for &(column, order) in columns {
        let left = left_batch.columns()[column]
            .get(left.1)
            .expect("validated DISTINCT ON order");
        let right = right_batch.columns()[column]
            .get(right.1)
            .expect("validated DISTINCT ON order");
        let comparison = match (left, right) {
            (crate::common::Value::Null, crate::common::Value::Null) => Ordering::Equal,
            (crate::common::Value::Null, _) => {
                if order.nulls_first {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (_, crate::common::Value::Null) => {
                if order.nulls_first {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (crate::common::Value::Integer(left), crate::common::Value::Integer(right)) => {
                let comparison = left.cmp(&right);
                if order.descending {
                    comparison.reverse()
                } else {
                    comparison
                }
            }
            _ => unreachable!("validated signed integer DISTINCT ON order"),
        };
        if comparison != Ordering::Equal {
            return comparison;
        }
    }
    Ordering::Equal
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compare_order_columns(
    left: &[Vector],
    index: usize,
    right: &[crate::common::Value],
    order: &[OrderExpr],
    types: &[crate::common::type_registry::BoundType],
    context: &ExecutionContext<'_>,
) -> Result<Ordering> {
    for (((column, right), order), data_type) in left.iter().zip(right).zip(order).zip(types) {
        let left = column.get(index).expect("validated DISTINCT ON order");
        let comparison = match (left.is_null(), right.is_null()) {
            (true, true) => Ordering::Equal,
            (true, false) => {
                if order.nulls_first {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, true) => {
                if order.nulls_first {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, false) => {
                let comparison = if data_type.ordering_representation()
                    == OrderingRepresentation::SignedInteger
                {
                    match (left, right) {
                        (
                            crate::common::Value::Integer(left),
                            crate::common::Value::Integer(right),
                        ) => left.cmp(right),
                        _ => unreachable!("validated signed integer order key"),
                    }
                } else {
                    data_type.compare(&left, right, context.query)?
                };
                if order.descending {
                    comparison.reverse()
                } else {
                    comparison
                }
            }
        };
        if comparison != Ordering::Equal {
            return Ok(comparison);
        }
    }
    Ok(Ordering::Equal)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn distinct(
    batch: DataChunk,
    seen: &mut HashSet<Vec<u8>>,
    context: &ExecutionContext<'_>,
) -> Result<Option<DataChunk>> {
    let types = batch
        .columns()
        .iter()
        .map(|c| context.query.types().bind(c.data_type()))
        .collect::<Result<Vec<_>>>()?;
    let mut selected = Vec::new();
    for (index, row) in batch.rows().enumerate() {
        context.query.check()?;
        let mut key = Vec::new();
        for (value, data_type) in row.iter().zip(&types) {
            data_type.append_key(value, &mut key, context.query)?;
        }
        if seen.insert(key) {
            context.query.check_rows(seen.len())?;
            selected.push(index);
        }
    }
    if selected.is_empty() {
        Ok(None)
    } else {
        batch.select(&selected).map(Some)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn distinct_on(
    rows: Vec<Row>,
    targets: &[BoundExpr],
    context: &ExecutionContext<'_>,
) -> Result<Vec<Row>> {
    let types = targets
        .iter()
        .map(|target| context.query.types().bind(&target.data_type))
        .collect::<Result<Vec<_>>>()?;
    let targets = targets
        .iter()
        .map(PreparedExpression::new)
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for row in rows {
        context.query.check()?;
        let mut key = Vec::new();
        for (target, data_type) in targets.iter().zip(&types) {
            let value = target.evaluate(&row, context)?;
            data_type.append_key(&value, &mut key, context.query)?;
        }
        if seen.insert(key) {
            context.query.check_rows(output.len().saturating_add(1))?;
            output.push(row);
        }
    }
    Ok(output)
}
