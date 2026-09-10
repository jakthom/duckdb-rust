use super::*;
use duckdb_rust::common::type_registry::{
    ComparisonPredicate, KeyRepresentation, KeyWriter, OrderingRepresentation, PrimitiveTypes,
    TypeAdapter, TypeRegistry, ValueValidation,
};
use duckdb_rust::{
    common::{
        cast::{CastFunction, CastSpec, numeric::ExactNumericCast},
        vector::{DataChunk, Vector},
    },
    execution::expression_executor::{BatchedEvaluator, ExpressionEvaluator, ScalarEvaluator},
    planner::BoundExpr,
};

#[derive(Debug)]
struct SelectionAdapter {
    kind: u8,
    numeric_keys: bool,
    logical: bool,
    ordered: bool,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TypeAdapter for SelectionAdapter {
    fn uniform_comparison(
        &self,
        _: &DataType,
        _: &Vector,
        _: &Vector,
        _: ComparisonPredicate,
        _: &QueryContext,
    ) -> Result<Option<bool>> {
        Ok(Some(true))
    }
    fn name(&self) -> &'static str {
        "test-selection-boundary"
    }
    fn key_representation(&self, _: &DataType) -> KeyRepresentation {
        if self.numeric_keys {
            KeyRepresentation::NumericCoefficient
        } else {
            KeyRepresentation::CanonicalBytes
        }
    }
    fn value_validation(&self) -> ValueValidation {
        if self.logical {
            ValueValidation::Logical
        } else {
            ValueValidation::Physical
        }
    }
    fn ordering_representation(&self, _: &DataType) -> OrderingRepresentation {
        if self.ordered {
            OrderingRepresentation::SignedInteger
        } else {
            OrderingRepresentation::Comparison
        }
    }
    fn validate_type(&self, _: &DataType) -> Result<()> {
        Ok(())
    }
    fn validate_value(&self, _: &DataType, _: &Value, query: &QueryContext) -> Result<()> {
        query.check()
    }
    fn common_type(&self, a: &DataType, b: &DataType) -> Result<Option<DataType>> {
        Ok(DataType::common(a, b).ok())
    }
    fn compare(
        &self,
        _: &DataType,
        a: &Value,
        b: &Value,
        query: &QueryContext,
    ) -> Result<std::cmp::Ordering> {
        query.check()?;
        a.compare(b)
    }
    fn write_key(
        &self,
        data_type: &DataType,
        value: &Value,
        key: &mut KeyWriter<'_>,
        query: &QueryContext,
    ) -> Result<()> {
        PrimitiveTypes.write_key(data_type, value, key, query)
    }
    fn select_comparison(
        &self,
        _: &DataType,
        left: &Vector,
        _: &Vector,
        _: ComparisonPredicate,
        _: &QueryContext,
    ) -> Result<Vec<usize>> {
        Ok(match self.kind {
            0 => vec![1, 1],
            1 => vec![1, 0],
            2 => vec![left.len()],
            _ => vec![1],
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn comparison_selection_checks_foreign_positions_nulls_and_capabilities() -> Result<()> {
    let predicate = ComparisonPredicate {
        less: true,
        equal: true,
        greater: true,
    };
    for kind in 0..4 {
        let mut types = TypeRegistry::builtins();
        types.replace(
            DataType::UBigInt.family(),
            Arc::new(SelectionAdapter {
                kind,
                numeric_keys: true,
                logical: false,
                ordered: false,
            }),
        )?;
        let query = QueryContext::background().with_types(Arc::new(types));
        let left = Vector::flat(
            DataType::UBigInt,
            vec![
                Value::Unsigned(0),
                if kind == 3 {
                    Value::Null
                } else {
                    Value::Unsigned(1)
                },
                Value::Unsigned(2),
            ],
        )?;
        let right = Vector::constant(DataType::UBigInt, Value::Unsigned(0), left.len())?;
        assert!(matches!(
            query
                .types()
                .bind(&DataType::UBigInt)?
                .select_comparison(&left, &right, predicate, &query),
            Err(Error::Internal(_))
        ));
        if kind == 3 {
            assert!(matches!(
                query
                    .types()
                    .bind(&DataType::UBigInt)?
                    .uniform_comparison(&left, &right, predicate, &query),
                Err(Error::Internal(_))
            ));
        }
    }
    let mut types = TypeRegistry::builtins();
    types.replace(
        DataType::Varchar.family(),
        Arc::new(SelectionAdapter {
            kind: 0,
            numeric_keys: true,
            logical: false,
            ordered: false,
        }),
    )?;
    assert!(matches!(
        types.bind(&DataType::Varchar),
        Err(Error::Bind(_))
    ));
    for (logical, ordered) in [(false, false), (true, true)] {
        let mut types = TypeRegistry::builtins();
        types.replace(
            DataType::HugeInt.family(),
            Arc::new(SelectionAdapter {
                kind: 0,
                numeric_keys: false,
                logical,
                ordered,
            }),
        )?;
        let query = QueryContext::background().with_types(Arc::new(types));
        let cast = CastRegistry::builtins().bind(
            &DataType::UBigInt,
            &DataType::HugeInt,
            CastMode::Explicit,
            query.types(),
        )?;
        let input = Vector::constant(DataType::UBigInt, Value::Unsigned(1), 3)?;
        assert!(
            cast.select_integer_comparison(
                &input,
                &Value::Integer(1),
                predicate,
                &query.types().bind(&DataType::HugeInt)?,
                &query
            )?
            .is_none()
        );
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn typed_numeric_column_constructors_check_bounds_and_stop_at_first_error() -> Result<()> {
    let mut consumed = 0;
    let result = Vector::try_unsigned(
        DataType::UTinyInt,
        [Some(255), Some(256), None].into_iter().map(|value| {
            consumed += 1;
            Ok(value)
        }),
    );
    assert!(matches!(result, Err(Error::Internal(_))));
    assert_eq!(consumed, 2);
    assert!(Vector::try_unsigned(DataType::BigInt, [Ok(Some(1))]).is_err());
    let unsigned = Vector::try_unsigned(
        DataType::UHugeInt,
        [Ok(Some(u128::MAX)), Ok(None), Ok(Some(0))],
    )?;
    assert_eq!(
        unsigned.values().cloned().collect::<Vec<_>>(),
        vec![Value::Unsigned(u128::MAX), Value::Null, Value::Unsigned(0)]
    );
    assert!(!unsigned.all_valid());
    let signed = Vector::try_hugeints([Ok(Some(i128::MIN)), Ok(Some(i128::MAX))])?;
    assert!(signed.all_valid());
    assert_eq!(
        signed.values().cloned().collect::<Vec<_>>(),
        vec![Value::Integer(i128::MIN), Value::Integer(i128::MAX)]
    );
    assert!(matches!(
        Vector::try_hugeints([Ok(Some(1)), Err(Error::Interrupted)]),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[derive(Debug)]
struct BadCast(u8);
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl CastFunction for BadCast {
    fn name(&self) -> &'static str {
        "bad-cast-batch"
    }
    fn supports(&self, spec: &CastSpec) -> bool {
        ExactNumericCast.supports(spec)
    }
    fn cast(&self, value: &Value, spec: &CastSpec, query: &QueryContext) -> Result<Value> {
        ExactNumericCast.cast(value, spec, query)
    }
    fn cast_batch(&self, input: &Vector, spec: &CastSpec, _: &QueryContext) -> Result<Vector> {
        match self.0 {
            0 => Vector::constant(spec.target.clone(), Value::Integer(1), input.len() + 1),
            1 => Vector::constant(DataType::BigInt, Value::Integer(1), input.len()),
            2 => Vector::constant(spec.target.clone(), Value::Null, input.len()),
            _ => Vector::constant(spec.target.clone(), Value::Integer(1), input.len()),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn cast_family_lookup_retains_precedence_replacement_and_mode_selection() -> Result<()> {
    let query = QueryContext::background();
    let source = DataType::UBigInt;
    let target = DataType::HugeInt;
    let mut casts = CastRegistry::default();
    casts.register_family(source.family(), target.family(), Arc::new(ExactNumericCast))?;
    let retained = casts.bind(&source, &target, CastMode::Explicit, query.types())?;
    assert!(
        casts
            .register_family(source.family(), target.family(), Arc::new(BadCast(0)))
            .is_err()
    );
    assert_eq!(
        casts
            .bind(&source, &target, CastMode::Explicit, query.types())?
            .adapter(),
        retained.adapter()
    );
    casts.replace_family(source.family(), target.family(), Arc::new(BadCast(0)))?;
    assert_eq!(
        casts
            .bind(&source, &target, CastMode::Explicit, query.types())?
            .adapter(),
        "bad-cast-batch"
    );
    casts.register(
        CastSpec {
            source: source.clone(),
            target: target.clone(),
            mode: CastMode::Explicit,
        },
        Arc::new(ExactNumericCast),
    )?;
    assert_eq!(
        casts
            .bind(&source, &target, CastMode::Explicit, query.types())?
            .adapter(),
        retained.adapter()
    );
    assert_eq!(
        casts
            .bind(&source, &target, CastMode::Assignment, query.types())?
            .adapter(),
        "bad-cast-batch"
    );
    assert!(
        casts
            .replace_family(
                source.family(),
                DataType::Boolean.family(),
                Arc::new(ExactNumericCast)
            )
            .is_err()
    );
    assert!(
        casts
            .replace_family(
                DataType::Varchar.family(),
                target.family(),
                Arc::new(ExactNumericCast)
            )
            .is_err()
    );
    assert_eq!(
        retained.apply(&Value::Unsigned(u64::MAX as u128), &query)?,
        Value::Integer(u64::MAX as i128)
    );
    assert!(
        casts
            .coercion_cost(&source, &DataType::Boolean, CastMode::Implicit)
            .is_none()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn cast_batch_boundary_rejects_foreign_shapes_nulls_and_retains_binding() -> Result<()> {
    let query = QueryContext::background();
    let spec = CastSpec {
        source: DataType::UBigInt,
        target: DataType::HugeInt,
        mode: CastMode::Explicit,
    };
    let mut casts = CastRegistry::builtins();
    let retained = casts.bind(&spec.source, &spec.target, spec.mode, query.types())?;
    let input = Vector::flat(
        spec.source.clone(),
        vec![Value::Unsigned(u64::MAX as u128), Value::Null],
    )?;
    for kind in 0..4 {
        casts.replace(spec.clone(), Arc::new(BadCast(kind)))?;
        let bound = casts.bind(&spec.source, &spec.target, spec.mode, query.types())?;
        assert!(!bound.is_total());
        assert!(matches!(
            bound.apply_batch(&input, &query),
            Err(Error::Internal(_))
        ));
        assert_eq!(
            retained
                .apply_batch(&input, &query)?
                .values()
                .cloned()
                .collect::<Vec<_>>(),
            vec![Value::Integer(u64::MAX as i128), Value::Null]
        );
    }
    assert!(matches!(
        retained.apply_batch(
            &Vector::constant(DataType::Integer, Value::Integer(1), 2)?,
            &query
        ),
        Err(Error::Internal(_))
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn dictionary_expression_reuse_preserves_first_logical_error_and_try_cast_nulls() -> Result<()> {
    let query = QueryContext::background();
    let casts = CastRegistry::builtins();
    let expression = BoundExpr::column(0, DataType::Varchar)
        .cast(DataType::BigInt, CastMode::Explicit, &casts, query.types())?
        .cast(DataType::TinyInt, CastMode::Explicit, &casts, query.types())?;
    for samples in [
        vec![Value::Varchar("bad".into()), Value::Varchar("256".into())],
        vec![Value::Varchar("1".into()), Value::Null],
    ] {
        let parent = Arc::new(Vector::flat(DataType::Varchar, samples)?);
        for first in [0, 1] {
            let selection = (0..64).map(|i| (first + i) % 2).collect::<Vec<_>>();
            let input = DataChunk::new(vec![parent.select(selection)?.slice(1, 60)?], 60)?;
            {
                let mut root = expression.clone();
                for try_cast in [false, true] {
                    if let duckdb_rust::planner::ExprKind::Cast(_, _, flag) = &mut root.kind {
                        *flag = try_cast;
                    }
                    let scalar = ScalarEvaluator
                        .evaluate_batch(&root, &input, &query)
                        .map(|v| v.values().cloned().collect::<Vec<_>>());
                    let batched = BatchedEvaluator
                        .evaluate_batch(&root, &input, &query)
                        .map(|v| v.values().cloned().collect::<Vec<_>>());
                    assert_eq!(format!("{scalar:?}"), format!("{batched:?}"));
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn numeric_dictionary_groups_match_independent_nullable_counts_and_sums() -> Result<()> {
    use duckdb_rust::{
        DatabaseBuilder,
        execution::{
            operator::aggregate::{AggregationAlgorithm, HashAggregation, OrderedAggregation},
            physical_plan::NativePhysicalPlanner,
        },
    };
    let mut groups = std::collections::BTreeMap::<Option<u128>, (i128, i128)>::new();
    for i in 0..4099 {
        let key = (i % 7 != 0).then_some(((4099 - i) % 128) as u128);
        let entry = groups.entry(key).or_default();
        entry.0 += 1;
        entry.1 += i;
    }
    let expected = groups
        .into_iter()
        .map(|(key, (count, sum))| {
            vec![
                key.map(Value::Unsigned).unwrap_or(Value::Null),
                Value::Integer(count),
                Value::Integer(sum),
            ]
        })
        .collect::<Vec<_>>();
    for alternative in [false, true] {
        for batch_size in [1, 7, 2048] {
            let aggregation: Arc<dyn AggregationAlgorithm> = if alternative {
                Arc::new(OrderedAggregation)
            } else {
                Arc::new(HashAggregation)
            };
            let expressions: Arc<dyn ExpressionEvaluator> = if alternative {
                Arc::new(ScalarEvaluator)
            } else {
                Arc::new(BatchedEvaluator)
            };
            let database = DatabaseBuilder::default()
                .physical_planner(Arc::new(
                    NativePhysicalPlanner::default().with_aggregation(aggregation),
                ))
                .expressions(expressions)
                .batch_size(batch_size)
                .build()?;
            let mut connection = database.connect();
            connection.execute("CREATE TABLE n AS SELECT i, CASE WHEN i%7=0 THEN NULL ELSE (4099-i)::UBIGINT END AS u FROM range(4099)t(i)")?;
            assert_eq!(connection.query("SELECT u%128,count(*),sum(i) FROM n GROUP BY u%128 ORDER BY u%128 NULLS FIRST")?.rows, expected);
            for step in [1, 2] {
                // Identity mapping may copy remaining destinations only after
                // every parent entry appears. Step 2 leaves half unobserved.
                connection.execute(&format!(
                    "CREATE TABLE m{step} AS SELECT i,(i*{step})::UBIGINT AS u FROM range(4099)t(i)"
                ))?;
                let mut groups = std::collections::BTreeMap::<u128, (i128, i128)>::new();
                for i in 0..4099 {
                    let entry = groups.entry(((i * step) % 128) as u128).or_default();
                    entry.0 += 1;
                    entry.1 += i;
                }
                let expected = groups
                    .into_iter()
                    .map(|(key, (count, sum))| {
                        vec![
                            Value::Unsigned(key),
                            Value::Integer(count),
                            Value::Integer(sum),
                        ]
                    })
                    .collect::<Vec<_>>();
                assert_eq!(connection.query(&format!("SELECT u%128,count(*),sum(i) FROM m{step} GROUP BY u%128 ORDER BY u%128"))?.rows, expected);
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn compact_numeric_keys_match_independent_relational_adapters_at_full_width() -> Result<()> {
    use duckdb_rust::{
        DatabaseBuilder,
        common::type_registry::{
            TypeAdapter, TypeRegistry,
            numeric::{ExactNumericTypes, LexicalNumericTypes},
        },
        execution::{
            operator::{
                aggregate::{AggregationAlgorithm, HashAggregation, OrderedAggregation},
                join::{HashJoin, JoinAlgorithm, NestedLoopJoin},
                window::{PartitionedWindows, SortedWindows, WindowAlgorithm},
            },
            physical_plan::NativePhysicalPlanner,
        },
    };
    for sql_type in ["UHUGEINT", "DECIMAL(38,0)", "DECIMAL(18,2)"] {
        let extrema = match sql_type {
            "UHUGEINT" => {
                "(0),(1),('170141183460469231731687303715884105727'),('170141183460469231731687303715884105728'),('340282366920938463463374607431768211455')"
            }
            "DECIMAL(38,0)" => {
                "(-99999999999999999999999999999999999999),(-1),(0),(1),(99999999999999999999999999999999999999)"
            }
            _ => "(-9999999999999999.99),(-0.01),(0),(0.01),(9999999999999999.99)",
        };
        let queries = [
            "SELECT k,count(*) FROM n GROUP BY k ORDER BY k",
            "SELECT count(*) FROM n a JOIN n b ON a.k=b.k",
            "SELECT count(*) FROM n a FULL JOIN n b ON a.k=b.k",
            "SELECT k FROM n WHERE EXISTS(SELECT 1 FROM n b WHERE b.k=n.k) ORDER BY k",
            "SELECT k FROM n UNION SELECT k FROM n ORDER BY k",
            "SELECT k FROM n INTERSECT ALL SELECT k FROM n ORDER BY k",
            "SELECT k,count(*) OVER(PARTITION BY k) FROM n ORDER BY k",
        ];
        let mut expected = None;
        for composition in 0..4 {
            let alternative = composition % 2 == 1;
            let alternate_execution = composition / 2 == 1;
            for batch_size in [1, 3, 2048] {
                let adapter: Arc<dyn TypeAdapter> = if alternative {
                    Arc::new(LexicalNumericTypes)
                } else {
                    Arc::new(ExactNumericTypes)
                };
                let join: Arc<dyn JoinAlgorithm> = if alternate_execution {
                    Arc::new(NestedLoopJoin)
                } else {
                    Arc::new(HashJoin)
                };
                let aggregate: Arc<dyn AggregationAlgorithm> = if alternate_execution {
                    Arc::new(OrderedAggregation)
                } else {
                    Arc::new(HashAggregation)
                };
                let windows: Arc<dyn WindowAlgorithm> = if alternate_execution {
                    Arc::new(SortedWindows::default())
                } else {
                    Arc::new(PartitionedWindows::default())
                };
                let mut types = TypeRegistry::builtins();
                types.replace(DataType::UHugeInt.family(), adapter.clone())?;
                types.replace(
                    DataType::Decimal {
                        width: 38,
                        scale: 0,
                    }
                    .family(),
                    adapter,
                )?;
                let mut connection = DatabaseBuilder::new()
                    .types(Arc::new(types))
                    .physical_planner(Arc::new(
                        NativePhysicalPlanner::with_joins(vec![join])
                            .with_aggregation(aggregate)
                            .with_windows(windows),
                    ))
                    .batch_size(batch_size)
                    .build()?
                    .connect();
                connection.execute(&format!(
                    "CREATE TABLE n(k {sql_type}); INSERT INTO n VALUES {extrema},(NULL),(1),(NULL)"
                ))?;
                let actual = queries
                    .iter()
                    .map(|sql| connection.query(sql).map(|r| r.rows))
                    .collect::<Result<Vec<_>>>()?;
                if let Some(expected) = &expected {
                    assert_eq!(
                        &actual, expected,
                        "{sql_type}, alternative={alternative}, batch={batch_size}"
                    );
                } else {
                    expected = Some(actual);
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn dense_join_growth_handles_maximum_keys_and_sparse_transitions() -> Result<()> {
    use duckdb_rust::DatabaseBuilder;
    for data_type in [DataType::HugeInt, DataType::UHugeInt] {
        let middle = i128::MAX.to_string();
        let low = if data_type == DataType::HugeInt {
            i128::MIN.to_string()
        } else {
            u128::MAX.to_string()
        };
        for values in [
            vec![middle.clone(), middle.clone(), middle.clone()],
            vec![
                middle.clone(),
                (i128::MAX - 1).to_string(),
                middle.clone(),
                low,
            ],
        ] {
            let expected = values
                .iter()
                .map(|value| values.iter().filter(|other| *other == value).count())
                .sum::<usize>();
            for batch_size in [1, 2, 3, 2048] {
                let mut connection = DatabaseBuilder::new()
                    .batch_size(batch_size)
                    .build()?
                    .connect();
                let values = values
                    .iter()
                    .map(|v| format!("('{v}')"))
                    .collect::<Vec<_>>()
                    .join(",");
                connection.execute(&format!(
                    "CREATE TABLE n(k {data_type}); INSERT INTO n VALUES {values}"
                ))?;
                for kind in ["INNER", "LEFT", "RIGHT", "FULL"] {
                    assert_eq!(
                        connection
                            .query(&format!(
                                "SELECT count(*) FROM n a {kind} JOIN n b ON a.k=b.k"
                            ))?
                            .rows,
                        vec![vec![Value::Integer(expected as i128)]]
                    );
                }
            }
        }
    }
    Ok(())
}
