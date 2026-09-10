use super::*;
use duckdb_rust::{
    common::{
        type_registry::{
            TypeRegistry,
            numeric::{ExactNumericTypes, LexicalNumericTypes},
        },
        vector::{DataChunk, Vector},
    },
    function::{
        FunctionRegistry,
        operator::{Operator, OperatorRegistry},
    },
    parallel::InterruptHandle,
};

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn signed_column_copies_preserve_prefixes_extrema_nulls_and_selected_views() -> Result<()> {
    for data_type in [
        DataType::TinyInt,
        DataType::SmallInt,
        DataType::Integer,
        DataType::BigInt,
        DataType::HugeInt,
    ] {
        let bits = data_type.integer_bits().unwrap();
        let minimum = i128::MIN >> (128 - bits);
        let maximum = i128::MAX >> (128 - bits);
        for values in [
            vec![
                Value::Integer(minimum),
                Value::Integer(0),
                Value::Integer(maximum),
            ],
            vec![
                Value::Null,
                Value::Integer(minimum),
                Value::Integer(maximum),
            ],
        ] {
            for column in encodings(&data_type, &values)? {
                let expected = std::iter::once(Value::Varchar("prefix".into()))
                    .chain(column.values().cloned())
                    .collect::<Vec<_>>();
                let mut output = vec![Value::Varchar("prefix".into())];
                column.append_to(&mut output);
                drop(column);
                assert_eq!(output, expected);
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn domains() -> Vec<(DataType, Vec<Value>)> {
    let mut domains = Vec::new();
    for (data_type, bits) in [
        (DataType::UTinyInt, 8),
        (DataType::USmallInt, 16),
        (DataType::UInteger, 32),
        (DataType::UBigInt, 64),
        (DataType::UHugeInt, 128),
    ] {
        let maximum = u128::MAX >> (128 - bits);
        domains.push((
            data_type,
            vec![
                Value::Unsigned(0),
                Value::Unsigned(1),
                Value::Unsigned(maximum / 2),
                Value::Unsigned(maximum),
            ],
        ));
    }
    for width in [1, 2, 9, 18, 19, 38] {
        for scale in [0, width / 2, width] {
            let maximum = 10_i128.pow(width as u32) - 1;
            domains.push((
                DataType::Decimal { width, scale },
                [-maximum, -1, 0, 1, maximum]
                    .into_iter()
                    .map(|value| decimal(value, width, scale).unwrap())
                    .collect(),
            ));
        }
    }
    domains
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn encodings(data_type: &DataType, values: &[Value]) -> Result<Vec<Vector>> {
    let flat = Vector::flat(data_type.clone(), values.to_vec())?;
    let mut nullable = values.to_vec();
    for i in (0..nullable.len()).step_by(3) {
        nullable[i] = Value::Null;
    }
    Ok(vec![
        flat.clone(),
        flat.slice(1, flat.len() - 1)?,
        Arc::new(flat.clone()).select(vec![values.len() - 1, 0, 1, 1])?,
        Vector::constant(data_type.clone(), values[0].clone(), 7)?,
        Vector::flat(data_type.clone(), nullable)?,
        Vector::constant(data_type.clone(), Value::Null, 7)?,
        flat.slice(0, 0)?,
    ])
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn unsigned_batch_division_matches_scalar_extrema_nulls_encodings_and_errors() -> Result<()> {
    let query = QueryContext::background();
    let registry = OperatorRegistry::builtins();
    for (data_type, values) in domains()
        .into_iter()
        .filter(|(data_type, _)| data_type.is_unsigned_integer())
    {
        for operator in [Operator::Modulo, Operator::IntegerDivide] {
            let bound = registry.bind(
                operator,
                &[data_type.clone(), data_type.clone()],
                query.types(),
            )?;
            for divisor in [
                Value::Null,
                Value::Unsigned(0),
                Value::Unsigned(1),
                Value::Unsigned(2),
                Value::Unsigned(3),
                Value::Unsigned(128),
                values.last().unwrap().clone(),
            ] {
                assert_eq!(
                    bound.is_total(&[None, Some(&divisor)]),
                    divisor != Value::Unsigned(0)
                );
                for left in encodings(&data_type, &values)? {
                    let right = Vector::constant(data_type.clone(), divisor.clone(), left.len())?;
                    let expected = left
                        .values()
                        .map(|value| bound.apply(&[value.clone(), divisor.clone()], &query))
                        .collect::<Result<Vec<_>>>();
                    let actual = bound
                        .apply_batch(
                            &DataChunk::new(vec![left.clone(), right], left.len())?,
                            &query,
                        )
                        .map(|v| v.values().cloned().collect::<Vec<_>>());
                    assert_eq!(
                        format!("{expected:?}"),
                        format!("{actual:?}"),
                        "{data_type} {operator:?} {divisor}"
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn small_unsigned_remainder_dictionaries_preserve_values_nulls_and_views() -> Result<()> {
    let query = QueryContext::background();
    let registry = OperatorRegistry::builtins();
    for data_type in [DataType::UTinyInt, DataType::UBigInt, DataType::UHugeInt] {
        let maximum = u128::MAX >> (128 - data_type.unsigned_bits().unwrap());
        for nullable in [false, true] {
            let input = Arc::new(Vector::flat(
                data_type.clone(),
                (0..4099)
                    .map(|i| {
                        if nullable && i % 7 == 0 {
                            Value::Null
                        } else {
                            Value::Unsigned([0, 1, maximum, 127][i % 4])
                        }
                    })
                    .collect(),
            )?);
            for left in [
                input.as_ref().clone(),
                input.slice(1, 4096)?,
                input.select((0..4099).rev().collect())?,
            ] {
                for divisor in [1, 2, 128] {
                    let bound = registry.bind(
                        Operator::Modulo,
                        &[data_type.clone(), data_type.clone()],
                        query.types(),
                    )?;
                    let value = Value::Unsigned(divisor);
                    let right = Vector::constant(data_type.clone(), value.clone(), left.len())?;
                    let expected = left
                        .values()
                        .map(|left| bound.apply(&[left.clone(), value.clone()], &query))
                        .collect::<Result<Vec<_>>>()?;
                    let result = bound.apply_batch(
                        &DataChunk::new(vec![left.clone(), right], left.len())?,
                        &query,
                    )?;
                    assert!(result.dictionary().is_some());
                    assert_eq!(result.all_valid(), !nullable);
                    assert_eq!(result.values().cloned().collect::<Vec<_>>(), expected);
                    if data_type != DataType::UHugeInt {
                        let cast = CastRegistry::builtins().bind(
                            &data_type,
                            &DataType::HugeInt,
                            CastMode::Explicit,
                            query.types(),
                        )?;
                        let comparison = query.types().bind(&DataType::HugeInt)?;
                        let right = Value::Integer(1);
                        for mask in 0..8 {
                            let predicate =
                                duckdb_rust::common::type_registry::ComparisonPredicate {
                                    less: mask & 1 != 0,
                                    equal: mask & 2 != 0,
                                    greater: mask & 4 != 0,
                                };
                            assert_eq!(
                                cast.select_integer_comparison(
                                    &result,
                                    &right,
                                    predicate,
                                    &comparison,
                                    &query
                                )?,
                                Some(comparison.select_comparison(
                                    &cast.apply_batch(&result, &query)?,
                                    &Vector::constant(
                                        DataType::HugeInt,
                                        right.clone(),
                                        result.len()
                                    )?,
                                    predicate,
                                    &query,
                                )?)
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn numeric_comparison_batches_match_both_scalar_type_adapters() -> Result<()> {
    for adapter in [
        Arc::new(ExactNumericTypes) as Arc<dyn duckdb_rust::common::type_registry::TypeAdapter>,
        Arc::new(LexicalNumericTypes),
    ] {
        for (data_type, values) in domains() {
            let mut types = TypeRegistry::builtins();
            types.replace(data_type.family(), adapter.clone())?;
            let query = QueryContext::background().with_types(Arc::new(types));
            let bound = query.types().bind(&data_type)?;
            for left in encodings(&data_type, &values)? {
                for value in values.iter().chain(std::iter::once(&Value::Null)) {
                    let right = Vector::constant(data_type.clone(), value.clone(), left.len())?;
                    let expected = left
                        .values()
                        .map(|a| {
                            if a.is_null() || value.is_null() {
                                Ok(None)
                            } else {
                                bound.compare(a, value, &query).map(Some)
                            }
                        })
                        .collect::<Result<Vec<_>>>()?;
                    assert_eq!(bound.compare_batch(&left, &right, &query)?, expected);
                    for mask in 0..8 {
                        let predicate = duckdb_rust::common::type_registry::ComparisonPredicate {
                            less: mask & 1 != 0,
                            equal: mask & 2 != 0,
                            greater: mask & 4 != 0,
                        };
                        let selected = expected
                            .iter()
                            .enumerate()
                            .filter_map(|(index, ordering)| {
                                ordering
                                    .is_some_and(|o| predicate.matches(o))
                                    .then_some(index)
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(
                            bound.select_comparison(&left, &right, predicate, &query)?,
                            selected
                        );
                        if let Some(accepted) =
                            bound.uniform_comparison(&left, &right, predicate, &query)?
                        {
                            assert!(expected.iter().all(|ordering| {
                                ordering.is_some_and(|o| predicate.matches(o)) == accepted
                            }));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn numeric_vector_order_proofs_and_concatenation_preserve_physical_values() -> Result<()> {
    for (data_type, samples) in domains() {
        for column in encodings(&data_type, &samples)? {
            if column.numeric_ascending() {
                let values = column.values().collect::<Vec<_>>();
                assert!(column.all_valid());
                assert!(
                    values
                        .windows(2)
                        .all(|pair| !pair[0].compare(pair[1]).unwrap().is_gt())
                );
            }
            let parts = [
                column.slice(0, column.len() / 2)?,
                column.slice(column.len() / 2, column.len() - column.len() / 2)?,
            ];
            let joined = Vector::concatenate(data_type.clone(), &parts)?;
            assert_eq!(
                column.values().collect::<Vec<_>>(),
                joined.values().collect::<Vec<_>>()
            );
            assert_eq!(column.all_valid(), joined.all_valid());
        }
    }
    assert!(
        Vector::concatenate(
            DataType::UBigInt,
            &[Vector::constant(DataType::BigInt, Value::Integer(1), 3)?]
        )
        .is_err()
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn exact_sum_batches_match_scalar_prefixes_nulls_and_wide_fallbacks() -> Result<()> {
    let functions = FunctionRegistry::builtins();
    let sum = functions.aggregate("sum").unwrap();
    let query = QueryContext::background();
    for (data_type, samples) in domains() {
        let values = (0..4099)
            .map(|i| samples[i % samples.len()].clone())
            .collect::<Vec<_>>();
        for column in encodings(&data_type, &values)? {
            for size in [1, 7, 1024, 2048] {
                let mut scalar =
                    sum.create_state(std::slice::from_ref(&data_type), query.types())?;
                let mut batched =
                    sum.create_state(std::slice::from_ref(&data_type), query.types())?;
                let mut borrowed =
                    sum.create_state(std::slice::from_ref(&data_type), query.types())?;
                let expected = column
                    .values()
                    .try_for_each(|value| scalar.update(std::slice::from_ref(value), &query))
                    .and_then(|_| scalar.finish());
                let actual = (0..column.len())
                    .step_by(size)
                    .try_for_each(|offset| {
                        let count = size.min(column.len() - offset);
                        batched.update_batch(
                            &DataChunk::new(vec![column.slice(offset, count)?], count)?,
                            &query,
                        )
                    })
                    .and_then(|_| batched.finish());
                let direct = (0..column.len())
                    .step_by(size)
                    .try_for_each(|offset| {
                        borrowed.update_column(
                            &column.slice(offset, size.min(column.len() - offset))?,
                            &query,
                        )
                    })
                    .and_then(|_| borrowed.finish());
                assert_eq!(format!("{expected:?}"), format!("{direct:?}"));
                assert_eq!(
                    format!("{expected:?}"),
                    format!("{actual:?}"),
                    "{data_type}, batch size {size}"
                );
            }
        }
    }
    // The dense proof must decline near either result bound, retaining the
    // scalar error at a bad prefix even when a later value would cancel it.
    for initial in [10_i128.pow(38) - 1, -10_i128.pow(38) + 1] {
        let data_type = DataType::Decimal {
            width: 38,
            scale: 0,
        };
        let mut state = sum.create_state(std::slice::from_ref(&data_type), query.types())?;
        state.update(&[decimal(initial, 38, 0)?], &query)?;
        let sign = initial.signum();
        let column = Vector::flat(
            DataType::Decimal { width: 1, scale: 0 },
            vec![decimal(sign, 1, 0)?, decimal(-sign, 1, 0)?],
        )?;
        assert!(
            state
                .update_batch(&DataChunk::new(vec![column], 2)?, &query)
                .is_err()
        );
    }
    let handle = InterruptHandle::default();
    let cancelled = QueryContext::new(handle.clone(), None, 3, 100)?;
    handle.interrupt();
    let mut state = sum.create_state(&[DataType::UBigInt], query.types())?;
    assert!(matches!(
        state.update_batch(
            &DataChunk::new(
                vec![Vector::constant(DataType::UBigInt, Value::Unsigned(1), 7)?],
                7
            )?,
            &cancelled
        ),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn exact_sum_column_proofs_cover_block_width_transitions_and_signed_tails() -> Result<()> {
    let sum = FunctionRegistry::builtins().aggregate("sum").unwrap();
    let query = QueryContext::background();
    // Width 15 admits an i64 magnitude proof for a 1024-value block; width
    // 16 does not. Full-column totals can exceed i64 in either implementation.
    for width in [12, 15, 16, 18] {
        let data_type = DataType::Decimal { width, scale: 2 };
        let maximum = 10_i128.pow(u32::from(width)) - 1;
        for count in [0, 1, 7, 8, 9, 1023, 1024, 1025, 4099, 10001] {
            for sign in [-1, 1] {
                let values = (0..count)
                    .map(|index| {
                        let direction = if index % 19 == 0 { -sign } else { sign };
                        decimal(direction * (maximum - index as i128), width, 2)
                    })
                    .collect::<Result<Vec<_>>>()?;
                let mut scalar =
                    sum.create_state(std::slice::from_ref(&data_type), query.types())?;
                for value in &values {
                    scalar.update(std::slice::from_ref(value), &query)?;
                }
                let column = Vector::flat(data_type.clone(), values)?;
                let mut vector =
                    sum.create_state(std::slice::from_ref(&data_type), query.types())?;
                vector.update_column(&column, &query)?;
                assert_eq!(vector.finish()?, scalar.finish()?, "{width}/{count}/{sign}");
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn numeric_cast_batches_retain_scalar_domains_nulls_and_totality() -> Result<()> {
    let query = QueryContext::background();
    let casts = CastRegistry::builtins();
    let targets = domains()
        .into_iter()
        .map(|(t, _)| t)
        .chain([
            DataType::TinyInt,
            DataType::BigInt,
            DataType::HugeInt,
            DataType::Varchar,
        ])
        .collect::<Vec<_>>();
    for (source, samples) in domains() {
        for target in &targets {
            let bound = casts.bind(&source, target, CastMode::Explicit, query.types())?;
            for input in encodings(&source, &samples)? {
                let expected = input
                    .values()
                    .map(|value| bound.apply(value, &query))
                    .collect::<Result<Vec<_>>>();
                let actual = bound
                    .apply_batch(&input, &query)
                    .map(|column| column.values().cloned().collect::<Vec<_>>());
                if bound.is_total() {
                    assert!(expected.is_ok(), "total cast {source} -> {target}");
                }
                assert_eq!(
                    format!("{actual:?}"),
                    format!("{expected:?}"),
                    "{source} -> {target}"
                );
            }
        }
    }
    for (source, target, total) in [
        (DataType::UBigInt, DataType::HugeInt, true),
        (DataType::UInteger, DataType::UBigInt, true),
        (DataType::UBigInt, DataType::BigInt, false),
        (DataType::UHugeInt, DataType::HugeInt, false),
    ] {
        assert_eq!(
            casts
                .bind(&source, &target, CastMode::Explicit, query.types())?
                .is_total(),
            total
        );
    }
    let comparison = query.types().bind(&DataType::HugeInt)?;
    for (source, samples) in domains()
        .into_iter()
        .filter(|(source, _)| source.unsigned_bits().is_some_and(|bits| bits < 128))
    {
        let cast = casts.bind(
            &source,
            &DataType::HugeInt,
            CastMode::Explicit,
            query.types(),
        )?;
        for input in encodings(&source, &samples)? {
            for right in [
                Value::Integer(-1),
                Value::Integer(0),
                Value::Integer(1),
                Value::Integer(i128::MAX),
                Value::Null,
            ] {
                for mask in 0..8 {
                    let predicate = duckdb_rust::common::type_registry::ComparisonPredicate {
                        less: mask & 1 != 0,
                        equal: mask & 2 != 0,
                        greater: mask & 4 != 0,
                    };
                    let expected = comparison.select_comparison(
                        &cast.apply_batch(&input, &query)?,
                        &Vector::constant(DataType::HugeInt, right.clone(), input.len())?,
                        predicate,
                        &query,
                    )?;
                    assert_eq!(
                        cast.select_integer_comparison(
                            &input,
                            &right,
                            predicate,
                            &comparison,
                            &query
                        )?,
                        Some(expected)
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn grouped_numeric_sums_preserve_scalar_states_and_decline_unbounded_overflow() -> Result<()> {
    use duckdb_rust::function::grouped::GroupSelection;
    let query = QueryContext::background();
    let sum = FunctionRegistry::builtins().aggregate("sum").unwrap();
    for (data_type, samples) in domains().into_iter().chain([
        (
            DataType::TinyInt,
            vec![Value::Integer(-128), Value::Null, Value::Integer(127)],
        ),
        (
            DataType::BigInt,
            vec![
                Value::Integer(i64::MIN as i128),
                Value::Null,
                Value::Integer(i64::MAX as i128),
            ],
        ),
    ]) {
        let eligible = data_type.integer_bits().is_some_and(|bits| bits <= 64)
            || data_type.unsigned_bits().is_some_and(|bits| bits <= 32)
            || matches!(data_type, DataType::Decimal { width: 1..=18, .. });
        assert_eq!(
            sum.create_grouped_state(std::slice::from_ref(&data_type), query.types())?
                .is_some(),
            eligible
        );
        if !eligible {
            continue;
        }
        let values = (0..4099)
            .map(|i| samples[i % samples.len()].clone())
            .collect::<Vec<_>>();
        for column in encodings(&data_type, &values)? {
            for batch_size in [1, 7, 1024, 2048] {
                for destinations in [1, 5] {
                    let mut grouped = sum
                        .create_grouped_state(std::slice::from_ref(&data_type), query.types())?
                        .unwrap();
                    grouped.resize(7, &query)?;
                    let mut scalar = (0..9)
                        .map(|_| sum.create_state(std::slice::from_ref(&data_type), query.types()))
                        .collect::<Result<Vec<_>>>()?;
                    for (row, value) in column.values().enumerate() {
                        scalar[row % destinations].update(std::slice::from_ref(value), &query)?;
                    }
                    for offset in (0..column.len()).step_by(batch_size) {
                        let len = batch_size.min(column.len() - offset);
                        let groups = (offset..offset + len)
                            .map(|row| row % destinations)
                            .collect::<Vec<_>>();
                        grouped.update_batch(
                            &GroupSelection::new(&groups, 7, &query)?,
                            &DataChunk::new(vec![column.slice(offset, len)?], len)?,
                            &query,
                        )?;
                    }
                    grouped.resize(9, &query)?;
                    let expected = scalar
                        .into_iter()
                        .map(|state| state.finish())
                        .collect::<Result<Vec<_>>>()?;
                    assert_eq!(
                        grouped.finish(&query)?,
                        expected,
                        "{data_type}, batch {batch_size}, groups {destinations}"
                    );
                }
            }
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn numeric_window_sums_match_scalar_frames_with_filters_nulls_and_selections() -> Result<()> {
    use duckdb_rust::function::window::{WindowBounds, WindowInput, WindowOptions, WindowRows};
    let query = QueryContext::background();
    let functions = FunctionRegistry::builtins();
    let sum = functions.aggregate("sum").unwrap();
    let window = functions.window("sum").unwrap();
    for (data_type, samples) in domains().into_iter().chain([
        (
            DataType::TinyInt,
            vec![Value::Integer(-128), Value::Null, Value::Integer(127)],
        ),
        (
            DataType::BigInt,
            vec![
                Value::Integer(i64::MIN as i128),
                Value::Null,
                Value::Integer(i64::MAX as i128),
            ],
        ),
    ]) {
        let mut rows = samples.into_iter().map(|v| vec![v]).collect::<Vec<_>>();
        rows.push(vec![Value::Null]);
        let rows = duckdb_rust::common::RowCollection::from_rows(1, rows)?;
        let indices = (0..rows.len()).rev().chain([0, 1, 1]).collect::<Vec<_>>();
        let count = indices.len();
        let peers = WindowBounds::uniform(0..count, count)?;
        for frames in [
            WindowBounds::uniform(0..count, count)?,
            WindowBounds::uniform(0..0, count)?,
            WindowBounds::rows(
                (0..count)
                    .map(|i| i.saturating_sub(1)..(i + 2).min(count))
                    .collect(),
            )?,
            WindowBounds::rows((0..count).map(|i| 0..i + 1).collect())?,
        ] {
            for filter in [
                vec![true; count],
                vec![false; count],
                (0..count).map(|i| i % 3 != 0).collect(),
            ] {
                let expected = frames
                    .iter()
                    .map(|frame| {
                        let mut state =
                            sum.create_state(std::slice::from_ref(&data_type), query.types())?;
                        for row in frame.clone() {
                            if filter[row] {
                                state.update(&rows[indices[row]], &query)?;
                            }
                        }
                        state.finish()
                    })
                    .collect::<Result<Vec<_>>>();
                let actual = window.evaluate(
                    &WindowInput {
                        arguments: WindowRows::new(&rows, &indices)?,
                        argument_types: std::slice::from_ref(&data_type),
                        frames: &frames,
                        peers: &peers,
                        filter: &filter,
                        options: WindowOptions {
                            filtered: true,
                            ..Default::default()
                        },
                    },
                    &query,
                );
                assert_eq!(
                    format!("{actual:?}"),
                    format!("{expected:?}"),
                    "{data_type}"
                );
            }
        }
    }
    Ok(())
}
