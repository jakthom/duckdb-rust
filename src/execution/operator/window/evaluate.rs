use super::*;

type PartitionBuilder<'a> = dyn Fn(&PreparedWindow, &WindowExpression, &ExecutionContext<'_>) -> Result<Vec<Vec<usize>>>
    + 'a;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn evaluate(
    input: &mut dyn BatchStream,
    schema: &Schema,
    windows: &[WindowExpression],
    context: &ExecutionContext<'_>,
    partition: &PartitionBuilder<'_>,
) -> Result<DataChunk> {
    let mut batches = Vec::new();
    let mut count = 0usize;
    while let Some(batch) = input.next(context.query.batch_size())? {
        count = count.saturating_add(batch.len());
        context.query.check_rows(count)?;
        batches.push(batch);
    }
    let mut columns = Vec::new();
    for (index, field) in schema.iter().enumerate() {
        let pieces = batches
            .iter()
            .map(|batch| batch.columns()[index].clone())
            .collect::<Vec<_>>();
        context.query.check()?;
        columns.push(Vector::concatenate(field.data_type.clone(), &pieces)?);
    }
    for window in windows {
        let data = PreparedWindow::new(&batches, window, context)?;
        let argument_types = window
            .arguments
            .iter()
            .map(|e| e.data_type.clone())
            .collect::<Vec<_>>();
        let result_type = context.query.types().bind(&window.data_type)?;
        let mut values = vec![Value::Null; count];
        // Retain flat signed output: complete result materialization is cheaper
        // without dictionary gathering. Numeric conversions downstream justify
        // compact unsigned/decimal output in this increment.
        let mut dictionary = (window.data_type.is_unsigned_integer()
            || window.data_type.is_decimal())
        .then(Vec::new);
        let mut selection = vec![0; if dictionary.is_some() { count } else { 0 }];
        for indices in partition(&data, window, context)? {
            let n = indices.len();
            if n == 0 {
                continue;
            }
            let args = WindowRows::new(&data.arguments, &indices)?;
            let filters = indices
                .iter()
                .map(|&index| data.filter[index])
                .collect::<Vec<_>>();
            let mut groups = Vec::new();
            let peers = if data.order_types.is_empty() {
                groups.push(0..n);
                WindowBounds::uniform(0..n, n)?
            } else {
                let mut peers = vec![0..n; n];
                let mut start = 0;
                while start < n {
                    if start % 1024 == 0 {
                        context.query.check()?;
                    }
                    let key =
                        equality_key(&data.order[indices[start]], &data.order_types, context)?;
                    let mut end = start + 1;
                    while end < n
                        && equality_key(&data.order[indices[end]], &data.order_types, context)?
                            == key
                    {
                        end += 1;
                    }
                    peers[start..end].fill(start..end);
                    groups.push(start..end);
                    start = end;
                }
                WindowBounds::rows(peers)?
            };
            let mut group = 0;
            let frames = if window.frame.start == FrameBound::UnboundedPreceding
                && (window.frame.end == FrameBound::UnboundedFollowing
                    || (data.order_types.is_empty()
                        && window.frame.units != FrameUnits::Rows
                        && window.frame.end == FrameBound::CurrentRow))
            {
                WindowBounds::uniform(0..n, n)?
            } else {
                let mut frames = Vec::with_capacity(n);
                for index in 0..n {
                    if index % 1024 == 0 {
                        context.query.check()?;
                    }
                    while index >= groups[group].end {
                        group += 1;
                    }
                    let position = FramePosition {
                        index,
                        group,
                        groups: &groups,
                        peers: &peers,
                    };
                    let start = position.boundary(window.frame.start, false, window.frame.units)?;
                    let end = position.boundary(window.frame.end, true, window.frame.units)?;
                    frames.push(start.min(end)..end);
                }
                WindowBounds::rows(frames)?
            };
            let output = window.function.evaluate(
                &WindowInput {
                    arguments: args,
                    argument_types: &argument_types,
                    peers: &peers,
                    frames: &frames,
                    filter: &filters,
                    options: window.options,
                },
                context.query,
            )?;
            if output.len() != n {
                return Err(Error::Internal(
                    "window function returned wrong cardinality".into(),
                ));
            }
            if let Some(entries) = &mut dictionary {
                if output.iter().all(|value| value == &output[0]) {
                    for &index in &indices {
                        selection[index] = entries.len();
                    }
                    entries.push(output[0].clone());
                } else {
                    dictionary = None;
                    selection.clear();
                }
            }
            for (index, value) in indices.into_iter().zip(output) {
                if result_type.requires_logical_validation() {
                    result_type
                        .validate(&value, context.query)
                        .map_err(|e| match e {
                            Error::Conversion(_) => Error::Internal(
                                "window function returned invalid logical value".into(),
                            ),
                            other => other,
                        })?;
                }
                values[index] = value;
            }
        }
        let column =
            Vector::flat(window.data_type.clone(), values).map_err(|error| match error {
                Error::Conversion(_) => {
                    Error::Internal("window function returned invalid physical value".into())
                }
                other => other,
            })?;
        columns.push(
            if let Some(entries) =
                dictionary.filter(|entries| entries.len() <= count / 4 && count > 0)
            {
                // Validate all function outputs before compacting. Only exact
                // physical equality permits reuse; floating-point SQL equality
                // would incorrectly merge signed-zero payloads.
                Arc::new(Vector::flat(window.data_type.clone(), entries)?).select(selection)?
            } else {
                column
            },
        );
    }
    context.query.check()?;
    DataChunk::new(columns, count)
}

