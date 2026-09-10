use super::*;
use crate::{
    common::{
        type_registry::KeyRepresentation,
        vector::{DataChunk, Vector},
    },
    execution::subquery::PreparedExpression,
};
use std::sync::Arc;

enum Index {
    Bytes(HashMap<Vec<u8>, Vec<usize>>),
    Integers(HashMap<i128, Matches>),
    Dense(i128, Vec<Matches>),
}

#[derive(Clone, Default)]
enum Matches {
    #[default]
    Empty,
    One(usize),
    Many(Vec<usize>),
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Matches {
    fn push(&mut self, row: usize) {
        match self {
            Self::Empty => *self = Self::One(row),
            Self::One(first) => *self = Self::Many(vec![*first, row]),
            Self::Many(rows) => rows.push(row),
        }
    }
    fn as_slice(&self) -> &[usize] {
        match self {
            Self::Empty => &[],
            Self::One(row) => std::slice::from_ref(row),
            Self::Many(rows) => rows,
        }
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Index {
    fn prefilter_useful(&self) -> bool {
        match self {
            // Keep the whole-batch byte-key error boundary. A small compact
            // build also benefits from rejecting nonmatches in a tight loop
            // before the duplicate/outer-output state machine is entered.
            Self::Bytes(_) => true,
            Self::Integers(keys) => keys.len() <= 8192,
            Self::Dense(_, slots) => slots.len() <= 8192,
        }
    }
    // A bounded dense build avoids allocating a hash bucket for every distinct
    // key. Range growth is decided once per batch; sparse or full-width keys
    // retain the ordinary map. This is equality addressing, not SQL ordering.
    fn prepare_batch(
        &mut self,
        values: &Vector,
        rows: usize,
        keys: &EqualityKeys,
        context: &ExecutionContext<'_>,
    ) -> Result<()> {
        let empty = matches!(self, Self::Integers(index) if index.is_empty());
        if !empty && !matches!(self, Self::Dense(..)) {
            return Ok(());
        }
        let (mut minimum, mut maximum) = match self {
            Self::Dense(minimum, slots) => (*minimum, *minimum + (slots.len() - 1) as i128),
            _ => (i128::MAX, i128::MIN),
        };
        for (i, value) in values.values().enumerate() {
            if i % 1024 == 0 {
                context.query.check()?;
            }
            if let Some(value) = keys.data_type.key_representation().integer_key(value)? {
                minimum = minimum.min(value);
                maximum = maximum.max(value);
            }
        }
        if minimum > maximum {
            return Ok(());
        }
        let width = maximum
            .checked_sub(minimum)
            .and_then(|v| usize::try_from(v).ok())
            .and_then(|v| v.checked_add(1))
            .filter(|&v| v <= 262_144 && v <= rows.saturating_mul(4));
        if let Some(width) = width {
            match self {
                Self::Dense(old_minimum, slots) if *old_minimum == minimum => {
                    slots.resize(width, Matches::Empty)
                }
                Self::Dense(old_minimum, slots) => {
                    let offset = (*old_minimum - minimum) as usize;
                    let mut next = vec![Matches::Empty; width];
                    for (target, source) in next[offset..].iter_mut().zip(slots.drain(..)) {
                        *target = source;
                    }
                    *slots = next;
                    *old_minimum = minimum;
                }
                _ => *self = Self::Dense(minimum, vec![Matches::Empty; width]),
            }
        } else if let Self::Dense(minimum, slots) = self {
            let mut index = HashMap::new();
            for (offset, rows) in slots.drain(..).enumerate() {
                if offset % 1024 == 0 {
                    context.query.check()?;
                }
                if !rows.as_slice().is_empty() {
                    index.insert(*minimum + offset as i128, rows);
                }
            }
            *self = Self::Integers(index);
        }
        Ok(())
    }
    fn new(keys: &EqualityKeys) -> Self {
        match keys.data_type.key_representation() {
            KeyRepresentation::Integer | KeyRepresentation::NumericCoefficient => {
                Self::Integers(HashMap::new())
            }
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
                let key = keys
                    .data_type
                    .key_representation()
                    .integer_key(value)?
                    .expect("non-NULL key");
                index.entry(key).or_default().push(row);
            }
            Self::Dense(minimum, slots) => {
                let key = keys
                    .data_type
                    .key_representation()
                    .integer_key(value)?
                    .expect("non-NULL key");
                slots[(key - *minimum) as usize].push(row);
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
        let mut dense = vec![Matches::Empty; width];
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
            Self::Integers(index) => membership::visit_integers(
                values,
                keys.data_type.key_representation(),
                context.query,
                |row, key| {
                    if key.is_some_and(|key| index.contains_key(&key)) {
                        selected.push(row);
                    }
                },
            )?,
            Self::Dense(minimum, index) => membership::visit_integers(
                values,
                keys.data_type.key_representation(),
                context.query,
                |row, key| {
                    if key
                        // Construction proves the dense inclusive maximum
                        // does not wrap. Full-width outliers cannot alias an
                        // offset below this bounded width.
                        .map(|key| key.wrapping_sub(*minimum) as u128)
                        .filter(|&offset| offset < index.len() as u128)
                        .map(|offset| &index[offset as usize])
                        .is_some_and(|rows| !rows.as_slice().is_empty())
                    {
                        selected.push(row);
                    }
                },
            )?,
        }
        Ok(selected)
    }
    fn probe(
        &self,
        probe: &mut Probe,
        demand: usize,
        outer: bool,
        matched: Option<&mut [bool]>,
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
                    .get(
                        &keys
                            .data_type
                            .key_representation()
                            .integer_key(value)?
                            .expect("non-NULL key"),
                    )
                    .map(Matches::as_slice)
                    .unwrap_or(&[]))
            }),
            Self::Dense(minimum, index) => probe.select(demand, outer, matched, context, |value| {
                let Some(value) = keys.data_type.key_representation().integer_key(value)? else {
                    return Ok(&[]);
                };
                Ok(Some(value.wrapping_sub(*minimum) as u128)
                    .filter(|&offset| offset < index.len() as u128)
                    .map(|offset| &index[offset as usize])
                    .map(Matches::as_slice)
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
    right: Vec<usize>,
}
// A valid build row is strictly below its cardinality, which fits usize.
// This sentinel can never alias a row, including at the largest row budget.
const NO_MATCH: usize = usize::MAX;
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Probe {
    fn select<'a>(
        &mut self,
        demand: usize,
        outer: bool,
        mut matched: Option<&mut [bool]>,
        context: &ExecutionContext<'_>,
        lookup: impl Fn(&Value) -> Result<&'a [usize]>,
    ) -> Result<Selection> {
        let capacity = demand.min(self.keys.len() - self.row);
        let mut output = Selection {
            left: Vec::with_capacity(capacity),
            right: Vec::with_capacity(capacity),
        };
        let start = self.row;
        let mut visit = |value: &Value| -> Result<bool> {
            if self.row.is_multiple_of(1024) {
                context.query.check()?;
            }
            let matches = lookup(value)?;
            if matches.is_empty() {
                if outer {
                    output.left.push(self.row);
                    output.right.push(NO_MATCH);
                }
            } else {
                while self.duplicate < matches.len() && output.left.len() < demand {
                    if output.left.len().is_multiple_of(1024) {
                        context.query.check()?;
                    }
                    let right = matches[self.duplicate];
                    if let Some(matched) = &mut matched {
                        matched[right] = true;
                    }
                    output.left.push(self.row);
                    output.right.push(right);
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
    let mut columns: Vec<Arc<Vector>> = Vec::new();
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
            let mut input_columns: Vec<Vec<Vector>> =
                plan.right.schema().iter().map(|_| Vec::new()).collect();
            let expression = PreparedExpression::new(&keys.right);
            while let Some(batch) = right.next(context.query.batch_size())? {
                let start = matched.len();
                context
                    .query
                    .check_rows(start.saturating_add(batch.len()))?;
                let values = expression.evaluate_batch(&batch, context)?;
                index.prepare_batch(&values, start + batch.len(), &keys, context)?;
                for (row, value) in values.values().enumerate() {
                    if row % 1024 == 0 {
                        context.query.check()?;
                    }
                    index.insert(value, start + row, &keys, context)?;
                }
                for (output, column) in input_columns.iter_mut().zip(batch.columns()) {
                    output.push(column.clone());
                }
                matched.resize(start + batch.len(), false);
            }
            index.seal();
            for (field, input) in plan.right.schema().iter().zip(input_columns) {
                context.query.check()?;
                columns.push(Arc::new(Vector::concatenate(
                    field.data_type.clone(),
                    &input,
                )?));
            }
            left = Some(stream::open(plan.left, context)?);
        }
        loop {
            if probe.is_none() && !exhausted {
                if let Some(batch) = left.as_mut().expect("opened probe input").next(max_rows)? {
                    let values =
                        PreparedExpression::new(&keys.left).evaluate_batch(&batch, context)?;
                    // Large compact builds probe directly. Small builds retain
                    // preselection, including whole unmatched outer batches.
                    let selected = if index.prefilter_useful() {
                        Some(index.select(&values, &keys, context)?)
                    } else {
                        None
                    };
                    if selected.as_ref().is_some_and(Vec::is_empty) {
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
                    let (batch, values) = if let Some(selected) = selected
                        && selected.len() != batch.len()
                        && matches!(plan.kind, JoinKind::Inner | JoinKind::Right)
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
                    matches!(plan.kind, JoinKind::Right | JoinKind::Full)
                        .then_some(matched.as_mut_slice()),
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
                        right.push(unmatched);
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
    columns: &[Arc<Vector>],
    schema: &Schema,
    selected: &[usize],
) -> Result<Vec<Vector>> {
    if columns.is_empty() {
        return Ok(Vec::new());
    }
    if !selected.contains(&NO_MATCH) {
        let count = columns.first().map_or(0, |column| column.len());
        return Ok(DataChunk::new(
            columns
                .iter()
                .map(|column| column.as_ref().clone())
                .collect(),
            count,
        )?
        .select(selected)?
        .columns()
        .to_vec());
    }
    columns
        .iter()
        .zip(schema)
        .map(|(column, field)| {
            if selected.iter().all(|&row| row == NO_MATCH) {
                return Vector::constant(field.data_type.clone(), Value::Null, selected.len());
            }
            Vector::flat(
                field.data_type.clone(),
                selected
                    .iter()
                    .map(|&row| {
                        if row == NO_MATCH {
                            Value::Null
                        } else {
                            column.get(row).expect("validated build index").clone()
                        }
                    })
                    .collect(),
            )
        })
        .collect()
}
