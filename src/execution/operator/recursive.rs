//! Fixed-point evaluation with lexical, immutable iteration inputs.
use std::{collections::HashSet, fmt::Debug};

use crate::{
    common::{Error, Result, Row, type_registry::BoundType},
    execution::{
        DataSet, ExecutionContext,
        physical_plan::{DeliveryMode, PhysicalOperator},
        stream::{self, Stream},
    },
    parallel::QueryContext,
    planner::{RecursiveId, Schema},
};

/// A query-local iteration binding. Multiple scans observe the same complete
/// previous generation. A nested recursion retains access to its parent frames.
pub struct RecursiveFrame<'a> {
    id: &'a RecursiveId,
    input: &'a DataSet,
    parent: Option<&'a RecursiveFrame<'a>>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> RecursiveFrame<'a> {
    pub fn new(id: &'a RecursiveId, input: &'a DataSet, parent: Option<&'a Self>) -> Self {
        Self { id, input, parent }
    }
    pub fn lookup(&self, id: &RecursiveId) -> Result<&DataSet> {
        let mut frame = Some(self);
        while let Some(current) = frame {
            if current.id == id {
                return Ok(current.input);
            }
            frame = current.parent;
        }
        Err(Error::Internal(
            "recursive input has no active binding".into(),
        ))
    }
}

#[derive(Clone, Copy)]
pub struct RecursivePlan<'a> {
    pub id: &'a RecursiveId,
    pub seed: &'a dyn PhysicalOperator,
    pub step: &'a dyn PhysicalOperator,
    pub schema: &'a Schema,
    pub all: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Owns fixed-point control, not compilation or transaction publication. Open
/// performs no input work; each cursor has independent generations and keys.
/// Errors/cancellation propagate and dropping a cursor performs no more work.
/// UNION equality uses the selected type adapters, including NULL equality.
pub trait RecursiveAlgorithm: Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn delivery(&self) -> DeliveryMode;
    fn open<'a>(
        &'a self,
        plan: RecursivePlan<'a>,
        context: &'a ExecutionContext<'a>,
    ) -> Result<Stream<'a>>;
}

#[derive(Debug, Default)]
pub struct StreamingRecursion;

#[derive(Debug, Default)]
pub struct MaterializingRecursion;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl RecursiveAlgorithm for StreamingRecursion {
    fn name(&self) -> &'static str {
        "streaming-recursion"
    }
    fn delivery(&self) -> DeliveryMode {
        DeliveryMode::Incremental
    }
    fn open<'a>(
        &'a self,
        plan: RecursivePlan<'a>,
        context: &'a ExecutionContext<'a>,
    ) -> Result<Stream<'a>> {
        open(plan, context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl RecursiveAlgorithm for MaterializingRecursion {
    fn name(&self) -> &'static str {
        "materializing-recursion"
    }
    fn delivery(&self) -> DeliveryMode {
        DeliveryMode::Blocking
    }
    fn open<'a>(
        &'a self,
        plan: RecursivePlan<'a>,
        context: &'a ExecutionContext<'a>,
    ) -> Result<Stream<'a>> {
        context.query.check()?;
        validate(plan)?;
        Ok(stream::deferred(plan.schema, context, move || {
            let mut cursor = open(plan, context)?;
            let mut rows = Vec::new();
            while let Some(chunk) = cursor.next(context.query.batch_size())? {
                context
                    .query
                    .check_rows(rows.len().saturating_add(chunk.len()))?;
                rows.extend(chunk.rows());
            }
            Ok(rows)
        }))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate(plan: RecursivePlan<'_>) -> Result<()> {
    for input in [plan.seed, plan.step] {
        if !input
            .schema()
            .iter()
            .map(|f| &f.data_type)
            .eq(plan.schema.iter().map(|f| &f.data_type))
        {
            return Err(Error::Internal("recursive physical schema mismatch".into()));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn open<'a>(plan: RecursivePlan<'a>, context: &'a ExecutionContext<'a>) -> Result<Stream<'a>> {
    context.query.check()?;
    validate(plan)?;
    let types = plan
        .schema
        .iter()
        .map(|field| context.query.types().bind(&field.data_type))
        .collect::<Result<Vec<_>>>()?;
    let mut seen = HashSet::new();
    let mut seed = Some(stream::open(plan.seed, context)?);
    let mut generation = DataSet {
        schema: plan.schema.clone(),
        rows: Vec::new(),
    };
    let mut position = 0;
    Ok(stream::from_fn(move |max_rows| {
        loop {
            context.query.check()?;
            if let Some(seed_input) = &mut seed {
                if let Some(batch) = seed_input.next(max_rows)? {
                    let rows = retain(batch.rows(), plan.all, &types, &mut seen, context.query)?;
                    context
                        .query
                        .check_rows(generation.rows.len().saturating_add(rows.len()))?;
                    generation.rows.extend(rows);
                } else {
                    seed = None;
                }
            }
            if position < generation.rows.len() {
                let end = position.saturating_add(max_rows).min(generation.rows.len());
                let output = stream::chunk(plan.schema, &generation.rows[position..end])?;
                position = end;
                return Ok(output);
            }
            if seed.is_some() {
                continue;
            }
            if generation.rows.is_empty() {
                return Ok(None);
            }
            let frame = RecursiveFrame::new(plan.id, &generation, context.recursive);
            let nested = ExecutionContext {
                recursive: Some(&frame),
                ..*context
            };
            let next = stream::collect(plan.step, &nested)?;
            generation.rows = retain(next.rows, plan.all, &types, &mut seen, context.query)?;
            position = 0;
        }
    }))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn retain(
    rows: impl IntoIterator<Item = Row>,
    all: bool,
    types: &[BoundType],
    seen: &mut HashSet<Vec<u8>>,
    context: &QueryContext,
) -> Result<Vec<Row>> {
    let mut output = Vec::new();
    for row in rows {
        context.check()?;
        if row.len() != types.len() {
            return Err(Error::Internal("recursive row width mismatch".into()));
        }
        if !all {
            let mut key = Vec::new();
            for (data_type, value) in types.iter().zip(&row) {
                data_type.append_key(value, &mut key, context)?;
            }
            if !seen.insert(key) {
                continue;
            }
            context.check_rows(seen.len())?;
        }
        context.check_rows(output.len().saturating_add(1))?;
        output.push(row);
    }
    Ok(output)
}
