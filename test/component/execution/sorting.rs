use super::*;
use duckdb_rust::{
    common::vector::Vector,
    execution::{
        expression_executor::{BatchedEvaluator, EvaluationContext, ExpressionEvaluator},
        operator::order::{ComparisonSort, RadixSort, SortAlgorithm},
        subquery::{PreparedSubqueries, StreamingSubqueries},
    },
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    planner::{BoundExpr, logical::OrderExpr},
};

#[path = "sorting/effects.rs"]
mod effects;
#[path = "sorting/keys.rs"]
mod keys;
#[path = "../../runner/mod.rs"]
mod runner;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn algorithms() -> [Arc<dyn SortAlgorithm>; 2] {
    [Arc::new(ComparisonSort), Arc::new(RadixSort)]
}

struct Input<'a> {
    batch: &'a DataChunk,
    position: usize,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BatchStream for Input<'_> {
    fn next(&mut self, demand: usize) -> Result<Option<DataChunk>> {
        let count = demand.min(self.batch.len() - self.position);
        if count == 0 {
            return Ok(None);
        }
        let result = self.batch.slice(self.position, count)?;
        self.position += count;
        Ok(Some(result))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sorting_algorithms_share_sql_ordering_across_compositions() -> Result<()> {
    let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test/sql/ordering.test");
    for algorithm in algorithms() {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            for expressions in [
                Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
                Arc::new(BatchedEvaluator),
            ] {
                for executor in executors() {
                    for batch_size in [1, 3, 2048] {
                        let db = DatabaseBuilder::new()
                            .physical_planner(Arc::new(
                                NativePhysicalPlanner::default().with_sorting(algorithm.clone()),
                            ))
                            .optimizer(optimizer.clone())
                            .expressions(expressions.clone())
                            .executor(executor.clone())
                            .batch_size(batch_size)
                            .build()?;
                        assert!(db.adapters().contains(&("sorting", algorithm.name())));
                        assert_eq!(runner::run_file(&db, &corpus)?, 20);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn sort(
    algorithm: &dyn SortAlgorithm,
    query: &QueryContext,
    expressions: &dyn ExpressionEvaluator,
    input: &DataChunk,
    order: &[OrderExpr],
) -> Result<Vec<Row>> {
    let manager = SnapshotTransactions::new(Arc::new(MemoryDurability))?;
    let tx = manager.begin()?;
    let planner = NativePhysicalPlanner::default();
    let context = ExecutionContext {
        transaction: tx.as_ref(),
        expressions,
        query,
        subquery_plans: &PreparedSubqueries::new(&planner),
        subqueries: &StreamingSubqueries,
        outer: None,
        recursive: None,
    };
    let mut stream = Input {
        batch: input,
        position: 0,
    };
    algorithm.sort(&mut stream, order, &context)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn key(column: usize, data_type: &DataType, descending: bool, nulls_first: bool) -> OrderExpr {
    OrderExpr {
        expression: BoundExpr::column(column, data_type.clone()),
        descending,
        nulls_first,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sorting_preserves_stable_lexicographic_order_for_widths_nulls_and_encodings() -> Result<()> {
    use std::cmp::Ordering as Cmp;
    for data_type in [
        DataType::TinyInt,
        DataType::SmallInt,
        DataType::Integer,
        DataType::BigInt,
        DataType::HugeInt,
    ] {
        let width = data_type.integer_bits().unwrap();
        let minimum = if width == 128 {
            i128::MIN
        } else {
            -(1i128 << (width - 1))
        };
        let maximum = if width == 128 {
            i128::MAX
        } else {
            (1i128 << (width - 1)) - 1
        };
        let values = [
            None,
            Some(minimum),
            Some(maximum),
            Some(-1),
            Some(0),
            Some(1),
            Some(minimum),
            Some(maximum),
            None,
            Some(2),
            Some(2),
            Some(2),
            Some(42),
        ];
        let flat = Vector::flat(
            data_type.clone(),
            values
                .map(|v| v.map_or(Value::Null, Value::Integer))
                .to_vec(),
        )?;
        let dictionary =
            Arc::new(flat.clone()).select(vec![2, 1, 2, 0, 8, 6, 5, 4, 3, 11, 10, 9, 12])?;
        for column in [
            flat,
            dictionary,
            Vector::constant(data_type.clone(), Value::Integer(-3), values.len())?,
            Vector::constant(data_type.clone(), Value::Null, values.len())?,
        ] {
            let second = Vector::flat(
                data_type.clone(),
                (0..values.len())
                    .map(|i| {
                        if i % 5 == 0 {
                            Value::Null
                        } else {
                            Value::Integer((i % 3) as i128 - 1)
                        }
                    })
                    .collect(),
            )?;
            let ids = Vector::flat(
                DataType::BigInt,
                (0..values.len())
                    .map(|i| Value::Integer(i as i128))
                    .collect(),
            )?;
            let batch = DataChunk::new(vec![column, second, ids], values.len())?;
            for count in [0, 1, values.len()] {
                let batch = batch.slice(0, count)?;
                for flags in 0..16 {
                    let order = [
                        key(0, &data_type, flags & 1 != 0, flags & 2 != 0),
                        key(1, &data_type, flags & 4 != 0, flags & 8 != 0),
                    ];
                    let mut expected = batch.rows().collect::<Vec<_>>();
                    expected.sort_by(|a, b| {
                        for (i, key) in order.iter().enumerate() {
                            let cmp = match (a[i].is_null(), b[i].is_null()) {
                                (true, true) => Cmp::Equal,
                                (true, false) => {
                                    if key.nulls_first {
                                        Cmp::Less
                                    } else {
                                        Cmp::Greater
                                    }
                                }
                                (false, true) => {
                                    if key.nulls_first {
                                        Cmp::Greater
                                    } else {
                                        Cmp::Less
                                    }
                                }
                                (false, false) => {
                                    let cmp = a[i].as_i128().unwrap().cmp(&b[i].as_i128().unwrap());
                                    if key.descending { cmp.reverse() } else { cmp }
                                }
                            };
                            if cmp != Cmp::Equal {
                                return cmp;
                            }
                        }
                        Cmp::Equal
                    });
                    for algorithm in algorithms() {
                        let query = QueryContext::new(InterruptHandle::default(), None, 3, 100)?;
                        assert_eq!(
                            sort(
                                algorithm.as_ref(),
                                &query,
                                &BatchedEvaluator,
                                &batch,
                                &order
                            )?,
                            expected,
                            "{} {data_type} flags={flags} count={count}",
                            algorithm.name()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn order_all_preserves_complete_row_order_across_many_input_batches() -> Result<()> {
    for algorithm in algorithms() {
        let database = DatabaseBuilder::new()
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_sorting(algorithm),
            ))
            .build()?;
        let mut connection = database.connect();
        connection.execute("CREATE TABLE t AS SELECT i FROM range(50000) t(i)")?;
        let rows = connection
            .query("SELECT i%64 AS k,i FROM t ORDER BY ALL DESC")?
            .rows;
        let expected: Vec<_> = (0..64)
            .rev()
            .flat_map(|key| {
                let last = (49999 - key) / 64;
                (0..=last)
                    .rev()
                    .map(move |offset| ints(&[key, key + 64 * offset]))
            })
            .collect();
        assert_eq!(rows, expected);
    }
    Ok(())
}

struct ReturnedKey(Value);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ExpressionEvaluator for ReturnedKey {
    fn name(&self) -> &'static str {
        "invalid-sort-keys"
    }
    fn evaluate(&self, _: &BoundExpr, _: &Row, _: &dyn EvaluationContext) -> Result<Value> {
        Ok(self.0.clone())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sorting_validates_keys_even_when_no_comparisons_are_needed() -> Result<()> {
    let batch = DataChunk::from_rows(&[DataType::BigInt], &[ints(&[1])])?;
    for algorithm in algorithms() {
        assert!(
            sort(
                algorithm.as_ref(),
                &QueryContext::background(),
                &ReturnedKey(Value::Varchar("not an integer".into())),
                &batch,
                &[key(0, &DataType::BigInt, false, false)]
            )
            .is_err(),
            "{} accepted invalid singleton key",
            algorithm.name()
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn sorting_respects_resource_limits_cancellation_and_result_ownership() -> Result<()> {
    let input = DataChunk::from_rows(&[DataType::BigInt], &[ints(&[3]), ints(&[1]), ints(&[2])])?;
    for algorithm in algorithms() {
        let order = [key(0, &DataType::BigInt, false, false)];
        let query = QueryContext::new(InterruptHandle::default(), None, 1, 2)?;
        assert!(matches!(
            sort(
                algorithm.as_ref(),
                &query,
                &BatchedEvaluator,
                &input,
                &order
            ),
            Err(Error::Resource(_))
        ));
        let interrupt = InterruptHandle::default();
        let query = QueryContext::new(interrupt.clone(), None, 1, 100)?;
        interrupt.interrupt();
        for input in [&input, &input.slice(0, 0)?] {
            assert!(matches!(
                sort(algorithm.as_ref(), &query, &BatchedEvaluator, input, &order),
                Err(Error::Interrupted)
            ));
        }
        let query = QueryContext::background();
        let retained = sort(
            algorithm.as_ref(),
            &query,
            &BatchedEvaluator,
            &input,
            &order,
        )?;
        let descending = [key(0, &DataType::BigInt, true, false)];
        assert_eq!(
            sort(
                algorithm.as_ref(),
                &query,
                &BatchedEvaluator,
                &input,
                &descending
            )?,
            vec![ints(&[3]), ints(&[2]), ints(&[1])]
        );
        assert_eq!(retained, vec![ints(&[1]), ints(&[2]), ints(&[3])]);
    }
    Ok(())
}
