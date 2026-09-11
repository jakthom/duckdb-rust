use super::*;
use duckdb_rust::{
    Error,
    common::type_registry::{KeyWriter, PrimitiveTypes, TypeAdapter, TypeRegistry},
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    function::{FunctionEffects, FunctionRegistry, ScalarBindArguments, ScalarFunction},
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
    parallel::InterruptHandle,
};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn text(value: &str) -> Value {
    Value::Varchar(value.into())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn integer_list(values: &[Option<i128>]) -> Result<Value> {
    NestedValue::value(
        NestedType::List(DataType::Integer).data_type(),
        NestedPayload::Sequence(
            values
                .iter()
                .map(|value| value.map_or(Value::Null, Value::Integer))
                .collect(),
        ),
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn aliases_sequence_types_nulls_and_batches_cross_execution_matrix() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut connection = DatabaseBuilder::new()
                .expressions(expressions.clone())
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();

            for (name, expression, expected, data_type) in [
                (
                    "list_sort",
                    "list_sort([3,NULL,1],'ASC','NULLS LAST')",
                    "[1, 3, NULL]",
                    "INTEGER[]",
                ),
                (
                    "array_sort",
                    "array_sort([3,NULL,1]::INTEGER[3],'DESC','NULLS FIRST')",
                    "[NULL, 3, 1]",
                    "INTEGER[]",
                ),
                (
                    "list_grade_up",
                    "list_grade_up([3,NULL,1],'ASC','NULLS LAST')",
                    "[3, 1, 2]",
                    "BIGINT[]",
                ),
                (
                    "array_grade_up",
                    "array_grade_up([3,NULL,1]::INTEGER[3],'DESC','NULLS FIRST')",
                    "[2, 1, 3]",
                    "BIGINT[]",
                ),
                (
                    "grade_up",
                    "grade_up([2,1,2,1],'ASC','NULLS LAST')",
                    "[2, 4, 1, 3]",
                    "BIGINT[]",
                ),
                (
                    "list_reverse_sort",
                    "list_reverse_sort([3,NULL,1],'NULLS LAST')",
                    "[3, 1, NULL]",
                    "INTEGER[]",
                ),
                (
                    "array_reverse_sort",
                    "array_reverse_sort([3,NULL,1]::INTEGER[3],'NULLS FIRST')",
                    "[NULL, 3, 1]",
                    "INTEGER[]",
                ),
            ] {
                assert_eq!(
                    connection
                        .query(&format!(
                            "SELECT ({expression})::VARCHAR,typeof({expression})"
                        ))?
                        .rows,
                    vec![vec![text(expected), text(data_type)]],
                    "{name}",
                );
            }

            assert_eq!(
                connection
                    .query(
                        "SELECT
                            list_sort([])::VARCHAR,
                            typeof(list_sort([])),
                            list_grade_up([])::VARCHAR,
                            typeof(list_grade_up([])),
                            list_sort(NULL::INTEGER[]),
                            typeof(list_sort(NULL::INTEGER[])),
                            grade_up(NULL),
                            typeof(grade_up(NULL)),
                            typeof(list_sort(v)),
                            typeof(list_grade_up(v))
                         FROM (VALUES (NULL::INTEGER[])) t(v)",
                    )?
                    .rows,
                vec![vec![
                    text("[]"),
                    text("\"NULL\"[]"),
                    text("[]"),
                    text("BIGINT[]"),
                    Value::Null,
                    text("\"NULL\""),
                    Value::Null,
                    text("\"NULL\""),
                    text("INTEGER[]"),
                    text("BIGINT[]"),
                ]],
            );

            let rows = connection
                .query(
                    "SELECT
                        i,
                        list_sort([i%3,NULL,2-(i%3)],'ASC','NULLS LAST')::VARCHAR,
                        list_grade_up([i%3,NULL,2-(i%3)],'ASC','NULLS LAST')::VARCHAR
                     FROM range(7) t(i)",
                )?
                .rows;
            assert_eq!(
                rows,
                vec![
                    vec![Value::Integer(0), text("[0, 2, NULL]"), text("[1, 3, 2]")],
                    vec![Value::Integer(1), text("[1, 1, NULL]"), text("[1, 3, 2]")],
                    vec![Value::Integer(2), text("[0, 2, NULL]"), text("[3, 1, 2]")],
                    vec![Value::Integer(3), text("[0, 2, NULL]"), text("[1, 3, 2]")],
                    vec![Value::Integer(4), text("[1, 1, NULL]"), text("[1, 3, 2]")],
                    vec![Value::Integer(5), text("[0, 2, NULL]"), text("[3, 1, 2]")],
                    vec![Value::Integer(6), text("[0, 2, NULL]"), text("[1, 3, 2]")],
                ],
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn defaults_options_and_prepared_parameters_rebind_to_execution_settings() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut connection = DatabaseBuilder::new()
                .expressions(expressions.clone())
                .optimizer(optimizer)
                .build()?
                .connect();
            for (setting, ascending, descending) in [
                ("NULLS_FIRST", "[NULL, 1, 3]", "[NULL, 3, 1]"),
                ("NULLS_LAST", "[1, 3, NULL]", "[3, 1, NULL]"),
                (
                    "NULLS_FIRST_ON_ASC_LAST_ON_DESC",
                    "[NULL, 1, 3]",
                    "[3, 1, NULL]",
                ),
                (
                    "NULLS_LAST_ON_ASC_FIRST_ON_DESC",
                    "[1, 3, NULL]",
                    "[NULL, 3, 1]",
                ),
            ] {
                connection.execute(&format!(
                    "SET default_order='ASC'; SET default_null_order='{setting}'"
                ))?;
                assert_eq!(
                    connection
                        .query(
                            "SELECT
                                list_sort([3,NULL,1])::VARCHAR,
                                list_sort([3,NULL,1],'DESC')::VARCHAR",
                        )?
                        .rows,
                    vec![vec![text(ascending), text(descending)]],
                    "{setting} with ascending default",
                );
                connection.execute("SET default_order='DESC'")?;
                assert_eq!(
                    connection
                        .query(
                            "SELECT
                                list_sort([3,NULL,1])::VARCHAR,
                                list_sort([3,NULL,1],'DEFAULT','ORDER_DEFAULT')::VARCHAR,
                                list_reverse_sort([3,NULL,1])::VARCHAR",
                        )?
                        .rows,
                    vec![vec![text(descending), text(descending), text(ascending)]],
                    "{setting} with descending default",
                );
            }

            let input = connection.query("SELECT [3,NULL,1]")?.rows[0][0].clone();
            let prepared = connection.prepare(
                "SELECT
                    list_sort($1)::VARCHAR,
                    list_grade_up($1)::VARCHAR,
                    list_reverse_sort($1)::VARCHAR",
            )?;
            connection.execute("SET default_order='ASC'; SET default_null_order='NULLS_FIRST'")?;
            assert_eq!(
                connection
                    .execute_prepared(&prepared, std::slice::from_ref(&input))?
                    .rows,
                vec![vec![
                    text("[NULL, 1, 3]"),
                    text("[2, 3, 1]"),
                    text("[NULL, 3, 1]"),
                ]],
            );
            connection.execute("SET default_order='DESC'; SET default_null_order='NULLS_LAST'")?;
            assert_eq!(
                connection.execute_prepared(&prepared, &[input])?.rows,
                vec![vec![
                    text("[3, 1, NULL]"),
                    text("[1, 3, 2]"),
                    text("[1, 3, NULL]"),
                ]],
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn named_calls_reorder_arguments_and_invalid_calls_fail_during_binding() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    assert_eq!(
        connection
            .query(
                "SELECT
                    list_sort(null_order := 'NULLS FIRST',list := [3,NULL,1],sort_order := 'DESC')::VARCHAR,
                    list_sort([3,NULL,1],null_order := 'NULLS FIRST',sort_order := 'DESC')::VARCHAR,
                    grade_up(sort_order := 'ASC',null_order := 'NULLS LAST',list := [3,NULL,1])::VARCHAR,
                    list_reverse_sort(null_order := 'NULLS LAST',list := [3,NULL,1])::VARCHAR",
            )?
            .rows,
        vec![vec![
            text("[NULL, 3, 1]"),
            text("[NULL, 3, 1]"),
            text("[3, 1, 2]"),
            text("[3, 1, NULL]"),
        ]],
    );

    for sql in [
        "SELECT list_sort()",
        "SELECT list_sort([1],'ASC','NULLS LAST','extra')",
        "SELECT list_reverse_sort([1],'NULLS LAST','extra')",
        "SELECT list_sort(nope := [1])",
        "SELECT list_sort([1],list := [2])",
        "SELECT list_sort([1],2)",
        "SELECT array_sort(1)",
        "SELECT list_grade_up({'a':1})",
        "SELECT list_sort([1],v) FROM (VALUES ('ASC')) t(v)",
    ] {
        assert!(
            matches!(connection.query(sql), Err(Error::Bind(_))),
            "{sql}",
        );
    }
    for sql in [
        "SELECT list_sort([1],'SIDEWAYS')",
        "SELECT list_sort([1],'ASC','FIRST')",
        "SELECT list_reverse_sort([1],'DESC')",
    ] {
        assert!(
            matches!(connection.query(sql), Err(Error::NotImplemented(_))),
            "{sql}",
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn stable_special_values_intervals_and_nested_payloads_keep_identity() -> Result<()> {
    let mut connection = Database::memory()?.connect();
    assert_eq!(
        connection
            .query(
                "SELECT
                    list_grade_up([2,1,2,1])::VARCHAR,
                    list_sort([1.0::DOUBLE,'NaN'::DOUBLE,-1.0::DOUBLE,'NaN'::DOUBLE])::VARCHAR,
                    list_grade_up([1.0::DOUBLE,'NaN'::DOUBLE,-1.0::DOUBLE,'NaN'::DOUBLE])::VARCHAR,
                    list_sort([[1],[1,2],NULL,[NULL],[],[1,2,3]],'ASC','NULLS FIRST')::VARCHAR,
                    list_sort([{'a':3},{'a':1},{'a':2}])::VARCHAR",
            )?
            .rows,
        vec![vec![
            text("[2, 4, 1, 3]"),
            text("[-1.0, 1.0, nan, nan]"),
            text("[3, 1, 2, 4]"),
            text("[NULL, [], [1], [1, 2], [1, 2, 3], [NULL]]"),
            text("[{'a': 1}, {'a': 2}, {'a': 3}]"),
        ]],
    );

    let sorted = connection
        .query("SELECT list_sort([INTERVAL '31 days',INTERVAL '1 month'])")?
        .rows[0][0]
        .clone();
    let expected = connection
        .query("SELECT [INTERVAL '1 month',INTERVAL '31 days']")?
        .rows[0][0]
        .clone();
    assert_eq!(sorted, expected);
    let equivalent = connection
        .query("SELECT [INTERVAL '25 hours',INTERVAL '1 day 1 hour',INTERVAL '1500 minutes']")?
        .rows[0][0]
        .clone();
    assert_eq!(
        connection
            .query(
                "SELECT list_sort([INTERVAL '25 hours',INTERVAL '1 day 1 hour',INTERVAL '1500 minutes'])",
            )?
            .rows[0][0],
        equivalent,
    );
    assert_eq!(
        connection
            .query("SELECT list_grade_up([INTERVAL '31 days',INTERVAL '1 month'])::VARCHAR",)?
            .rows,
        vec![vec![text("[2, 1]")]],
    );
    Ok(())
}

#[derive(Debug)]
struct SortPoison {
    name: &'static str,
    result: DataType,
    calls: Arc<AtomicUsize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarFunction for SortPoison {
    fn name(&self) -> &str {
        self.name
    }

    fn effects(&self) -> FunctionEffects {
        FunctionEffects {
            volatile: true,
            external_access: true,
        }
    }

    fn return_type(&self, arguments: &[DataType], _: &TypeRegistry) -> Result<DataType> {
        if !arguments.is_empty() {
            return Err(Error::Bind(format!("{} accepts no arguments", self.name)));
        }
        Ok(self.result.clone())
    }

    fn evaluate(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if !arguments.is_empty() {
            return Err(Error::Internal(format!(
                "{} received bound arguments",
                self.name
            )));
        }
        self.calls.fetch_add(1, AtomicOrdering::SeqCst);
        Err(Error::Resource(format!("{} was evaluated", self.name)))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn constant_null_binding_suppresses_all_argument_and_sort_callbacks() -> Result<()> {
    for expressions in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let mut functions = FunctionRegistry::builtins();
            functions.register_scalar(Arc::new(SortPoison {
                name: "sort_poison_text",
                result: DataType::Varchar,
                calls: calls.clone(),
            }))?;
            functions.register_scalar(Arc::new(SortPoison {
                name: "sort_poison_list",
                result: NestedType::List(DataType::Integer).data_type(),
                calls: calls.clone(),
            }))?;
            let mut connection = DatabaseBuilder::new()
                .functions(functions)
                .expressions(expressions.clone())
                .optimizer(optimizer)
                .batch_size(2)
                .build()?
                .connect();
            assert_eq!(
                connection
                    .query(
                        "SELECT
                            list_sort(NULL::INTEGER[],sort_poison_text()),
                            list_sort(sort_poison_list(),NULL)",
                    )?
                    .rows,
                vec![vec![Value::Null, Value::Null]],
            );
            assert_eq!(calls.load(AtomicOrdering::SeqCst), 0);
            assert!(matches!(
                connection.query("SELECT list_sort([2,1],sort_poison_text())"),
                Err(Error::Bind(_))
            ));
            assert_eq!(calls.load(AtomicOrdering::SeqCst), 0);
        }
    }
    Ok(())
}

struct DirectArguments(DataType);

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ScalarBindArguments for DirectArguments {
    fn len(&self) -> usize {
        1
    }

    fn data_type(&self, index: usize) -> Result<DataType> {
        if index != 0 {
            return Err(Error::Bind("direct sort argument index".into()));
        }
        Ok(self.0.clone())
    }

    fn constant(&self, index: usize) -> Result<Value> {
        self.data_type(index)?;
        Err(Error::Unsupported(
            "direct sort constants are unavailable".into(),
        ))
    }

    fn is_provably_null(&self, index: usize) -> Result<bool> {
        self.data_type(index).map(|_| false)
    }
}

struct SelectedSortChild {
    comparisons: Arc<AtomicUsize>,
    fail: bool,
    interrupt: Option<InterruptHandle>,
}

impl std::fmt::Debug for SelectedSortChild {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SelectedSortChild")
            .field("fail", &self.fail)
            .field("interrupt", &self.interrupt.is_some())
            .finish()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for SelectedSortChild {
    fn name(&self) -> &'static str {
        "selected-sort-child"
    }

    fn validate_type(&self, data_type: &DataType) -> Result<()> {
        PrimitiveTypes.validate_type(data_type)
    }

    fn validate_value(
        &self,
        data_type: &DataType,
        value: &Value,
        query: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.validate_value(data_type, value, query)
    }

    fn common_type(&self, left: &DataType, right: &DataType) -> Result<Option<DataType>> {
        PrimitiveTypes.common_type(left, right)
    }

    fn compare(
        &self,
        data_type: &DataType,
        left: &Value,
        right: &Value,
        query: &QueryContext,
    ) -> Result<Ordering> {
        self.comparisons.fetch_add(1, AtomicOrdering::SeqCst);
        if self.fail {
            return Err(Error::Resource("selected sort child failure".into()));
        }
        if let Some(interrupt) = &self.interrupt {
            interrupt.interrupt();
            query.check()?;
        }
        PrimitiveTypes.compare(data_type, left, right, query)
    }

    fn write_key(
        &self,
        data_type: &DataType,
        value: &Value,
        output: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.write_key(data_type, value, output, query)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn direct_sort(adapter: Option<Arc<dyn TypeAdapter>>) -> Result<Arc<dyn ScalarFunction>> {
    let mut types = TypeRegistry::builtins();
    if let Some(adapter) = adapter {
        types.replace(DataType::Integer.family(), adapter)?;
    }
    let query = QueryContext::background().with_types(Arc::new(types));
    FunctionRegistry::builtins()
        .scalar("list_sort")?
        .bind(
            &DirectArguments(NestedType::List(DataType::Integer).data_type()),
            &query,
        )?
        .ok_or_else(|| Error::Internal("list_sort was not specialized".into()))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn direct_sort_retains_selected_child_failures_cancellation_and_limits() -> Result<()> {
    let input = integer_list(&[Some(3), Some(1), Some(2)])?;
    let comparisons = Arc::new(AtomicUsize::new(0));
    let selected = direct_sort(Some(Arc::new(SelectedSortChild {
        comparisons: comparisons.clone(),
        fail: false,
        interrupt: None,
    })))?;
    assert_eq!(
        selected.evaluate(std::slice::from_ref(&input), &QueryContext::background())?,
        integer_list(&[Some(1), Some(2), Some(3)])?,
    );
    assert!(comparisons.load(AtomicOrdering::SeqCst) > 0);

    let failing = direct_sort(Some(Arc::new(SelectedSortChild {
        comparisons: Arc::new(AtomicUsize::new(0)),
        fail: true,
        interrupt: None,
    })))?;
    assert!(matches!(
        failing.evaluate(std::slice::from_ref(&input), &QueryContext::background()),
        Err(Error::Resource(message)) if message == "selected sort child failure"
    ));

    let interrupt = InterruptHandle::default();
    let cancelling = direct_sort(Some(Arc::new(SelectedSortChild {
        comparisons: Arc::new(AtomicUsize::new(0)),
        fail: false,
        interrupt: Some(interrupt.clone()),
    })))?;
    let cancellable = QueryContext::new(interrupt, None, 2, 10)?;
    assert!(matches!(
        cancelling.evaluate(std::slice::from_ref(&input), &cancellable),
        Err(Error::Interrupted)
    ));

    let ordinary = direct_sort(None)?;
    let limited = QueryContext::new(InterruptHandle::default(), None, 2, 2)?;
    assert!(matches!(
        ordinary.evaluate(std::slice::from_ref(&input), &limited),
        Err(Error::Resource(_))
    ));
    let interrupted = InterruptHandle::default();
    interrupted.interrupt();
    let cancelled = QueryContext::new(interrupted, None, 2, 10)?;
    assert!(matches!(
        ordinary.evaluate(&[input], &cancelled),
        Err(Error::Interrupted)
    ));
    Ok(())
}
