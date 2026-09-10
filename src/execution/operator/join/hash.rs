use super::*;
use crate::{
    common::{
        Error,
        type_registry::KeyRepresentation,
        vector::{DataChunk, Vector},
    },
    execution::subquery::PreparedExpression,
};
use std::sync::Arc;

enum Index {
    Bytes(HashMap<Vec<u8>, Vec<usize>>),
    Integers(HashMap<i128, Vec<usize>>),
    Dense(i128, Vec<Vec<usize>>),
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Index {
    fn new(keys: &EqualityKeys) -> Self {
        match keys.data_type.key_representation() {
            KeyRepresentation::Integer => Self::Integers(HashMap::new()),
            KeyRepresentation::CanonicalBytes => Self::Bytes(HashMap::new()),
        }
    }
    fn insert(
        &mut self,
        value: &Value,
        row: usize,
        keys: &EqualityKeys,
        context: &ExecutionContext<'_>,
    ) -> Result<()> {
        if value.is_null() {
            return Ok(());
        }
        match self {
            Self::Bytes(index) => {
                let mut key = Vec::new();
                keys.data_type.append_key(value, &mut key, context.query)?;
                index.entry(key).or_default().push(row);
            }
            Self::Integers(index) => {
                index.entry(value.as_i128()?).or_default().push(row);
            }
            Self::Dense(..) => {
                return Err(Error::Internal("insertion into sealed join index".into()));
            }
        }
        Ok(())
    }
    fn seal(&mut self) {
        let Self::Integers(index) = self else { return };
        let Some(minimum) = index.keys().copied().min() else {
            return;
        };
        let maximum = index.keys().copied().max().unwrap();
        let Some(width) = maximum
            .checked_sub(minimum)
            .and_then(|width| width.checked_add(1))
            .and_then(|width| usize::try_from(width).ok())
            .filter(|&width| width <= 8192 && width <= index.len().saturating_mul(8))
        else {
            return;
        };
        let mut dense = vec![Vec::new(); width];
        for (key, rows) in index.drain() {
            dense[(key - minimum) as usize] = rows;
        }
        *self = Self::Dense(minimum, dense);
    }
    fn select(
        &self,
        values: &Vector,
        keys: &EqualityKeys,
        context: &ExecutionContext<'_>,
    ) -> Result<Vec<usize>> {
        let mut selected = Vec::new();
        match self {
            Self::Bytes(index) => {
                keys.data_type
                    .for_each_key(values, context.query, |row, key| {
                        if key.is_some_and(|key| index.contains_key(key)) {
                            selected.push(row);
                        }
                        Ok(())
                    })?
            }
            Self::Integers(index) => {
                membership::visit_integers(values, context.query, |row, key| {
                    if key.is_some_and(|key| index.contains_key(&key)) {
                        selected.push(row);
                    }
                })?
            }
            Self::Dense(minimum, index) => {
                membership::visit_integers(values, context.query, |row, key| {
                    if key
                        .and_then(|key| key.checked_sub(*minimum))
                        .and_then(|offset| usize::try_from(offset).ok())
                        .and_then(|offset| index.get(offset))
                        .is_some_and(|rows| !rows.is_empty())
                    {
                        selected.push(row);
                    }
                })?
            }
        }
        Ok(selected)
    }
    fn probe(
        &self,
        probe: &mut Probe,
        demand: usize,
        outer: bool,
        matched: &mut [bool],
        keys: &EqualityKeys,
        context: &ExecutionContext<'_>,
    ) -> Result<Selection> {
        match self {
            Self::Bytes(index) => probe.select(demand, outer, matched, context, |value| {
                if value.is_null() {
                    return Ok(&[]);
                }
                let mut key = Vec::new();
                keys.data_type.append_key(value, &mut key, context.query)?;
                Ok(index.get(&key).map(Vec::as_slice).unwrap_or(&[]))
            }),
            Self::Integers(index) => probe.select(demand, outer, matched, context, |value| {
                if value.is_null() {
                    return Ok(&[]);
                }
                Ok(index
                    .get(&value.as_i128()?)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]))
            }),
            Self::Dense(minimum, index) => probe.select(demand, outer, matched, context, |value| {
                let Value::Integer(value) = value else {
                    return Ok(&[]);
                };
                Ok(value
                    .checked_sub(*minimum)
                    .and_then(|v| usize::try_from(v).ok())
                    .and_then(|offset| index.get(offset))
                    .map(Vec::as_slice)
                    .unwrap_or(&[]))
            }),
        }
    }
}

struct Probe {
    batch: DataChunk,
    keys: Vector,
    row: usize,
    duplicate: usize,
}

