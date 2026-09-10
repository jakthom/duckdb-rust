use std::{collections::HashSet, fmt::Debug, sync::Arc};

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
    catalog::TableName,
    common::{Result, Row, Value, vector::DataChunk},
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
    Scan(TableName),
    RecursiveInput(crate::planner::RecursiveId),
    Recursive {
        id: crate::planner::RecursiveId,
        seed: Arc<dyn PhysicalOperator>,
        step: Arc<dyn PhysicalOperator>,
        all: bool,
        algorithm: Arc<dyn RecursiveAlgorithm>,
    },
    FilteredScan(TableName, BoundExpr),
    KeyLookup {
        table: TableName,
        columns: Vec<usize>,
        key: Row,
    },
    Range {
        start: i64,
        end: i64,
        step: i64,
    },
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
                stream::from_fn(move |max_rows| {
                    let end = position.saturating_add(max_rows).min(data.rows.len());
                    let batch = stream::chunk(schema, &data.rows[position..end])?;
                    position = end;
                    Ok(batch)
                })
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
                let mut scan = context.transaction.storage().open_scan(table)?;
                stream::from_fn(move |max_rows| {
                    let Some(batch) =
                        crate::storage::scan::next_batch(scan.as_mut(), max_rows, context.query)?
                    else {
                        return Ok(None);
                    };
                    batch.into_data().map(Some)
                })
            }
            Node::FilteredScan(table, predicate) => super::operator::scan::filtered(
                context.transaction.storage().open_scan(table)?,
                schema,
                predicate,
                context,
            )?,
            Node::KeyLookup {
                table,
                columns,
                key,
            } => stream::deferred(schema, context, move || {
                context
                    .transaction
                    .storage()
                    .lookup(table, columns, key, context.query)
                    .map(|rows| rows.into_iter().map(|(_, row)| row).collect())
            }),
            Node::Range { start, end, step } => {
                if *step == 0 {
                    return Err(crate::Error::Bind("range step is zero".into()));
                }
                let mut current = i128::from(*start);
                stream::from_fn(move |max_rows| {
                    let mut rows = Vec::new();
                    while rows.len() < max_rows
                        && if *step > 0 {
                            current < i128::from(*end)
                        } else {
                            current > i128::from(*end)
                        }
                    {
                        context.query.check()?;
                        rows.push(vec![Value::Integer(current)]);
                        current += i128::from(*step);
                    }
                    stream::chunk(schema, &rows)
                })
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
                // One root has no inter-column evaluations to reorder. The
                // selected batch evaluator already promises logical row order,
                // first errors and effects for a potentially fallible root.
                let batch_safe =
                    expressions.len() == 1 || expressions.iter().all(BoundExpr::is_pure_and_total);
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
                    let rows = batch
                        .rows()
                        .map(|row| {
                            expressions
                                .iter()
                                .map(|e| e.evaluate(&row, context))
                                .collect::<Result<Row>>()
                        })
                        .collect::<Result<Vec<_>>>()?;
                    stream::chunk(schema, &rows)
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
