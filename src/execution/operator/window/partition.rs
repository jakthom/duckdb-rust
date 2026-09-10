use super::*;

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn integer_partitions(
    keys: &RowCollection,
    types: &[BoundType],
    context: &ExecutionContext<'_>,
) -> Result<Option<Vec<Vec<usize>>>> {
    let [data_type] = types else { return Ok(None) };
    if data_type.key_representation() != KeyRepresentation::Integer {
        return Ok(None);
    }
    let mut minimum = i128::MAX;
    let mut maximum = i128::MIN;
    for (index, row) in keys.iter().enumerate() {
        if index % 1024 == 0 {
            context.query.check()?;
        }
        if let Value::Integer(value) = row[0] {
            minimum = minimum.min(value);
            maximum = maximum.max(value);
        }
    }
    if maximum < minimum {
        return Ok(Some(vec![(0..keys.len()).collect()]));
    }
    let Some(width) = maximum
        .checked_sub(minimum)
        .and_then(|n| n.checked_add(1))
        .and_then(|n| usize::try_from(n).ok())
        .filter(|&width| width <= 8192 && width <= keys.len().saturating_mul(4))
    else {
        return Ok(None);
    };
    let mut partitions = vec![Vec::new(); width + 1];
    for (index, row) in keys.iter().enumerate() {
        if index % 1024 == 0 {
            context.query.check()?;
        }
        let bucket = match row[0] {
            Value::Integer(value) => (value - minimum) as usize,
            Value::Null => width,
            _ => {
                return Err(Error::Internal(
                    "integer partition key has another representation".into(),
                ));
            }
        };
        partitions[bucket].push(index);
    }
    partitions.retain(|partition| !partition.is_empty());
    Ok(Some(partitions))
}

#[derive(PartialEq, Eq, Hash)]
pub(super) enum Key {
    Integer(Option<i128>),
    Bytes(Vec<u8>),
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn equality_key(
    row: &[Value],
    types: &[BoundType],
    context: &ExecutionContext<'_>,
) -> Result<Key> {
    // PreparedExpression validates logical values before integer identity is used.
    if let [data_type] = types
        && data_type.key_representation() == KeyRepresentation::Integer
    {
        return match &row[0] {
            Value::Integer(value) => Ok(Key::Integer(Some(*value))),
            Value::Null => Ok(Key::Integer(None)),
            _ => Err(Error::Internal(
                "integer window key has another representation".into(),
            )),
        };
    }
    let mut key = Vec::new();
    for (value, data_type) in row.iter().zip(types) {
        data_type.append_key(value, &mut key, context.query)?;
    }
    Ok(Key::Bytes(key))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn sort_indices(
    indices: &[usize],
    keys: &RowCollection,
    types: &[BoundType],
    order: &[OrderExpr],
    algorithm: &dyn SortAlgorithm,
    context: &ExecutionContext<'_>,
) -> Result<Vec<usize>> {
    if order.is_empty() || indices.len() < 2 {
        return Ok(indices.to_vec());
    }
    let schema: Schema = types
        .iter()
        .map(|t| Field::new("key", t.data_type().clone()))
        .chain(std::iter::once(Field::new("row", DataType::BigInt)))
        .collect();
    let order = order
        .iter()
        .enumerate()
        .map(|(index, key)| OrderExpr {
            expression: BoundExpr::column(index, types[index].data_type().clone()),
            ..key.clone()
        })
        .collect::<Vec<_>>();
    // A validated integer ordering capability permits a linear proof that an
    // existing permutation is already sorted; no comparison adapter is bypassed.
    if types
        .iter()
        .all(|t| t.ordering_representation() == OrderingRepresentation::SignedInteger)
    {
        let mut sorted = true;
        for (position, pair) in indices.windows(2).enumerate() {
            if position % 1024 == 0 {
                context.query.check()?;
            }
            for (column, key) in order.iter().enumerate() {
                let (a, b) = (&keys[pair[0]][column], &keys[pair[1]][column]);
                let comparison = match (a, b) {
                    (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
                    (Value::Null, _) => {
                        if key.nulls_first {
                            std::cmp::Ordering::Less
                        } else {
                            std::cmp::Ordering::Greater
                        }
                    }
                    (_, Value::Null) => {
                        if key.nulls_first {
                            std::cmp::Ordering::Greater
                        } else {
                            std::cmp::Ordering::Less
                        }
                    }
                    (Value::Integer(a), Value::Integer(b)) => {
                        if key.descending {
                            b.cmp(a)
                        } else {
                            a.cmp(b)
                        }
                    }
                    _ => {
                        return Err(Error::Internal(
                            "integer ordering returned another representation".into(),
                        ));
                    }
                };
                if comparison.is_gt() {
                    sorted = false;
                    break;
                }
                if comparison.is_lt() {
                    break;
                }
            }
            if !sorted {
                break;
            }
        }
        if sorted {
            return Ok(indices.to_vec());
        }
    }
    let mut position = 0usize;
    let mut source = stream::from_fn(|max_rows| {
        let end = position.saturating_add(max_rows).min(indices.len());
        let mut columns = Vec::with_capacity(types.len() + 1);
        for (column, data_type) in types.iter().enumerate() {
            columns.push(Vector::flat(
                data_type.data_type().clone(),
                indices[position..end]
                    .iter()
                    .map(|&index| keys[index][column].clone())
                    .collect(),
            )?);
        }
        columns.push(Vector::flat(
            DataType::BigInt,
            indices[position..end]
                .iter()
                .map(|&index| Value::Integer(index as i128))
                .collect(),
        )?);
        let count = end - position;
        position = end;
        if count == 0 {
            Ok(None)
        } else {
            DataChunk::new(columns, count).map(Some)
        }
    });
    let rows = algorithm.sort(source.as_mut(), &order, context)?;
    if rows.len() != indices.len() {
        return Err(Error::Internal("window sort changed cardinality".into()));
    }
    let mut seen = vec![false; keys.len()];
    let mut allowed = vec![false; keys.len()];
    for &index in indices {
        allowed[index] = true;
    }
    rows.into_iter()
        .map(|row| {
            if row.len() != schema.len() {
                return Err(Error::Internal("window sort changed row width".into()));
            }
            let index = usize::try_from(row.last().unwrap().as_i128()?)
                .map_err(|_| Error::Internal("invalid window row identity".into()))?;
            if index >= seen.len() || !allowed[index] || std::mem::replace(&mut seen[index], true) {
                return Err(Error::Internal(
                    "window sort returned an invalid permutation".into(),
                ));
            }
            Ok(index)
        })
        .collect()
}