#[derive(Default)]
struct Selection {
    left: Vec<usize>,
    right: Vec<Option<usize>>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Probe {
    fn select<'a>(
        &mut self,
        demand: usize,
        outer: bool,
        matched: &mut [bool],
        context: &ExecutionContext<'_>,
        lookup: impl Fn(&Value) -> Result<&'a [usize]>,
    ) -> Result<Selection> {
        let mut output = Selection::default();
        let start = self.row;
        let mut visit = |value: &Value| -> Result<bool> {
            if self.row.is_multiple_of(1024) {
                context.query.check()?;
            }
            let matches = lookup(value)?;
            if matches.is_empty() {
                if outer {
                    output.left.push(self.row);
                    output.right.push(None);
                }
            } else {
                while self.duplicate < matches.len() && output.left.len() < demand {
                    if output.left.len() % 1024 == 0 {
                        context.query.check()?;
                    }
                    let right = matches[self.duplicate];
                    matched[right] = true;
                    output.left.push(self.row);
                    output.right.push(Some(right));
                    self.duplicate += 1;
                }
                if self.duplicate != matches.len() {
                    return Ok(false);
                }
                self.duplicate = 0;
            }
            self.row += 1;
            Ok(output.left.len() < demand)
        };
        if let Some(values) = self.keys.flat_values() {
            // Dispatch the encoding and key representation once per batch.
            for value in &values[start..] {
                if !visit(value)? {
                    break;
                }
            }
        } else {
            for value in self.keys.slice(start, self.keys.len() - start)?.values() {
                if !visit(value)? {
                    break;
                }
            }
        }
        Ok(output)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Build a bounded right side, then probe checked batches without collecting the
/// left input. Source column views survive output delivery; duplicate matches
/// resume within a row when demand ends. Outer padding retains declared types.
pub(super) fn open<'a>(
    plan: JoinPlan<'a>,
    keys: EqualityKeys,
    context: &'a ExecutionContext<'a>,
) -> Result<Stream<'a>> {
    let mut left = None;
    let mut index = Index::new(&keys);
    let mut columns: Vec<Vec<Value>> = plan.right.schema().iter().map(|_| Vec::new()).collect();
    let mut matched = Vec::new();
    let mut probe: Option<Probe> = None;
    let mut initialized = false;
    let mut exhausted = false;
    let mut unmatched = 0;
    Ok(stream::from_fn(move |max_rows| {
        context.query.check()?;
        if !initialized {
            initialized = true;
            let mut right = stream::open(plan.right, context)?;
            let expression = PreparedExpression::new(&keys.right);
            while let Some(batch) = right.next(context.query.batch_size())? {
                let start = matched.len();
                context
                    .query
                    .check_rows(start.saturating_add(batch.len()))?;
                let values = expression.evaluate_batch(&batch, context)?;
                for (row, value) in values.values().enumerate() {
                    if row % 1024 == 0 {
                        context.query.check()?;
                    }
                    index.insert(value, start + row, &keys, context)?;
                }
                for (output, column) in columns.iter_mut().zip(batch.columns()) {
                    column.append_to(output);
                }
                matched.resize(start + batch.len(), false);
            }
            index.seal();
            left = Some(stream::open(plan.left, context)?);
        }
        loop {
            if probe.is_none() && !exhausted {
                if let Some(batch) = left.as_mut().expect("opened probe input").next(max_rows)? {
                    let values =
                        PreparedExpression::new(&keys.left).evaluate_batch(&batch, context)?;
                    let selected = index.select(&values, &keys, context)?;
                    if selected.is_empty() {
                        if matches!(plan.kind, JoinKind::Left | JoinKind::Full) {
                            let mut output = batch.columns().to_vec();
                            for field in plan.right.schema() {
                                output.push(Vector::constant(
                                    field.data_type.clone(),
                                    Value::Null,
                                    batch.len(),
                                )?);
                            }
                            return DataChunk::new(output, batch.len()).map(Some);
                        }
                        continue;
                    }
                    let (batch, values) = if matches!(plan.kind, JoinKind::Inner | JoinKind::Right)
                    {
                        (batch.select(&selected)?, Arc::new(values).select(selected)?)
                    } else {
                        (batch, values)
                    };
                    probe = Some(Probe {
                        batch,
                        keys: values,
                        row: 0,
                        duplicate: 0,
                    });
                } else {
                    exhausted = true;
                }
            }
            if let Some(current) = &mut probe {
                let Selection {
                    left: selected,
                    right,
                } = index.probe(
                    current,
                    max_rows,
                    matches!(plan.kind, JoinKind::Left | JoinKind::Full),
                    &mut matched,
                    &keys,
                    context,
                )?;
                let result = if selected.is_empty() {
                    None
                } else {
                    let selected = if selected.iter().copied().eq(0..current.batch.len()) {
                        current.batch.clone()
                    } else {
                        current.batch.select(&selected)?
                    };
                    let mut output = selected.columns().to_vec();
                    output.extend(right_columns(&columns, plan.right.schema(), &right)?);
                    Some(DataChunk::new(output, right.len())?)
                };
                if current.row == current.batch.len() {
                    probe = None;
                }
                if result.is_some() {
                    return Ok(result);
                }
                continue;
            }
            if matches!(plan.kind, JoinKind::Right | JoinKind::Full) {
                let mut right = Vec::new();
                while unmatched < matched.len() && right.len() < max_rows {
                    if unmatched % 1024 == 0 {
                        context.query.check()?;
                    }
                    if !matched[unmatched] {
                        right.push(Some(unmatched));
                    }
                    unmatched += 1;
                }
                if !right.is_empty() {
                    let mut output = plan
                        .left
                        .schema()
                        .iter()
                        .map(|field| {
                            Vector::constant(field.data_type.clone(), Value::Null, right.len())
                        })
                        .collect::<Result<Vec<_>>>()?;
                    output.extend(right_columns(&columns, plan.right.schema(), &right)?);
                    return DataChunk::new(output, right.len()).map(Some);
                }
            }
            return Ok(None);
        }
    }))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn right_columns(
    columns: &[Vec<Value>],
    schema: &Schema,
    selected: &[Option<usize>],
) -> Result<Vec<Vector>> {
    columns
        .iter()
        .zip(schema)
        .map(|(column, field)| {
            if selected.iter().all(Option::is_none) {
                return Vector::constant(field.data_type.clone(), Value::Null, selected.len());
            }
            Vector::flat(
                field.data_type.clone(),
                selected
                    .iter()
                    .map(|row| row.map(|row| column[row].clone()).unwrap_or(Value::Null))
                    .collect(),
            )
        })
        .collect()
}
