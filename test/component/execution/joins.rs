use super::*;
use duckdb_rust::{
    execution::{
        operator::join::{HashJoin, JoinAlgorithm, JoinPlan},
        subquery::{PreparedSubqueries, StreamingSubqueries},
    },
    planner::{BoundExpr, ExprKind, expression::BinaryOp, logical::JoinKind},
};

struct ProbePlan {
    schema: Schema,
    rows: Vec<Row>,
    opens: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
    invalid: bool,
}
impl std::fmt::Debug for ProbePlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("probe-plan")
    }
}
impl ProbePlan {
    fn new(values: &[i128]) -> Self {
        Self {
            schema: vec![Field::new("i", DataType::BigInt)],
            rows: values.iter().map(|value| ints(&[*value])).collect(),
            opens: Arc::new(AtomicUsize::new(0)),
            reads: Arc::new(AtomicUsize::new(0)),
            invalid: false,
        }
    }
}
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

fn equality(query: &QueryContext) -> Result<BoundExpr> {
    Ok(BoundExpr {
        data_type: DataType::Boolean,
        kind: ExprKind::Binary(
            BinaryOp::Equal,
            Box::new(BoundExpr::column(0, DataType::BigInt)),
            Box::new(BoundExpr::column(1, DataType::BigInt)),
            query.types().bind(&DataType::BigInt)?.into(),
        ),
    })
}
fn join<'a>(
    left: &'a ProbePlan,
    right: &'a ProbePlan,
    condition: &'a BoundExpr,
    kind: JoinKind,
) -> JoinPlan<'a> {
    JoinPlan {
        left,
        right,
        kind,
        condition,
        schema: &left.schema,
    }
}

#[test]
fn hash_semi_join_builds_once_streams_demand_and_retains_owned_chunks() -> Result<()> {
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let tx = manager.begin()?;
    let query = QueryContext::new(InterruptHandle::default(), None, 3, 100)?;
    let planner = NativePhysicalPlanner::default();
    let context = ExecutionContext {
        transaction: tx.as_ref(),
        expressions: &ScalarEvaluator,
        query: &query,
        subquery_plans: &PreparedSubqueries::new(&planner),
        subqueries: &StreamingSubqueries,
        outer: None,
    };
    let condition = equality(&query)?;
    let left = ProbePlan::new(&(0..10).collect::<Vec<_>>());
    let mut right = ProbePlan::new(&[0, 2, 2, 4]);
    right.rows.push(vec![Value::Null]);
    let mut first = HashJoin.open(join(&left, &right, &condition, JoinKind::Semi), &context)?;
    assert_eq!(left.reads.load(Ordering::Relaxed), 0);
    assert_eq!(right.opens.load(Ordering::Relaxed), 0);
    let retained = first.next(1)?.unwrap();
    assert_eq!(retained.rows().collect::<Vec<_>>(), vec![ints(&[0])]);
    assert_eq!(left.reads.load(Ordering::Relaxed), 1);
    assert_eq!(right.reads.load(Ordering::Relaxed), 5);
    assert_eq!(
        first.next(2)?.unwrap().rows().collect::<Vec<_>>(),
        vec![ints(&[2])]
    );
    assert_eq!(left.reads.load(Ordering::Relaxed), 3);
    assert_eq!(right.opens.load(Ordering::Relaxed), 1);
    let mut second = HashJoin.open(join(&left, &right, &condition, JoinKind::Anti), &context)?;
    assert_eq!(
        second.next(1)?.unwrap().rows().collect::<Vec<_>>(),
        vec![ints(&[1])]
    );
    assert_eq!(right.opens.load(Ordering::Relaxed), 2);
    assert_eq!(
        first.next(3)?.unwrap().rows().collect::<Vec<_>>(),
        vec![ints(&[4])]
    );
    assert!(first.next(100)?.is_none());
    assert!(first.next(0)?.is_none());
    drop(first);
    drop(second);
    drop(left);
    drop(right);
    assert_eq!(retained.rows().collect::<Vec<_>>(), vec![ints(&[0])]);
    Ok(())
}

#[test]
fn hash_semi_join_checks_build_schema_limits_cancellation_and_empty_outer() -> Result<()> {
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let tx = manager.begin()?;
    let interrupt = InterruptHandle::default();
    let query = QueryContext::new(interrupt.clone(), None, 2, 2)?;
    let planner = NativePhysicalPlanner::default();
    let context = ExecutionContext {
        transaction: tx.as_ref(),
        expressions: &ScalarEvaluator,
        query: &query,
        subquery_plans: &PreparedSubqueries::new(&planner),
        subqueries: &StreamingSubqueries,
        outer: None,
    };
    let condition = equality(&query)?;
    let empty = ProbePlan::new(&[]);
    let left = ProbePlan::new(&[0, 1, 2]);
    let mut invalid = ProbePlan::new(&[0]);
    invalid.invalid = true;
    let mut cursor = HashJoin.open(join(&empty, &invalid, &condition, JoinKind::Semi), &context)?;
    assert!(cursor.next(1)?.is_none());
    assert_eq!(invalid.opens.load(Ordering::Relaxed), 0);
    for kind in [JoinKind::Semi, JoinKind::Anti] {
        let mut cursor = HashJoin.open(join(&left, &invalid, &condition, kind), &context)?;
        assert!(matches!(cursor.next(1), Err(Error::Internal(_))));
        assert!(cursor.next(1)?.is_none());
        let oversized = ProbePlan::new(&[0, 1, 2]);
        let mut cursor = HashJoin.open(join(&left, &oversized, &condition, kind), &context)?;
        assert!(matches!(cursor.next(1), Err(Error::Resource(_))));
        assert!(cursor.next(1)?.is_none());
    }
    let right = ProbePlan::new(&[0, 1]);
    let mut cursor = HashJoin.open(join(&left, &right, &condition, JoinKind::Semi), &context)?;
    assert!(cursor.next(1)?.is_some());
    interrupt.interrupt();
    assert!(matches!(cursor.next(1), Err(Error::Interrupted)));
    interrupt.reset();
    assert!(cursor.next(1)?.is_none());
    Ok(())
}