struct FramePosition<'a> {
    index: usize,
    group: usize,
    groups: &'a [Range<usize>],
    peers: &'a WindowBounds,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl FramePosition<'_> {
    fn boundary(&self, bound: FrameBound, end: bool, units: FrameUnits) -> Result<usize> {
        let Self {
            index,
            group,
            groups,
            peers,
        } = *self;
        let count = peers.len();
        match bound {
            FrameBound::UnboundedPreceding => return Ok(0),
            FrameBound::UnboundedFollowing => return Ok(count),
            FrameBound::CurrentRow if units != FrameUnits::Rows => {
                return Ok(if end {
                    peers[index].end
                } else {
                    peers[index].start
                });
            }
            _ => (),
        }
        if units == FrameUnits::Range {
            return Err(Error::Unsupported("RANGE frames with value offsets".into()));
        }
        let current = if units == FrameUnits::Groups {
            group
        } else {
            index
        };
        let position = match bound {
            FrameBound::CurrentRow => current as i128,
            FrameBound::Preceding(offset) => current as i128 - offset as i128,
            FrameBound::Following(offset) => current as i128 + offset as i128,
            _ => unreachable!(),
        };
        if units == FrameUnits::Rows {
            return Ok((position + i128::from(end)).clamp(0, count as i128) as usize);
        }
        Ok(if position < 0 {
            0
        } else if position >= groups.len() as i128 {
            count
        } else if end {
            groups[position as usize].end
        } else {
            groups[position as usize].start
        })
    }
}

#[cfg(kani)]
mod verification {
    use super::*;

    #[kani::proof]
    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    fn kani_rows_frame_offsets_clip_to_partition() {
        let count: usize = kani::any();
        let index: usize = kani::any();
        let offset: usize = kani::any();
        let end: bool = kani::any();
        let following: bool = kani::any();
        // Production calls boundary only for an existing partition row.
        kani::assume(index < count);
        let peers = WindowBounds::uniform(0..count, count).unwrap();
        let position = FramePosition {
            index,
            group: 0,
            groups: &[], // ROWS does not consult peer-group positions.
            peers: &peers,
        };
        let bound = if following {
            FrameBound::Following(offset)
        } else {
            FrameBound::Preceding(offset)
        };
        // Saturating unsigned arithmetic is independent of the signed formula
        // used in production. Widen before adding the exclusive-end adjustment.
        let base = index as u128 + u128::from(end);
        let expected = if following {
            base + offset as u128
        } else {
            base.saturating_sub(offset as u128)
        }
        .min(count as u128) as usize;
        let actual = position.boundary(bound, end, FrameUnits::Rows).unwrap();
        assert_eq!(actual, expected);
        assert!(actual <= count);
    }
}
