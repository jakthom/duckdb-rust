use super::*;
use crate::parallel::InterruptHandle;
use std::sync::Arc;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn bound(separator: Option<&str>) -> StringAgg {
    StringAgg("string_agg", Some(separator.map(str::to_owned)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn encodings() -> Result<Vec<Vector>> {
    let flat = Vector::flat(
        DataType::Varchar,
        vec![
            Value::Null,
            Value::Varchar(String::new()),
            Value::Varchar("é".into()),
            Value::Varchar("中\0x".into()),
            Value::Null,
            Value::Varchar("z".into()),
        ],
    )?;
    let dictionary = Arc::new(flat.clone()).select(vec![5, 1, 3, 1, 0, 2])?;
    let offset_dictionary = Arc::new(flat.slice(1, 4)?).select(vec![3, 0, 2, 1])?;
    let chunks = Vector::chunked(
        DataType::Varchar,
        vec![flat.slice(0, 3)?, flat.slice(3, 3)?],
    )?;
    Ok(vec![
        flat.clone(),
        flat.slice(1, 4)?,
        dictionary.clone(),
        dictionary.slice(1, 4)?,
        Arc::new(dictionary).select(vec![2, 0, 2, 5])?,
        offset_dictionary.clone(),
        offset_dictionary.slice(1, 2)?,
        chunks.clone(),
        chunks.slice(1, 4)?,
        Vector::constant(DataType::Varchar, Value::Varchar("界".into()), 2051)?,
        Vector::constant(DataType::Varchar, Value::Null, 2051)?,
        Vector::flat(DataType::Varchar, Vec::new())?,
        Vector::constant(DataType::Varchar, Value::Varchar("unused".into()), 0)?,
    ])
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn column_and_batch_match_scalar_for_all_encodings_and_separators() -> Result<()> {
    let query = QueryContext::background();
    for separator in [None, Some(""), Some("|界")] {
        let function = bound(separator);
        for column in encodings()? {
            let mut scalar = function.create_state(&[DataType::Varchar], query.types())?;
            let mut batch = function.create_state(&[DataType::Varchar], query.types())?;
            for _ in 0..2 {
                for value in column.values() {
                    scalar.update(&[value], &query)?;
                }
            }
            batch.update_column(&column, &query)?;
            batch.update_batch(&DataChunk::new(vec![column.clone()], column.len())?, &query)?;
            assert_eq!(batch.finish()?, scalar.finish()?);
        }
    }
    let mut state = bound(Some("|")).create_state(&[DataType::Varchar], query.types())?;
    for value in [
        Value::Null,
        Value::Varchar(String::new()),
        Value::Varchar("é".into()),
    ] {
        state.update(&[value], &query)?;
    }
    assert_eq!(state.finish()?, Value::Varchar("|é".into()));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn grouped_columns_preserve_growth_empty_groups_and_row_order() -> Result<()> {
    let query = QueryContext::background();
    for separator in [None, Some(""), Some("|界")] {
        let function = bound(separator);
        for column in encodings()? {
            let mut grouped = function
                .create_grouped_state(&[DataType::Varchar], query.types())?
                .expect("STRING_AGG grouped opt-in");
            let mut scalar = (0..3)
                .map(|_| function.create_state(&[DataType::Varchar], query.types()))
                .collect::<Result<Vec<_>>>()?;
            let chunk = DataChunk::new(vec![column.clone()], column.len())?;
            grouped.resize(1, &query)?;
            let first = vec![0; column.len()];
            grouped.update_batch(&GroupSelection::new(&first, 1, &query)?, &chunk, &query)?;
            for value in column.values() {
                scalar[0].update(&[value], &query)?;
            }
            grouped.resize(3, &query)?;
            let second = (0..column.len()).map(|row| row % 2).collect::<Vec<_>>();
            grouped.update_batch(&GroupSelection::new(&second, 3, &query)?, &chunk, &query)?;
            for (row, value) in column.values().enumerate() {
                scalar[second[row]].update(&[value], &query)?;
            }
            assert_eq!(
                grouped.finish(&query)?,
                scalar
                    .into_iter()
                    .map(|state| state.finish())
                    .collect::<Result<Vec<_>>>()?
            );
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn grouped_direct_delivery_preserves_interleaved_flat_dictionary_and_constant_order() -> Result<()>
{
    let query = QueryContext::background();
    let function = bound(Some("|"));
    let mut grouped = function
        .create_grouped_state(&[DataType::Varchar], query.types())?
        .expect("STRING_AGG grouped opt-in");
    let mut scalar = (0..3)
        .map(|_| function.create_state(&[DataType::Varchar], query.types()))
        .collect::<Result<Vec<_>>>()?;
    grouped.resize(3, &query)?;

    let flat = Vector::flat(
        DataType::Varchar,
        vec![
            Value::Varchar("a".into()),
            Value::Null,
            Value::Varchar(String::new()),
            Value::Varchar("界".into()),
        ],
    )?;
    let dictionary_parent = Arc::new(Vector::flat(
        DataType::Varchar,
        vec![
            Value::Varchar("x".into()),
            Value::Null,
            Value::Varchar("é\0".into()),
        ],
    )?);
    let dictionary = dictionary_parent.select(vec![2, 0, 1, 2])?;
    let constant = Vector::constant(DataType::Varchar, Value::Varchar("c".into()), 4)?;

    for (column, destinations) in [
        (flat, vec![2, 0, 2, 1]),
        (dictionary, vec![0, 2, 1, 0]),
        (constant, vec![1, 0, 1, 2]),
    ] {
        let chunk = DataChunk::new(vec![column.clone()], column.len())?;
        grouped.update_batch(
            &GroupSelection::new(&destinations, 3, &query)?,
            &chunk,
            &query,
        )?;
        for (destination, value) in destinations.into_iter().zip(column.values()) {
            scalar[destination].update(&[value], &query)?;
        }
    }

    assert_eq!(
        grouped.finish(&query)?,
        scalar
            .into_iter()
            .map(|state| state.finish())
            .collect::<Result<Vec<_>>>()?
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn grouped_inline_storage_preserves_boundaries_promotion_and_capacity_policy() -> Result<()> {
    assert_eq!(GroupedStringBuffer::default().finish()?, Value::Null);

    let mut empty = GroupedStringBuffer::default();
    empty.append(&Value::Varchar(String::new()), "|")?;
    assert!(matches!(empty, GroupedStringBuffer::Inline { len: 0, .. }));
    assert_eq!(empty.finish()?, Value::Varchar(String::new()));

    let boundary = format!("{}é", "a".repeat(30));
    assert_eq!(boundary.len(), GROUPED_INLINE_BYTES);
    let mut inline = GroupedStringBuffer::default();
    inline.append(&Value::Varchar(boundary.clone()), "|")?;
    assert!(matches!(
        inline,
        GroupedStringBuffer::Inline { len: 32, .. }
    ));
    inline.append(&Value::Varchar("中\0".into()), "|")?;
    assert!(matches!(inline, GroupedStringBuffer::Heap(_)));
    assert_eq!(inline.finish()?, Value::Varchar(format!("{boundary}|中\0")));

    let long = "x".repeat(96);
    let mut singleton = GroupedStringBuffer::default();
    singleton.append(&Value::Varchar(long.clone()), "|")?;
    assert!(matches!(singleton, GroupedStringBuffer::Heap(_)));
    assert_eq!(singleton.finish()?, Value::Varchar(long));

    let mut promoted = GroupedStringBuffer::default();
    promoted.append(&Value::Varchar("s".into()), "|")?;
    promoted.append(&Value::Varchar("界".repeat(32)), "|")?;
    promoted.append(&Value::Varchar("t".into()), "|")?;
    assert_eq!(
        promoted.finish()?,
        Value::Varchar(format!("s|{}|t", "界".repeat(32)))
    );

    let mut reused = String::new();
    reused
        .try_reserve_exact(128)
        .map_err(|_| Error::Resource("test string allocation failed".into()))?;
    reused.push_str(&"r".repeat(96));
    let capacity = reused.capacity();
    let mut reused = GroupedStringBuffer::Heap(reused);
    reused.append(&Value::Varchar("z".into()), "|")?;
    let GroupedStringBuffer::Heap(reused) = reused else {
        unreachable!("heap buffer remains promoted");
    };
    assert_eq!(reused.capacity(), capacity);
    assert!(reused.ends_with("|z"));

    assert_eq!(grouped_heap_capacity(0), 1);
    assert_eq!(grouped_heap_capacity(1), 1);
    assert_eq!(grouped_heap_capacity(32), 32);
    assert_eq!(grouped_heap_capacity(33), 64);
    assert_eq!(grouped_heap_capacity(63), 64);
    assert_eq!(grouped_heap_capacity(64), 64);
    assert_eq!(grouped_heap_capacity(65), 128);
    assert_eq!(grouped_heap_capacity(96), 128);
    assert_eq!(grouped_heap_capacity(110), 128);
    assert_eq!(grouped_heap_capacity(129), 256);
    assert_eq!(
        grouped_heap_capacity(usize::MAX / 2 + 1),
        usize::MAX / 2 + 1
    );
    assert_eq!(grouped_heap_capacity(usize::MAX), usize::MAX);
    assert_eq!(grouped_growth_capacity(8, 16), 16);
    assert_eq!(grouped_growth_capacity(100, 127), 127);
    assert_eq!(grouped_growth_capacity(129, 192), 192);
    assert_eq!(grouped_growth_capacity(193, 192), 256);
    assert_eq!(
        grouped_growth_capacity(usize::MAX - 1, usize::MAX / 2 + 1),
        usize::MAX - 1
    );
    assert_eq!(
        grouped_growth_capacity(usize::MAX, usize::MAX - 1),
        usize::MAX
    );
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn grouped_inline_storage_scales_from_empty_to_many_groups() -> Result<()> {
    let query = QueryContext::background();
    let mut grouped = StringAggGroups {
        separator: Some("|".into()),
        buffers: Vec::new(),
    };
    grouped.resize(64, &query)?;
    grouped.resize(8192, &query)?;
    let destinations = (64..8192).collect::<Vec<_>>();
    grouped.update_batch(
        &GroupSelection::new(&destinations, 8192, &query)?,
        &DataChunk::new(
            vec![Vector::constant(
                DataType::Varchar,
                Value::Varchar("é\0".into()),
                destinations.len(),
            )?],
            destinations.len(),
        )?,
        &query,
    )?;
    assert!(
        grouped.buffers[..64]
            .iter()
            .all(|buffer| matches!(buffer, GroupedStringBuffer::Unseen))
    );
    assert!(
        grouped.buffers[64..]
            .iter()
            .all(|buffer| matches!(buffer, GroupedStringBuffer::Inline { len: 3, .. }))
    );
    let values = Box::new(grouped).finish(&query)?;
    assert!(values[..64].iter().all(Value::is_null));
    let expected = Value::Varchar("é\0".into());
    assert!(values[64..].iter().all(|value| value == &expected));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn malformed_shapes_limits_and_cancellation_are_errors() -> Result<()> {
    let query = QueryContext::background();
    let function = bound(Some("|"));
    let mut state = function.create_state(&[DataType::Varchar], query.types())?;
    assert!(matches!(
        state.update_batch(&DataChunk::new(Vec::new(), 1)?, &query),
        Err(Error::Internal(_))
    ));
    let mut state = function.create_state(&[DataType::Varchar], query.types())?;
    assert!(matches!(
        state.update_column(
            &Vector::constant(DataType::BigInt, Value::Integer(1), 1)?,
            &query
        ),
        Err(Error::Internal(_))
    ));
    let mut state = function.create_state(&[DataType::Varchar], query.types())?;
    assert!(matches!(
        state.update(&[Value::Integer(1)], &query),
        Err(Error::Internal(_))
    ));
    let mut grouped = function
        .create_grouped_state(&[DataType::Varchar], query.types())?
        .unwrap();
    grouped.resize(2, &query)?;
    assert!(matches!(grouped.resize(1, &query), Err(Error::Internal(_))));
    let mut grouped = function
        .create_grouped_state(&[DataType::Varchar], query.types())?
        .unwrap();
    grouped.resize(1, &query)?;
    let chunk = DataChunk::new(
        vec![Vector::constant(DataType::Varchar, Value::Null, 1)?],
        1,
    )?;
    assert!(matches!(
        grouped.update_batch(&GroupSelection::new(&[0], 2, &query)?, &chunk, &query),
        Err(Error::Internal(_))
    ));
    for malformed in [
        DataChunk::new(Vec::new(), 1)?,
        DataChunk::new(
            vec![chunk.columns()[0].clone(), chunk.columns()[0].clone()],
            1,
        )?,
        DataChunk::new(
            vec![Vector::constant(DataType::BigInt, Value::Integer(1), 1)?],
            1,
        )?,
    ] {
        let mut grouped = function
            .create_grouped_state(&[DataType::Varchar], query.types())?
            .unwrap();
        grouped.resize(1, &query)?;
        assert!(matches!(
            grouped.update_batch(&GroupSelection::new(&[0], 1, &query)?, &malformed, &query),
            Err(Error::Internal(_))
        ));
    }
    let limited = QueryContext::new(InterruptHandle::default(), None, 2, 1)?;
    let mut grouped = function
        .create_grouped_state(&[DataType::Varchar], query.types())?
        .unwrap();
    assert!(matches!(
        grouped.resize(2, &limited),
        Err(Error::Resource(_))
    ));

    let interrupt = InterruptHandle::default();
    let cancelled = QueryContext::new(interrupt.clone(), None, 2, 100)?;
    interrupt.interrupt();
    let mut state = function.create_state(&[DataType::Varchar], query.types())?;
    assert!(matches!(
        state.update_column(&chunk.columns()[0], &cancelled),
        Err(Error::Interrupted)
    ));
    let mut grouped = function
        .create_grouped_state(&[DataType::Varchar], query.types())?
        .unwrap();
    assert!(matches!(
        grouped.resize(1, &cancelled),
        Err(Error::Interrupted)
    ));
    let mut grouped = function
        .create_grouped_state(&[DataType::Varchar], query.types())?
        .unwrap();
    grouped.resize(1, &query)?;
    assert!(matches!(
        grouped.update_batch(&GroupSelection::new(&[0], 1, &query)?, &chunk, &cancelled),
        Err(Error::Interrupted)
    ));
    let grouped = function
        .create_grouped_state(&[DataType::Varchar], query.types())?
        .unwrap();
    assert!(matches!(
        grouped.finish(&cancelled),
        Err(Error::Interrupted)
    ));
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[test]
fn checked_output_lengths_reject_each_overflow_boundary() -> Result<()> {
    assert_eq!(additional_bytes(0, 0, 0)?, 0);
    assert_eq!(additional_bytes(usize::MAX - 3, 1, 2)?, 3);
    assert!(matches!(
        additional_bytes(0, usize::MAX, 1),
        Err(Error::Resource(_))
    ));
    assert!(matches!(
        additional_bytes(usize::MAX, 0, 1),
        Err(Error::Resource(_))
    ));
    Ok(())
}
