use super::*;
use duckdb_rust::{
    common::{
        NestedPayload, NestedType, NestedValue,
        cast::{CastBehavior, CastFailure, CastResult, CastSourceContext},
        type_registry::{KeyWriter, TypeAdapter, TypeRegistry, builtin_types},
    },
    execution::expression_executor::BatchedEvaluator,
    optimizer::{IdentityOptimizer, Optimizer, PipelineOptimizer},
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn failure(code: u8) -> Error {
    match code {
        0 => Error::Conversion("cast conversion witness".into()),
        1 => Error::InvalidInput("cast invalid input witness".into()),
        2 => Error::OutOfRange("cast range witness".into()),
        3 => Error::Internal("cast internal witness".into()),
        4 => Error::Resource("cast resource witness".into()),
        5 => Error::Interrupted,
        6 => Error::Corrupt("cast corruption witness".into()),
        _ => Error::Execution("cast execution witness".into()),
    }
}

#[derive(Debug)]
struct ClassifiedCast(u8, bool);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for ClassifiedCast {
    fn name(&self) -> &'static str {
        "classified-test-cast"
    }
    fn supports(&self, _: &CastSpec) -> bool {
        true
    }
    fn cast(&self, _: &Value, _: &CastSpec, _: &QueryContext) -> Result<Value> {
        Err(failure(self.0))
    }
    fn cast_attempt(
        &self,
        value: &Value,
        spec: &CastSpec,
        _: CastBehavior,
        query: &QueryContext,
    ) -> CastResult<Value> {
        self.cast(value, spec, query).map_err(if self.1 {
            CastFailure::invalid_input
        } else {
            CastFailure::fatal
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn try_cast_uses_selected_failure_origin_without_reclassifying_public_errors() -> Result<()> {
    let query = QueryContext::background();
    let source = DataType::Varchar;
    let target = DataType::Integer;
    let input = Value::Varchar("x".into());
    for code in 0..8 {
        for recoverable in [false, true] {
            let mut casts = CastRegistry::builtins();
            casts.replace(
                spec(target.clone(), CastMode::Explicit),
                Arc::new(ClassifiedCast(code, recoverable)),
            )?;
            let retained = casts.bind(&source, &target, CastMode::Explicit, query.types())?;
            casts.replace(
                spec(target.clone(), CastMode::Explicit),
                Arc::new(PrimitiveCast),
            )?;
            assert_eq!(
                retained.apply(&input, &query).unwrap_err().to_string(),
                failure(code).to_string()
            );
            let result = retained.apply_try(&input, &query);
            if recoverable && code <= 2 {
                assert_eq!(result?, Value::Null);
            } else {
                assert_eq!(result.unwrap_err().to_string(), failure(code).to_string());
            }
            // The selected child contract must survive a composite conversion.
            let mut casts = CastRegistry::builtins();
            casts.replace(
                spec(target.clone(), CastMode::Explicit),
                Arc::new(ClassifiedCast(code, recoverable)),
            )?;
            let list = |ty| NestedType::List(ty).data_type();
            let bound = casts.bind(
                &list(source.clone()),
                &list(target.clone()),
                CastMode::Explicit,
                query.types(),
            )?;
            let input = NestedValue::value(
                list(source.clone()),
                NestedPayload::Sequence(vec![input.clone()]),
            )?;
            let result = bound.apply_try(&input, &query);
            if recoverable && code <= 2 {
                assert_eq!(
                    result?,
                    NestedValue::value(
                        list(target.clone()),
                        NestedPayload::Sequence(vec![Value::Null])
                    )?
                );
            } else {
                assert_eq!(result.unwrap_err().to_string(), failure(code).to_string());
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct RejectLogical(u8);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for RejectLogical {
    fn name(&self) -> &'static str {
        "reject-logical-test"
    }
    fn validate_type(&self, _: &DataType) -> Result<()> {
        Ok(())
    }
    fn validate_value(&self, _: &DataType, _: &Value, _: &QueryContext) -> Result<()> {
        Err(failure(self.0))
    }
    fn common_type(&self, _: &DataType, _: &DataType) -> Result<Option<DataType>> {
        Ok(None)
    }
    fn compare(
        &self,
        _: &DataType,
        _: &Value,
        _: &Value,
        _: &QueryContext,
    ) -> Result<std::cmp::Ordering> {
        unreachable!("validation fails first")
    }
    fn write_key(
        &self,
        _: &DataType,
        _: &Value,
        _: &mut KeyWriter<'_>,
        _: &QueryContext,
    ) -> Result<()> {
        unreachable!("validation fails first")
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn try_cast_keeps_source_target_and_nested_child_validation_failures_fatal() -> Result<()> {
    let query = QueryContext::background();
    let casts = CastRegistry::builtins();
    for code in 0..8 {
        for source_failure in [false, true] {
            let mut types: TypeRegistry = builtin_types().as_ref().clone();
            types.replace(
                if source_failure {
                    "builtin.varchar"
                } else {
                    "builtin.integer"
                },
                Arc::new(RejectLogical(code)),
            )?;
            for nested in [false, true] {
                let shape = |ty| {
                    if nested {
                        NestedType::List(ty).data_type()
                    } else {
                        ty
                    }
                };
                let source = shape(DataType::Varchar);
                let target = shape(DataType::Integer);
                let input = Value::Varchar("7".into());
                let input = if nested {
                    NestedValue::value(source.clone(), NestedPayload::Sequence(vec![input]))?
                } else {
                    input
                };
                let bound = casts.bind(&source, &target, CastMode::Explicit, &types)?;
                for source_context in [CastSourceContext::Ordinary, CastSourceContext::Variant] {
                    let error = bound
                        .attempt_with_context(&input, CastBehavior::Try, source_context, &query)
                        .map_err(CastFailure::into_error)
                        .unwrap_err();
                    if !source_failure && code == 0 {
                        assert!(matches!(error, Error::Internal(_)));
                    } else {
                        assert_eq!(error.to_string(), failure(code).to_string());
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn nested_try_casts_preserve_partial_children_and_variant_whole_value_failures() -> Result<()> {
    for evaluator in [
        Arc::new(ScalarEvaluator) as Arc<dyn ExpressionEvaluator>,
        Arc::new(BatchedEvaluator),
    ] {
        for optimizer in [
            Arc::new(IdentityOptimizer) as Arc<dyn Optimizer>,
            Arc::new(PipelineOptimizer::default()),
        ] {
            let mut c = DatabaseBuilder::new()
                .expressions(evaluator.clone())
                .optimizer(optimizer.clone())
                .batch_size(3)
                .build()?
                .connect();
            for (expression, expected) in [
                ("TRY_CAST(['1','x'] AS INTEGER[])", "[1, NULL]"),
                ("TRY_CAST(['1','x'] AS INTEGER[2])", "[1, NULL]"),
                (
                    "TRY_CAST({'a':'x','b':'1'} AS STRUCT(a INTEGER,b INTEGER))",
                    "{'a': NULL, 'b': 1}",
                ),
                (
                    "TRY_CAST(map(['1'],['x']) AS MAP(INTEGER,INTEGER))",
                    "{1=NULL}",
                ),
                ("TRY_CAST(map(['x'],['1']) AS MAP(INTEGER,INTEGER))", "NULL"),
                (
                    "TRY_CAST(map(['01','1'],['a','b']) AS MAP(INTEGER,VARCHAR))",
                    "NULL",
                ),
                ("TRY_CAST(['1','x']::VARIANT AS INTEGER[])", "NULL"),
                (
                    "TRY_CAST({'a':'x','b':'1'}::VARIANT AS STRUCT(a INTEGER,b INTEGER))",
                    "NULL",
                ),
                ("TRY_CAST('x'::ENUM('x','1') AS INTEGER)", "NULL"),
                (
                    "union_tag(TRY_CAST(union_value(a:='x') AS UNION(a INTEGER)))",
                    "a",
                ),
            ] {
                let sql = format!("SELECT {expression} FROM range(7)");
                assert_eq!(
                    c.query(&sql)?
                        .rows
                        .into_iter()
                        .map(|row| row[0].to_string())
                        .collect::<Vec<_>>(),
                    vec![expected; 7],
                    "{sql}"
                );
            }
            assert_eq!(
                c.query("SELECT TRY_CAST(union_value(a:='x') AS UNION(a INTEGER)) IS NULL")?
                    .rows,
                vec![vec![Value::Boolean(false)]]
            );
            assert!(matches!(
                c.query("SELECT TRY_CAST(CAST('x' AS INTEGER) AS VARCHAR)"),
                Err(Error::Conversion(_))
            ));
            assert!(matches!(
                c.query("SELECT CAST(map(['01','1'],['a','b']) AS MAP(INTEGER,VARCHAR))"),
                Err(Error::Conversion(_))
            ));
            c.execute("CREATE TABLE t(id INTEGER PRIMARY KEY,xs INTEGER[],s STRUCT(d DECIMAL(8,2),t TIMESTAMP)); INSERT INTO t VALUES(1,[9],{'d':2.50,'t':TIMESTAMP '2000-01-01'})")?;
            let prepared = c.prepare("UPDATE t SET xs=TRY_CAST(? AS INTEGER[]),s=TRY_CAST({'d':?, 't':?} AS STRUCT(d DECIMAL(8,2),t TIMESTAMP)) WHERE id=1")?;
            let parameters = vec![
                NestedValue::value(
                    NestedType::List(DataType::Varchar).data_type(),
                    NestedPayload::Sequence(vec![
                        Value::Varchar("1".into()),
                        Value::Varchar("x".into()),
                    ]),
                )?,
                Value::Varchar("bad".into()),
                Value::Varchar("2001-01-01".into()),
            ];
            c.execute("BEGIN")?;
            c.execute_prepared(&prepared, &parameters)?;
            assert_eq!(
                c.query("SELECT xs,s.d,CAST(s.t AS VARCHAR) FROM t")?.rows[0]
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                vec!["[1, NULL]", "NULL", "2001-01-01 00:00:00"]
            );
            c.execute("ROLLBACK")?;
            assert_eq!(c.query("SELECT xs FROM t")?.rows[0][0].to_string(), "[9]");
        }
    }
    Ok(())
}
