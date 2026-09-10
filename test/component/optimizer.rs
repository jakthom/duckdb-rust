use std::sync::Arc;

use duckdb_rust::{
    DataType, DatabaseBuilder, Error, Result, Value,
    common::{
        cast::{CastFunction, CastMode, CastRegistry, CastSpec, DigitIntegerCast},
        type_registry::{
            TypeRegistry,
            ascii::{self, AsciiCast, StreamingAscii},
        },
    },
    optimizer::{
        IdentityOptimizer, Optimizer, OptimizerContext, PipelineOptimizer, SimplifyExpressions,
        ValidatedPlan,
    },
    parallel::{InterruptHandle, QueryContext},
    planner::{BoundExpr, ExprKind, Field, LogicalPlan, PlanNode},
    storage::table::Snapshot,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn optimize(expression: BoundExpr, query: &QueryContext) -> Result<BoundExpr> {
    let snapshot = Snapshot::new(query.type_registry());
    let plan = LogicalPlan {
        schema: vec![Field::new("value", expression.data_type.clone())],
        node: PlanNode::Values(vec![vec![expression]]),
    };
    let context = OptimizerContext {
        catalog: &snapshot,
        storage: &snapshot,
        query,
    };
    let plan = PipelineOptimizer::new(vec![Arc::new(SimplifyExpressions)])
        .optimize(ValidatedPlan::new(plan, &context)?)?
        .into_plan(&context)?;
    let PlanNode::Values(mut rows) = plan.node else {
        panic!("values plan changed")
    };
    Ok(rows.remove(0).remove(0))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn folding_retains_declared_types_selected_casts_and_float_bits() -> Result<()> {
    let mut registry = CastRegistry::builtins();
    registry.replace(
        CastSpec {
            source: DataType::Varchar,
            target: DataType::Integer,
            mode: CastMode::Explicit,
        },
        Arc::new(DigitIntegerCast),
    )?;
    let query = QueryContext::background();
    for (source, target, expected) in [
        (
            Value::Varchar("-2147483648".into()),
            DataType::Integer,
            Value::Integer(-2147483648),
        ),
        (Value::Double(-0.0), DataType::Float, Value::Float(-0.0)),
        (
            Value::Float(f32::from_bits(0x7fc12345)),
            DataType::Double,
            Value::Double(f32::from_bits(0x7fc12345) as f64),
        ),
        (Value::Null, DataType::BigInt, Value::Null),
    ] {
        let bound = BoundExpr::literal(source).cast(
            target.clone(),
            CastMode::Explicit,
            &registry,
            query.types(),
        )?;
        let folded = optimize(bound, &query)?;
        assert_eq!(folded.data_type, target);
        let ExprKind::Literal(value) = folded.kind else {
            panic!("constant cast not folded")
        };
        match (value, expected) {
            (Value::Float(actual), Value::Float(expected)) => {
                assert_eq!(actual.to_bits(), expected.to_bits())
            }
            (Value::Double(actual), Value::Double(expected)) => {
                assert_eq!(actual.to_bits(), expected.to_bits())
            }
            (actual, expected) => assert_eq!(actual, expected),
        }
    }
    // Narrow logical metadata must survive even though Value::Integer uses i128.
    let nested = BoundExpr::literal(Value::Varchar("127".into()))
        .cast(
            DataType::TinyInt,
            CastMode::Explicit,
            &registry,
            query.types(),
        )?
        .cast(
            DataType::BigInt,
            CastMode::Implicit,
            &registry,
            query.types(),
        )?;
    assert!(matches!(
        optimize(nested, &query)?.kind,
        ExprKind::Literal(Value::Integer(127))
    ));

    let mut types = TypeRegistry::builtins();
    types.register(ascii::FAMILY, Arc::new(StreamingAscii))?;
    let data_type = ascii::data_type(16)?;
    registry.register(
        CastSpec {
            source: DataType::Varchar,
            target: data_type.clone(),
            mode: CastMode::Explicit,
        },
        Arc::new(AsciiCast),
    )?;
    let query = query.with_types(Arc::new(types));
    let expression = BoundExpr::literal(Value::Varchar("A\0b".into())).cast(
        data_type.clone(),
        CastMode::Explicit,
        &registry,
        query.types(),
    )?;
    // No registry lookup is needed after the bound adapter is retained.
    drop(registry);
    let folded = optimize(expression, &query)?;
    assert_eq!(folded.data_type, data_type);
    assert!(
        matches!(folded.kind, ExprKind::Literal(ref value) if value == &Value::extension(data_type, b"A\0b".to_vec()))
    );
    Ok(())
}

#[derive(Debug)]
struct ResourceFailure;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for ResourceFailure {
    fn name(&self) -> &'static str {
        "resource-failure"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        spec.source == DataType::Varchar && spec.target == DataType::Integer
    }
    fn cast(&self, _: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        Err(Error::Resource("injected cast allocation failure".into()))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn failed_folding_preserves_lazy_errors_and_query_cancellation() -> Result<()> {
    let query = QueryContext::background();
    let mut registry = CastRegistry::builtins();
    let invalid = BoundExpr::literal(Value::Varchar("bad".into())).cast(
        DataType::Integer,
        CastMode::Explicit,
        &registry,
        query.types(),
    )?;
    assert!(matches!(
        optimize(invalid, &query)?.kind,
        ExprKind::Cast(..)
    ));
    registry.replace(
        CastSpec {
            source: DataType::Varchar,
            target: DataType::Integer,
            mode: CastMode::Explicit,
        },
        Arc::new(ResourceFailure),
    )?;
    let failing = BoundExpr::literal(Value::Varchar("1".into())).cast(
        DataType::Integer,
        CastMode::Explicit,
        &registry,
        query.types(),
    )?;
    assert!(matches!(
        optimize(failing.clone(), &query)?.kind,
        ExprKind::Cast(..)
    ));
    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 16, 100)?;
    interrupt.interrupt();
    assert!(matches!(
        optimize(failing, &cancelled),
        Err(Error::Interrupted)
    ));

    for optimizer in optimizers() {
        let mut connection = DatabaseBuilder::new()
            .optimizer(optimizer)
            .casts(registry.clone())
            .build()?
            .connect();
        for sql in [
            "SELECT CASE WHEN true THEN 1 ELSE CAST('2' AS INTEGER) END",
            "SELECT coalesce(1, CAST('2' AS INTEGER))",
            "SELECT CAST('2' AS INTEGER) FROM range(0)",
            "SELECT CAST('2' AS INTEGER) FROM range(1) LIMIT 0",
        ] {
            connection.query(sql)?;
        }
        for sql in [
            "SELECT CAST('2' AS INTEGER)",
            "SELECT TRY_CAST('2' AS INTEGER)",
        ] {
            assert!(
                matches!(connection.query(sql), Err(Error::Resource(_))),
                "{sql}"
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn optimizers() -> Vec<Arc<dyn Optimizer>> {
    vec![
        Arc::new(IdentityOptimizer),
        Arc::new(PipelineOptimizer::new(vec![Arc::new(SimplifyExpressions)])),
        Arc::new(PipelineOptimizer::default()),
    ]
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn validated_plans_reject_invalid_changes_context_transfer_and_cancellation() -> Result<()> {
    let snapshot = Snapshot::default();
    let query = QueryContext::background();
    let context = OptimizerContext {
        catalog: &snapshot,
        storage: &snapshot,
        query: &query,
    };
    let other_context = OptimizerContext {
        catalog: &snapshot,
        storage: &snapshot,
        query: &query,
    };
    let plan = LogicalPlan {
        schema: vec![Field::new("value", DataType::BigInt)],
        node: PlanNode::Range {
            start: 0,
            end: 1,
            step: 1,
        },
    };
    let mut invalid = plan.clone();
    invalid.schema[0].data_type = DataType::Varchar;
    assert!(matches!(
        ValidatedPlan::new(invalid, &context),
        Err(Error::Bind(_))
    ));
    assert!(matches!(
        ValidatedPlan::new(plan.clone(), &context)?.rewrite(|mut plan, _| {
            plan.schema[0].data_type = DataType::Varchar;
            Ok(plan)
        }),
        Err(Error::Bind(_))
    ));
    assert!(matches!(
        ValidatedPlan::new(plan.clone(), &context)?.into_plan(&other_context),
        Err(Error::Internal(_))
    ));
    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 16, 100)?;
    let context = OptimizerContext {
        catalog: &snapshot,
        storage: &snapshot,
        query: &cancelled,
    };
    let validated = ValidatedPlan::new(plan.clone(), &context)?;
    interrupt.interrupt();
    assert!(matches!(
        validated.into_plan(&context),
        Err(Error::Interrupted)
    ));
    assert!(matches!(
        ValidatedPlan::new(plan, &context),
        Err(Error::Interrupted)
    ));
    Ok(())
}

struct InvalidSchema;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::optimizer::OptimizerPass for InvalidSchema {
    fn name(&self) -> &'static str {
        "invalid-schema"
    }
    fn rewrite(&self, mut plan: LogicalPlan, _: &OptimizerContext<'_>) -> Result<LogicalPlan> {
        plan.schema[0].data_type = DataType::Varchar;
        Ok(plan)
    }
}

struct ObservePass(Arc<std::sync::atomic::AtomicUsize>);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl duckdb_rust::optimizer::OptimizerPass for ObservePass {
    fn name(&self) -> &'static str {
        "observe-pass"
    }
    fn rewrite(&self, plan: LogicalPlan, _: &OptimizerContext<'_>) -> Result<LogicalPlan> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(plan)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn invalid_pass_output_never_reaches_later_passes_or_execution() -> Result<()> {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let optimizer = PipelineOptimizer::new(vec![
        Arc::new(InvalidSchema),
        Arc::new(ObservePass(calls.clone())),
    ]);
    let mut connection = DatabaseBuilder::new()
        .optimizer(Arc::new(optimizer))
        .build()?
        .connect();
    assert!(matches!(
        connection.query("SELECT range FROM range(1)"),
        Err(Error::Bind(_))
    ));
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn optimizer_compositions_preserve_relational_and_short_circuit_results() -> Result<()> {
    let queries = [
        "SELECT CASE WHEN true THEN 2 ELSE CAST('bad' AS INTEGER) END, coalesce(1, CAST('bad' AS INTEGER)), TRY_CAST('bad' AS INTEGER)",
        "SELECT CAST('bad' AS INTEGER) FROM range(0)",
        "SELECT range%17 FROM range(100) WHERE range%17=0 ORDER BY range DESC LIMIT 3",
        "SELECT range%3, sum(CAST('2' AS BIGINT)) FILTER (WHERE range<CAST('5' AS BIGINT)) FROM range(10) GROUP BY range%3 HAVING count(*)>CAST('2' AS BIGINT) ORDER BY range%3",
        "SELECT a.range, b.range FROM range(4) a LEFT JOIN range(2) b ON a.range=b.range+CAST('1' AS BIGINT) ORDER BY a.range",
        "SELECT range FROM range(5) WHERE range IN (CAST('2' AS BIGINT),CAST('3' AS BIGINT)) UNION SELECT CAST('4' AS BIGINT)",
        "SELECT CAST('bad' AS INTEGER)",
    ];
    let mut baseline = None;
    for optimizer in optimizers() {
        let name = optimizer.name();
        let mut connection = DatabaseBuilder::new()
            .optimizer(optimizer)
            .build()?
            .connect();
        let result = queries
            .iter()
            .map(|sql| {
                connection
                    .query(sql)
                    .map(|result| result.rows)
                    .map_err(|error| error.to_string())
            })
            .collect::<Vec<_>>();
        for (sql, result) in queries[..6].iter().zip(&result) {
            assert!(result.is_ok(), "{name}: {sql}: {result:?}");
        }
        assert!(result[6].is_err());
        if let Some(expected) = &baseline {
            assert_eq!(&result, expected, "{name}");
        } else {
            baseline = Some(result);
        }
    }
    Ok(())
}
