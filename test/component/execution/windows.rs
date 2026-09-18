use super::*;
use duckdb_rust::{
    common::type_registry::TypeRegistry,
    execution::{
        expression_executor::{BatchedEvaluator, ExpressionEvaluator},
        operator::window::{PartitionedWindows, SortedWindows, WindowAlgorithm},
    },
    function::{
        AggregateFunction, AggregateState,
        window::{WindowFunction, WindowInput, WindowOptions},
    },
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn algorithms() -> Vec<Arc<dyn WindowAlgorithm>> {
    vec![
        Arc::new(PartitionedWindows::default()),
        Arc::new(SortedWindows::default()),
    ]
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn window_input_views_validate_bounds_and_borrow_partition_rows() -> Result<()> {
    use duckdb_rust::{
        common::RowCollection,
        function::window::{WindowBounds, WindowRows},
    };
    let mut rows = RowCollection::from_rows(1, vec![ints(&[7]), vec![Value::Null], ints(&[9])])?;
    assert!(rows.push(&ints(&[1, 2])).is_err());
    assert_eq!(rows.len(), 3);
    rows.push(&ints(&[11]))?;
    let permutation = [3, 0, 1];
    let view = WindowRows::new(&rows, &permutation)?;
    assert_eq!(
        view.iter().collect::<Vec<_>>(),
        [ints(&[11]), ints(&[7]), vec![Value::Null]]
            .iter()
            .map(Vec::as_slice)
            .collect::<Vec<_>>()
    );
    assert!(std::ptr::eq(view.get(0).unwrap(), &rows[3]));
    assert!(view.get(3).is_none());
    assert!(WindowRows::new(&rows, &[4]).is_err());
    assert!(WindowBounds::uniform(0..5, 4).is_err());
    assert!(WindowBounds::rows(std::iter::once(0..2).collect()).is_err());
    let uniform = WindowBounds::uniform(0..4, 4)?;
    let explicit = WindowBounds::rows(vec![0..4; 4])?;
    assert_eq!(
        uniform.iter().collect::<Vec<_>>(),
        explicit.iter().collect::<Vec<_>>()
    );
    assert!(WindowBounds::uniform(0..0, 0)?.is_empty());
    let mut empty_rows = RowCollection::new(0);
    empty_rows.push(&[])?;
    empty_rows.append(&DataChunk::new(vec![], 2)?)?;
    assert_eq!(
        WindowRows::new(&empty_rows, &[2, 0])?
            .iter()
            .collect::<Vec<_>>(),
        vec![&[] as &[Value]; 2]
    );
    Ok(())
}

#[derive(Debug)]
struct InvalidPermutation(bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::execution::operator::order::SortAlgorithm for InvalidPermutation {
    fn name(&self) -> &'static str {
        "invalid-permutation"
    }
    fn sort(
        &self,
        input: &mut dyn BatchStream,
        _: &[duckdb_rust::planner::logical::OrderExpr],
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<Row>> {
        let mut rows = Vec::new();
        while let Some(batch) = input.next(context.query.batch_size())? {
            rows.extend(batch.rows());
        }
        if self.0 {
            rows.pop();
        } else if rows.len() > 1 {
            rows[1] = rows[0].clone();
        }
        Ok(rows)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn window_sort_adapters_cannot_drop_or_duplicate_row_identities() -> Result<()> {
    for cardinality in [false, true] {
        let sort = Arc::new(InvalidPermutation(cardinality));
        for algorithm in [
            Arc::new(PartitionedWindows::new(sort.clone())) as Arc<dyn WindowAlgorithm>,
            Arc::new(SortedWindows::new(sort.clone())),
        ] {
            let mut c = DatabaseBuilder::new()
                .physical_planner(Arc::new(
                    NativePhysicalPlanner::default().with_windows(algorithm),
                ))
                .build()?
                .connect();
            assert!(matches!(
                c.query("SELECT row_number() OVER(ORDER BY i) FROM (VALUES(3),(1),(2))t(i)"),
                Err(Error::Internal(_))
            ));
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn window_effects_keep_distinct_calls_and_qualify_precedes_result_expressions() -> Result<()> {
    for algorithm in algorithms() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut functions = FunctionRegistry::builtins();
        functions.register_scalar(Arc::new(CountCalls(calls.clone())))?;
        let mut c = DatabaseBuilder::new()
            .functions(functions)
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_windows(algorithm),
            ))
            .build()?
            .connect();
        let result = c.query(
            "SELECT sum(count_calls(i)) OVER (),sum(count_calls(i)) OVER () FROM range(4)t(i)",
        )?;
        assert_eq!(calls.swap(0, Ordering::Relaxed), 8);
        assert_eq!(result.rows, vec![ints(&[6, 6]); 4]);
        c.query("SELECT count_calls(i),row_number() OVER () AS rn FROM range(4)t(i) QUALIFY rn=1")?;
        assert_eq!(calls.swap(0, Ordering::Relaxed), 1);
        assert!(matches!(c.query("SELECT count_calls(i) AS observed,row_number() OVER () FROM range(4)t(i) QUALIFY observed=1"),Err(Error::Bind(_))));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        let result=c.query("SELECT CAST(v AS INTEGER) FROM (VALUES(1,'42'),(2,'bad'))t(k,v) QUALIFY row_number() OVER (ORDER BY k)=1")?;
        assert_eq!(result.rows, vec![ints(&[42])]);
        let result = c.query(
            "SELECT i AS x,i+1 AS x,row_number() OVER () AS rn FROM range(3)t(i) QUALIFY rn=1",
        )?;
        assert_eq!(result.rows, vec![ints(&[0, 1, 1])]);
        let result = c.query("SELECT percent_rank() OVER(ORDER BY i),cume_dist() OVER(ORDER BY i) FROM (VALUES(1),(2),(2),(3))t(i) ORDER BY i")?;
        assert!(
            result
                .columns
                .iter()
                .all(|field| field.data_type == DataType::Double)
        );
        assert_eq!(
            result.rows,
            vec![
                vec![Value::Double(0.0), Value::Double(0.25)],
                vec![Value::Double(1.0 / 3.0), Value::Double(0.75)],
                vec![Value::Double(1.0 / 3.0), Value::Double(0.75)],
                vec![Value::Double(1.0), Value::Double(1.0)]
            ]
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn window_adapters_share_frames_peers_names_and_qualify_across_compositions() -> Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    for algorithm in algorithms() {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            for executor in executors() {
                for expressions in [
                    Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
                    Arc::new(BatchedEvaluator),
                ] {
                    for batch_size in [1, 3, 2048] {
                        let db = DatabaseBuilder::new()
                            .physical_planner(Arc::new(
                                NativePhysicalPlanner::default().with_windows(algorithm.clone()),
                            ))
                            .optimizer(optimizer.clone())
                            .expressions(expressions.clone())
                            .executor(executor.clone())
                            .batch_size(batch_size)
                            .build()?;
                        assert!(db.adapters().contains(&("windows", algorithm.name())));
                        for file in [
                            "windows.test",
                            "window-binding.test",
                            "windows-development.test",
                        ] {
                            runner::run_file(&db, &root.join("test/sql").join(file))?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct FrameAggregate(Arc<dyn AggregateFunction>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl AggregateFunction for FrameAggregate {
    fn name(&self) -> &str {
        "frame_sum"
    }
    fn return_type(&self, args: &[DataType], types: &TypeRegistry) -> Result<DataType> {
        self.0.return_type(args, types)
    }
    fn create_state(
        &self,
        args: &[DataType],
        types: &TypeRegistry,
    ) -> Result<Box<dyn AggregateState>> {
        self.0.create_state(args, types)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn aggregate_window_capabilities_match_frame_states_and_independent_sums() -> Result<()> {
    for algorithm in algorithms() {
        let mut functions = FunctionRegistry::builtins();
        functions.register_aggregate(Arc::new(FrameAggregate(
            functions.aggregate("sum").unwrap(),
        )))?;
        let db = DatabaseBuilder::new()
            .functions(functions)
            .physical_planner(Arc::new(
                NativePhysicalPlanner::default().with_windows(algorithm),
            ))
            .batch_size(3)
            .build()?;
        let mut c = db.connect();
        c.execute("CREATE TABLE t(i INTEGER, v BIGINT)")?;
        let values: Vec<_> = (0..31)
            .map(|i| if i % 7 == 0 { None } else { Some(i % 11 - 5) })
            .collect();
        for (i, value) in values.iter().enumerate() {
            c.execute(&format!(
                "INSERT INTO t VALUES ({i},{})",
                value.map(|n| n.to_string()).unwrap_or("NULL".into())
            ))?;
        }
        for preceding in [0, 1, 5, 40] {
            for following in [0, 2, 50] {
                let result = c.query(&format!("SELECT i,sum(v) OVER w,frame_sum(v) OVER w,count(v) OVER w FROM t WINDOW w AS (ORDER BY i ROWS BETWEEN {preceding} PRECEDING AND {following} FOLLOWING) ORDER BY i"))?;
                for (i, row) in result.rows.iter().enumerate() {
                    let frame =
                        &values[i.saturating_sub(preceding)..(i + following + 1).min(values.len())];
                    let count = frame.iter().flatten().count();
                    let sum = if count == 0 {
                        Value::Null
                    } else {
                        Value::Integer(frame.iter().flatten().map(|&v| i128::from(v)).sum())
                    };
                    assert_eq!(
                        row,
                        &vec![
                            Value::Integer(i as i128),
                            sum.clone(),
                            sum,
                            Value::Integer(count as i128)
                        ]
                    );
                }
            }
        }
        let result = c.query("SELECT sum(DISTINCT v) FILTER (WHERE i%2=0) OVER w,frame_sum(DISTINCT v) FILTER (WHERE i%2=0) OVER w FROM t WINDOW w AS (ORDER BY i ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW)")?;
        assert!(result.rows.iter().all(|row| row[0] == row[1]));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowFault {
    None,
    Cardinality,
    PhysicalType,
}

struct PartitionSize {
    fault: WindowFault,
    interrupt: Arc<std::sync::Mutex<Option<InterruptHandle>>>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl std::fmt::Debug for PartitionSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PartitionSize")
            .field("fault", &self.fault)
            .finish()
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl WindowFunction for PartitionSize {
    fn name(&self) -> &str {
        "partition_size"
    }
    fn return_type(
        &self,
        args: &[DataType],
        _: WindowOptions,
        _: &TypeRegistry,
    ) -> Result<DataType> {
        if !args.is_empty() {
            return Err(Error::Bind("partition_size takes no arguments".into()));
        }
        Ok(DataType::BigInt)
    }
    fn evaluate(&self, input: &WindowInput<'_>, query: &QueryContext) -> Result<Vec<Value>> {
        query.check()?;
        if let Some(interrupt) = self.interrupt.lock().unwrap().take() {
            interrupt.interrupt();
        }
        Ok(vec![
            if self.fault == WindowFault::PhysicalType {
                Value::Varchar("invalid".into())
            } else {
                Value::Integer(input.arguments.len() as i128)
            };
            input.arguments.len().saturating_sub(usize::from(
                self.fault == WindowFault::Cardinality
            ))
        ])
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn registered_window_functions_obey_cardinality_resources_and_owned_results() -> Result<()> {
    for algorithm in algorithms() {
        for fault in [
            WindowFault::None,
            WindowFault::Cardinality,
            WindowFault::PhysicalType,
        ] {
            let mut functions = FunctionRegistry::builtins();
            let pending_interrupt = Arc::new(std::sync::Mutex::new(None));
            functions.register_window(Arc::new(PartitionSize {
                fault,
                interrupt: pending_interrupt.clone(),
            }))?;
            let db = DatabaseBuilder::new()
                .functions(functions)
                .physical_planner(Arc::new(
                    NativePhysicalPlanner::default().with_windows(algorithm.clone()),
                ))
                .batch_size(2)
                .max_intermediate_rows(12)
                .build()?;
            let mut c = db.connect();
            let sql = "SELECT i,partition_size() OVER (PARTITION BY i%2) AS n FROM range(5)t(i) ORDER BY i";
            if fault != WindowFault::None {
                assert!(matches!(c.query(sql), Err(Error::Internal(_))));
                continue;
            }
            let expected = c.query(sql)?.rows;
            assert_eq!(
                expected,
                vec![
                    ints(&[0, 3]),
                    ints(&[1, 2]),
                    ints(&[2, 3]),
                    ints(&[3, 2]),
                    ints(&[4, 3])
                ]
            );
            let prepared = c.prepare(sql)?;
            let mut retained = Vec::new();
            c.query_batches(sql, |_, batch| {
                retained.push(batch);
                Ok(StreamControl::Continue)
            })?;
            assert_eq!(c.execute_prepared(&prepared, &[])?.rows, expected);
            assert!(matches!(
                c.query("SELECT row_number() OVER () FROM range(13)"),
                Err(Error::Resource(_))
            ));
            let interrupt = c.interrupt_handle();
            *pending_interrupt.lock().unwrap() = Some(interrupt.clone());
            assert!(matches!(
                c.execute_prepared(&prepared, &[]),
                Err(Error::Interrupted)
            ));
            interrupt.reset();
            drop(c);
            drop(db);
            assert_eq!(
                retained
                    .iter()
                    .flat_map(DataChunk::rows)
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }
    Ok(())
}
