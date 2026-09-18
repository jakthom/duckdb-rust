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
