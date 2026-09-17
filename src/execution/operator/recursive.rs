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
    // UNION ALL never constructs equality keys.  In particular, correlated
    // scalar recursions open a fresh fixed point for every outer row, so avoid
    // binding the otherwise unused adapters on each open.
    let types = (!plan.all)
        .then(|| {
            plan.schema
                .iter()
                .map(|field| context.query.types().bind(&field.data_type))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    let mut seen = HashSet::new();
    let mut seed = Some(stream::open(plan.seed, context)?);
    let mut generation = DataSet {
        schema: plan.schema.clone(),
        rows: Vec::new(),
        // The seed and each ordinary recursive step commonly produce one
        // vector batch; avoid allocating their batch list on every correlated
        // fixed point while still allowing larger generations to grow.
        chunks: plan.all.then(|| Vec::with_capacity(1)),
    };
    // UNION ALL generations retain their batches instead of materializing
    // rows.  Keep their cardinality beside the batches: repeatedly summing
    // every batch made small correlated fixed points quadratic in generations.
    let mut generation_len = 0;
    let mut position = 0;
    Ok(stream::from_fn(move |max_rows| {
        loop {
            context.query.check()?;
            if let Some(seed_input) = &mut seed {
                if let Some(batch) = seed_input.next(max_rows)? {
                    if plan.all {
                        append_chunk(&mut generation, batch, &mut generation_len, context.query)?;
                    } else {
                        retain(
                            batch.rows(),
                            plan.schema.len(),
                            types.as_deref().expect("UNION requires key types"),
                            &mut seen,
                            &mut generation.rows,
                            context.query,
                        )?;
                        generation_len = generation.rows.len();
                        context.query.check_rows(generation.rows.len())?;
                    }
                } else {
                    seed = None;
                }
            }
            if position < generation_len {
                let output = generation.next_batch(&mut position, max_rows)?;
                return Ok(output);
            }
            if seed.is_some() {
                continue;
            }
            if generation_len == 0 {
                return Ok(None);
            }
            let frame = RecursiveFrame::new(plan.id, &generation, context.recursive);
            let nested = ExecutionContext {
                recursive: Some(&frame),
                ..*context
            };
            // The step no longer borrows this generation after collection.
            if plan.all {
                (generation, generation_len) = collect_chunks(plan.step, &nested)?;
            } else {
                let next = stream::collect(plan.step, &nested)?;
                generation.rows.clear();
                retain(
                    next.rows,
                    plan.schema.len(),
                    types.as_deref().expect("UNION requires key types"),
                    &mut seen,
                    &mut generation.rows,
                    context.query,
                )?;
                generation_len = generation.rows.len();
            }
            position = 0;
        }
    }))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn append_chunk(
    data: &mut DataSet,
    chunk: crate::common::vector::DataChunk,
    len: &mut usize,
    context: &QueryContext,
) -> Result<()> {
    context.check_rows(len.saturating_add(chunk.len()))?;
    *len = len.saturating_add(chunk.len());
    data.chunks
        .as_mut()
        .expect("UNION ALL dataset retains chunks")
        .push(chunk);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Keep each UNION ALL step in its produced vector batches. This is the same
/// raw-generation limit that `stream::collect` enforces before rows are
/// materialized, without the row-to-vector round trip at every iteration.
fn collect_chunks(
    plan: &dyn PhysicalOperator,
    context: &ExecutionContext<'_>,
) -> Result<(DataSet, usize)> {
    let mut input = stream::open(plan, context)?;
    let mut data = DataSet {
        schema: plan.schema().clone(),
        rows: Vec::new(),
        chunks: Some(Vec::with_capacity(1)),
    };
    let mut len = 0;
    while let Some(chunk) = input.next(context.query.batch_size())? {
        append_chunk(&mut data, chunk, &mut len, context.query)?;
    }
    Ok((data, len))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn retain(
    rows: impl IntoIterator<Item = Row>,
    width: usize,
    types: &[BoundType],
    seen: &mut HashSet<Vec<u8>>,
    output: &mut Vec<Row>,
    context: &QueryContext,
) -> Result<()> {
    for row in rows {
        context.check()?;
        if row.len() != width {
            return Err(Error::Internal("recursive row width mismatch".into()));
        }
        let mut key = Vec::new();
        for (data_type, value) in types.iter().zip(&row) {
            data_type.append_key(value, &mut key, context)?;
        }
        if !seen.insert(key) {
            continue;
        }
        context.check_rows(seen.len())?;
        context.check_rows(output.len().saturating_add(1))?;
        output.push(row);
    }
    Ok(())
}
