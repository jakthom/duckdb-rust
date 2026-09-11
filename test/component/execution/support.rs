use super::*;

pub(super) struct ProbePlan {
    pub(super) schema: Schema,
    pub(super) rows: Vec<Row>,
    pub(super) opens: Arc<AtomicUsize>,
    pub(super) reads: Arc<AtomicUsize>,
    pub(super) invalid: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for ProbePlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("probe-plan")
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ProbePlan {
    pub(super) fn new(values: &[i128]) -> Self {
        Self {
            schema: vec![Field::new("i", DataType::BigInt)],
            rows: values.iter().map(|value| ints(&[*value])).collect(),
            opens: Arc::new(AtomicUsize::new(0)),
            reads: Arc::new(AtomicUsize::new(0)),
            invalid: false,
        }
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl PhysicalOperator for ProbePlan {
    fn schema(&self) -> &Schema {
        &self.schema
    }
    fn delivery(&self) -> DeliveryMode {
        DeliveryMode::Incremental
    }
    fn open<'a>(&'a self, _: &'a ExecutionContext<'a>) -> Result<Stream<'a>> {
        self.opens.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(ProbeStream {
            plan: self,
            position: 0,
        }))
    }
}
struct ProbeStream<'a> {
    plan: &'a ProbePlan,
    position: usize,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BatchStream for ProbeStream<'_> {
    fn next(&mut self, max_rows: usize) -> Result<Option<DataChunk>> {
        if self.plan.invalid {
            return DataChunk::from_rows(
                &[DataType::Varchar],
                &[vec![Value::Varchar("invalid".into())]],
            )
            .map(Some);
        }
        let end = self
            .position
            .saturating_add(max_rows)
            .min(self.plan.rows.len());
        if self.position == end {
            return Ok(None);
        }
        self.plan
            .reads
            .fetch_add(end - self.position, Ordering::Relaxed);
        let result =
            DataChunk::from_rows(&[DataType::BigInt], &self.plan.rows[self.position..end])?;
        self.position = end;
        Ok(Some(result))
    }
}
