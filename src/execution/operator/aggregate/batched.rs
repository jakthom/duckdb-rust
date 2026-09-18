//! Column grouping for total expressions and opt-in aggregate states.
mod index;
mod modifiers;
use super::*;
use crate::{
    DataType, Value,
    common::type_registry::{BoundType, KeyRepresentation, OrderingRepresentation},
    common::vector::{NullableBigIntView, SignedI64At, Vector},
    execution::subquery::PreparedExpression,
    function::{
        AggregateModifierStrategy, AggregateState, OrderedAggregateStrategy,
        grouped::{GroupSelection, GroupedAggregateState},
    },
    planner::{
        aggregation::AggregateOutput,
        expression::{BinaryOp, UnaryOp},
    },
};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, hash_map::Entry},
    sync::Arc,
};

enum OrderedAccumulator {
    State(Box<dyn GroupedAggregateState>),
    Candidate {
        strategy: OrderedAggregateStrategy,
        order: Vec<crate::planner::logical::OrderExpr>,
        types: Vec<BoundType>,
        values: Vec<Option<(Value, CandidateOrder)>>,
    },
}

enum CandidateOrder {
    One(Value),
    /// A one-key signed integer candidate keeps its nullable physical
    /// coefficient. This is intentionally separate from `One`: nullable
    /// BIGINT vectors use flat `Value` storage, while all-valid vectors use
    /// signed lanes, and neither needs an owned scalar in the hot loop.
    SignedI64(Option<i64>),
    Many(Vec<Value>),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// None is returned only before consuming input. Unknown expression effects,
/// DISTINCT/FILTER, broader keys and functions retain the ordered row driver.
pub(super) fn try_run(
    input: &mut dyn BatchStream,
    aggregation: &Aggregation,
    context: &ExecutionContext<'_>,
) -> Result<Option<AggregateResult>> {
    if let Some(rows) = try_filtered_signed_ungrouped(input, aggregation, context)? {
        return Ok(Some(AggregateResult::Rows(rows)));
    }
    if let Some(rows) = try_distinct_modifiers(input, aggregation, context)? {
        return Ok(Some(AggregateResult::Rows(rows)));
    }
    if let Some(rows) = try_ordered_candidates(input, aggregation, context)? {
        return Ok(Some(AggregateResult::Rows(rows)));
    }
    if let Some(rows) = modifiers::try_run(input, aggregation, context)? {
        return Ok(Some(AggregateResult::Rows(rows)));
    }
    if let Some(rows) = try_ungrouped(input, aggregation, context)? {
        return Ok(Some(AggregateResult::Rows(rows)));
    }
    // Keep the existing ungrouped column kernel, including its overflow proof.
    if aggregation.groups.is_empty() && aggregation.sets.len() == 1 {
        return Ok(None);
    }
    if aggregation.sets.iter().any(|s| s.indices().len() > 2)
        || aggregation.groups.iter().any(|e| !e.is_pure_and_total())
        || aggregation.functions().any(|f| {
            f.distinct
                || f.filter.is_some()
                || !f.order_by.is_empty()
                || f.arguments.iter().any(|e| !e.is_pure_and_total())
        })
    {
        return Ok(None);
    }
    let representations = aggregation
        .groups
        .iter()
        .map(|group| {
            context
                .query
                .types()
                .bind(&group.data_type)
                .map(|data_type| data_type.key_representation())
        })
        .collect::<Result<Vec<_>>>()?;
    if representations.iter().any(|key| !key.has_integer_keys()) {
        return Ok(None);
    }
    aggregation.validate_metadata(context.query)?;
    let functions = aggregation.functions().collect::<Vec<_>>();
    let mut accumulators = Vec::with_capacity(functions.len());
    for function in &functions {
        let arguments = function
            .arguments
            .iter()
            .map(|e| e.data_type.clone())
            .collect::<Vec<_>>();
        let Some(state) = function
            .function
            .create_grouped_state(&arguments, context.query.types())?
        else {
            return Ok(None);
        };
        if state.group_count() != 0 {
            return Err(Error::Internal("new aggregate state is not empty".into()));
        }
        accumulators.push(state);
    }
    let expressions = aggregation
        .groups
        .iter()
        .map(PreparedExpression::new)
        .collect::<Vec<_>>();
    let arguments = functions
        .iter()
        .map(|f| {
            f.arguments
                .iter()
                .map(PreparedExpression::new)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut groups = ColumnarGroups::new(aggregation.groups.len())?;
    let mut indices = aggregation
        .sets
        .iter()
        .enumerate()
        .map(|(set_index, set)| {
            let mut index = index::IntegerIndex::default();
            if set.is_empty() {
                context.query.check_rows(groups.len() + 1)?;
                context.query.check()?;
                index.set_empty(groups.len());
                groups.push_empty(set_index)?;
            }
            Ok(index)
        })
        .collect::<Result<Vec<_>>>()?;
    while let Some(batch) = input.next(context.query.batch_size())? {
        let columns = expressions
            .iter()
            .map(|e| e.evaluate_batch(&batch, context))
            .collect::<Result<Vec<_>>>()?;
        let inputs = arguments
            .iter()
            .map(|args| {
                DataChunk::new(
                    args.iter()
                        .map(|e| e.evaluate_batch(&batch, context))
                        .collect::<Result<_>>()?,
                    batch.len(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        for (set_index, set) in aggregation.sets.iter().enumerate() {
            let keys = set
                .indices()
                .iter()
                .map(|&index| &columns[index])
                .collect::<Vec<_>>();
            let key_representations = set
                .indices()
                .iter()
                .map(|&index| representations[index])
                .collect::<Vec<_>>();
            let destinations = indices[set_index].locate(
                &keys,
                &key_representations,
                batch.len(),
                context.query,
                |row| {
                    context.query.check_rows(groups.len() + 1)?;
                    if groups.len() % 1024 == 0 {
                        context.query.check()?;
                    }
                    let index = groups.len();
                    groups.push(&columns, set, set_index, row)?;
                    Ok(index)
                },
            )?;
            let destinations = GroupSelection::new(&destinations, groups.len(), context.query)?;
            for (state, arguments) in accumulators.iter_mut().zip(&inputs) {
                state.resize(groups.len(), context.query)?;
                if state.group_count() != groups.len() {
                    return Err(Error::Internal(
                        "aggregate resize returned wrong group count".into(),
                    ));
                }
                state.update_batch(&destinations, arguments, context.query)?;
            }
        }
    }
    let values = accumulators
        .into_iter()
        .map(|mut state| {
            state.resize(groups.len(), context.query)?;
            if state.group_count() != groups.len() {
                return Err(Error::Internal(
                    "aggregate resize returned wrong group count".into(),
                ));
            }
            let values = state.finish(context.query)?;
            if values.len() != groups.len() {
                return Err(Error::Internal(
                    "aggregate result has wrong group count".into(),
                ));
            }
            Ok(values)
        })
        .collect::<Result<Vec<_>>>()?;
    let count = groups.len();
    let (group_values, set_indices) = groups.into_parts();
    let width = aggregation
        .groups
        .len()
        .checked_add(aggregation.outputs.len())
        .ok_or_else(|| Error::Resource("aggregate result width exceeds usize".into()))?;
    let mut columns = Vec::new();
    columns
        .try_reserve_exact(width)
        .map_err(|_| Error::Resource("aggregate result column allocation failed".into()))?;
    for (expression, values) in aggregation.groups.iter().zip(group_values) {
        context.query.check()?;
        columns.push(Vector::flat(expression.data_type.clone(), values)?);
    }
    let mut values = values.into_iter();
    for output in &aggregation.outputs {
        match output {
            AggregateOutput::Function(function) => {
                let values = values.next().ok_or_else(|| {
                    Error::Internal("aggregate result column count differs".into())
                })?;
                context.query.check()?;
                columns.push(Vector::flat(function.data_type.clone(), values)?);
            }
            AggregateOutput::Grouping(indices) => {
                let mut masks = Vec::new();
                masks.try_reserve_exact(count).map_err(|_| {
                    Error::Resource("GROUPING result column allocation failed".into())
                })?;
                for (group, &set_index) in set_indices.iter().enumerate() {
                    if group % 1024 == 0 {
                        context.query.check()?;
                    }
                    let set = aggregation.sets.get(set_index).ok_or_else(|| {
                        Error::Internal("aggregate group has invalid grouping set".into())
                    })?;
                    masks.push(Value::Integer(indices.iter().fold(0, |mask, &index| {
                        (mask << 1) | i128::from(!set.contains(index))
                    })));
                }
                columns.push(Vector::flat(DataType::BigInt, masks)?);
            }
        }
    }
    if values.next().is_some() {
        return Err(Error::Internal(
            "aggregate result column count differs".into(),
        ));
    }
    context.query.check()?;
    Ok(Some(AggregateResult::Columns(DataChunk::new(
        columns, count,
    )?)))
}

/// Group identity is row-oriented only while locating a new key. Retaining the
/// discovered values by output column avoids one allocation per group and a
/// second row-to-column transpose at publication.
struct ColumnarGroups {
    values: Vec<Vec<Value>>,
    set_indices: Vec<usize>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ColumnarGroups {
    fn new(width: usize) -> Result<Self> {
        let mut values = Vec::new();
        values
            .try_reserve_exact(width)
            .map_err(|_| Error::Resource("aggregate group column allocation failed".into()))?;
        values.resize_with(width, Vec::new);
        Ok(Self {
            values,
            set_indices: Vec::new(),
        })
    }

    fn len(&self) -> usize {
        self.set_indices.len()
    }

    fn reserve_one(&mut self) -> Result<()> {
        self.set_indices
            .try_reserve(1)
            .map_err(|_| Error::Resource("aggregate group allocation failed".into()))?;
        for values in &mut self.values {
            values
                .try_reserve(1)
                .map_err(|_| Error::Resource("aggregate group allocation failed".into()))?;
        }
        Ok(())
    }

    fn push_empty(&mut self, set_index: usize) -> Result<()> {
        self.reserve_one()?;
        for values in &mut self.values {
            values.push(Value::Null);
        }
        self.set_indices.push(set_index);
        Ok(())
    }

    fn push(
        &mut self,
        columns: &[Vector],
        set: &crate::planner::aggregation::GroupingSet,
        set_index: usize,
        row: usize,
    ) -> Result<()> {
        if columns.len() != self.values.len() {
            return Err(Error::Internal(
                "aggregate group column count differs".into(),
            ));
        }
        self.reserve_one()?;
        for (ordinal, (values, column)) in self.values.iter_mut().zip(columns).enumerate() {
            values.push(if set.contains(ordinal) {
                column
                    .get(row)
                    .ok_or_else(|| Error::Internal("aggregate group row outside input".into()))?
            } else {
                Value::Null
            });
        }
        self.set_indices.push(set_index);
        Ok(())
    }

    fn into_parts(self) -> (Vec<Vec<Value>>, Vec<usize>) {
        (self.values, self.set_indices)
    }
}

enum FilteredSignedState {
    Count {
        value: i128,
        argument: Option<usize>,
    },
    Sum {
        value: i128,
        seen: bool,
        argument: usize,
    },
}

enum SignedColumnView<'a> {
    Flat(&'a [i64]),
    Nullable(NullableBigIntView<'a>),
    DictionaryFlat(&'a [i64], &'a [usize]),
    DictionaryNullable(NullableBigIntView<'a>, &'a [usize]),
    Generic(&'a Vector),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> SignedColumnView<'a> {
    #[inline(always)]
    fn new(column: &'a Vector) -> Self {
        if let Some(values) = column.flat_nullable_bigints() {
            Self::Nullable(values)
        } else if let Some(values) = column.flat_bigints() {
            Self::Flat(values)
        } else if let Some((parent, selection)) = column.dictionary() {
            if let Some(values) = parent.flat_nullable_bigints() {
                Self::DictionaryNullable(values, selection)
            } else if let Some(values) = parent.flat_bigints() {
                Self::DictionaryFlat(values, selection)
            } else {
                Self::Generic(column)
            }
        } else {
            Self::Generic(column)
        }
    }

    #[inline(always)]
    fn at(&self, row: usize) -> SignedI64At {
        let value = match self {
            Self::Flat(values) => values.get(row).copied().map(Some),
            Self::Nullable(values) => values.value(row),
            Self::DictionaryFlat(values, selection) => selection
                .get(row)
                .and_then(|&source| values.get(source))
                .copied()
                .map(Some),
            Self::DictionaryNullable(values, selection) => {
                selection.get(row).and_then(|&source| values.value(source))
            }
            Self::Generic(column) => return signed_column_at(column, row),
        };
        match value {
            Some(Some(value)) => SignedI64At::Value(value),
            Some(None) => SignedI64At::Null,
            None => SignedI64At::Unsupported,
        }
    }
}

enum BooleanColumnView<'a> {
    Flat(&'a [Value]),
    Constant(Option<bool>),
    DictionaryFlat(&'a [Value], &'a [usize]),
    Generic(&'a Vector),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> BooleanColumnView<'a> {
    #[inline(always)]
    fn new(column: &'a Vector) -> Self {
        if let Some(values) = column.flat_values() {
            Self::Flat(values)
        } else if let Some(value) = column.constant_value() {
            Self::Constant(boolean_value(value))
        } else if let Some((parent, selection)) = column.dictionary()
            && let Some(values) = parent.flat_values()
        {
            Self::DictionaryFlat(values, selection)
        } else {
            Self::Generic(column)
        }
    }

    #[inline(always)]
    fn at(&self, row: usize) -> Option<Option<bool>> {
        match self {
            Self::Flat(values) => values.get(row).map(boolean_value),
            Self::Constant(value) => Some(*value),
            Self::DictionaryFlat(values, selection) => selection
                .get(row)
                .and_then(|&source| values.get(source))
                .map(boolean_value),
            Self::Generic(column) => column.boolean_at(row),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline(always)]
fn boolean_value(value: &Value) -> Option<bool> {
    match value {
        Value::Boolean(value) => Some(*value),
        Value::Null => None,
        _ => None,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Fuse a shared stored Boolean FILTER with explicitly opted-in COUNT/SUM
/// signed lanes. Every broader expression and every registered callback keeps
/// the ordinary state path.
fn try_filtered_signed_ungrouped(
    input: &mut dyn BatchStream,
    aggregation: &Aggregation,
    context: &ExecutionContext<'_>,
) -> Result<Option<Vec<Row>>> {
    if !aggregation.groups.is_empty()
        || aggregation.sets.len() != 1
        || !aggregation.sets[0].is_empty()
        || aggregation
            .outputs
            .iter()
            .any(|output| !matches!(output, AggregateOutput::Function(_)))
    {
        return Ok(None);
    }
    let functions = aggregation.functions().collect::<Vec<_>>();
    let Some(first_filter) = functions
        .first()
        .and_then(|function| function.filter.as_ref())
    else {
        return Ok(None);
    };
    let ExprKind::Column(filter_column) = first_filter.kind else {
        return Ok(None);
    };
    let mut states = Vec::with_capacity(functions.len());
    for function in &functions {
        if function.distinct
            || !function.order_by.is_empty()
            || !matches!(function.filter.as_ref().map(|filter| &filter.kind), Some(ExprKind::Column(column)) if *column == filter_column)
        {
            return Ok(None);
        }
        let argument_types = function
            .arguments
            .iter()
            .map(|argument| argument.data_type.clone())
            .collect::<Vec<_>>();
        match function.function.modifier_strategy(&argument_types) {
            AggregateModifierStrategy::DistinctCount => {
                let argument = match function.arguments.as_slice() {
                    [] => None,
                    [
                        BoundExpr {
                            kind: ExprKind::Column(column),
                            data_type,
                        },
                    ] if signed_i64_type(data_type) => Some(*column),
                    _ => return Ok(None),
                };
                states.push(FilteredSignedState::Count { value: 0, argument });
            }
            AggregateModifierStrategy::FilteredSum => {
                let [
                    BoundExpr {
                        kind: ExprKind::Column(argument),
                        data_type,
                    },
                ] = function.arguments.as_slice()
                else {
                    return Ok(None);
                };
                if !signed_i64_type(data_type) {
                    return Ok(None);
                }
                states.push(FilteredSignedState::Sum {
                    value: 0,
                    seen: false,
                    argument: *argument,
                });
            }
            _ => return Ok(None),
        }
    }
    aggregation.validate_metadata(context.query)?;
    while let Some(batch) = input.next(context.query.batch_size())? {
        let filter = batch
            .columns()
            .get(filter_column)
            .ok_or_else(|| Error::Internal("filter column outside aggregate input".into()))?;
        let filter = BooleanColumnView::new(filter);
        let arguments = states
            .iter()
            .map(|state| match state {
                FilteredSignedState::Count {
                    argument: Some(argument),
                    ..
                }
                | FilteredSignedState::Sum { argument, .. } => {
                    Some(SignedColumnView::new(&batch.columns()[*argument]))
                }
                FilteredSignedState::Count { argument: None, .. } => None,
            })
            .collect::<Vec<_>>();
        if let (
            [
                FilteredSignedState::Count {
                    value: count,
                    argument: None,
                },
                FilteredSignedState::Sum {
                    value: sum,
                    seen,
                    argument: _,
                },
            ],
            [None, Some(argument)],
        ) = (states.as_mut_slice(), arguments.as_slice())
            && let Some((batch_count, batch_sum, batch_seen)) =
                filtered_count_sum_batch(&filter, argument, batch.len(), context.query)?
        {
            *count = count
                .checked_add(batch_count as i128)
                .ok_or_else(|| Error::Execution("aggregate count overflow".into()))?;
            *sum = sum
                .checked_add(batch_sum)
                .ok_or_else(|| Error::Execution("sum overflow".into()))?;
            *seen |= batch_seen;
            continue;
        }
        for row in 0..batch.len() {
            if row % 1024 == 0 {
                context.query.check()?;
            }
            let selected = match filter.at(row) {
                Some(Some(value)) => value,
                Some(None) => false,
                None => return Err(Error::Internal("filter column is not Boolean".into())),
            };
            if !selected {
                continue;
            }
            for (state, argument_view) in states.iter_mut().zip(&arguments) {
                match state {
                    FilteredSignedState::Count { value, argument } => {
                        let present = match argument {
                            None => true,
                            Some(_) => !matches!(
                                argument_view.as_ref().expect("count argument view").at(row),
                                SignedI64At::Null
                            ),
                        };
                        if present {
                            *value = value.checked_add(1).ok_or_else(|| {
                                Error::Execution("aggregate count overflow".into())
                            })?;
                        }
                    }
                    FilteredSignedState::Sum {
                        value,
                        seen,
                        argument: _,
                    } => {
                        let input = match argument_view.as_ref().expect("sum argument view").at(row)
                        {
                            SignedI64At::Value(value) => Some(i128::from(value)),
                            SignedI64At::Null => None,
                            SignedI64At::Unsupported => {
                                return Err(Error::Internal(
                                    "signed aggregate argument lost physical representation".into(),
                                ));
                            }
                        };
                        if let Some(input) = input {
                            *value = value
                                .checked_add(input)
                                .ok_or_else(|| Error::Execution("sum overflow".into()))?;
                            *seen = true;
                        }
                    }
                }
            }
        }
    }
    let row = states
        .into_iter()
        .map(|state| match state {
            FilteredSignedState::Count { value, .. } => Value::Integer(value),
            FilteredSignedState::Sum { value, seen, .. } => {
                if seen {
                    Value::Integer(value)
                } else {
                    Value::Null
                }
            }
        })
        .collect();
    Ok(Some(vec![row]))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn filtered_count_sum_batch(
    filter: &BooleanColumnView<'_>,
    argument: &SignedColumnView<'_>,
    len: usize,
    query: &crate::parallel::QueryContext,
) -> Result<Option<(usize, i128, bool)>> {
    let (
        BooleanColumnView::DictionaryFlat(filter_values, filter_selection),
        SignedColumnView::DictionaryNullable(argument_values, argument_selection),
    ) = (filter, argument)
    else {
        return Ok(None);
    };
    if filter_selection.len() < len || argument_selection.len() < len {
        return Err(Error::Internal(
            "filtered aggregate dictionary is shorter than its batch".into(),
        ));
    }
    let mut count = 0usize;
    let mut sum = 0i128;
    let mut seen = false;
    for start in (0..len).step_by(1024) {
        query.check()?;
        for row in start..len.min(start + 1024) {
            let selected = match filter_values.get(filter_selection[row]) {
                Some(Value::Boolean(value)) => *value,
                Some(Value::Null) => false,
                _ => {
                    return Err(Error::Internal(
                        "aggregate filter column is not Boolean".into(),
                    ));
                }
            };
            if !selected {
                continue;
            }
            count += 1;
            if let Some(Some(value)) = argument_values.value(argument_selection[row]) {
                sum += i128::from(value);
                seen = true;
            }
        }
    }
    Ok(Some((count, sum, seen)))
}

struct DistinctAccumulator {
    function: Arc<dyn crate::function::AggregateFunction>,
    argument_type: DataType,
    strategy: AggregateModifierStrategy,
    argument_column: usize,
    filter: ModifierFilter,
    order: Option<crate::planner::logical::OrderExpr>,
    states: Vec<Box<dyn AggregateState>>,
    retained: Vec<Vec<Option<i64>>>,
}

struct DistinctDomain {
    argument_column: usize,
    filter: ModifierFilter,
    consumers: Vec<usize>,
    seen: Vec<SignedDistinctSet>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ModifierFilter {
    All,
    BooleanColumn(usize),
    BooleanAndArgumentPresent(usize),
}

/// The admitted integer modifiers commonly have a compact domain. Keep the
/// first 256-value window inline and promote collision-safely when it is not.
enum SignedDistinctSet {
    Empty,
    NullOnly,
    Dense {
        base: i64,
        bits: [u64; 4],
        null: bool,
        len: usize,
    },
    Sparse(HashSet<Option<i64>>),
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SignedDistinctSet {
    #[inline(always)]
    fn insert(&mut self, value: Option<i64>) -> bool {
        match self {
            Self::Empty => {
                *self = match value {
                    None => Self::NullOnly,
                    Some(value) => Self::dense(value, false),
                };
                true
            }
            Self::NullOnly => match value {
                None => false,
                Some(value) => {
                    *self = Self::dense(value, true);
                    true
                }
            },
            Self::Dense {
                base,
                bits,
                null,
                len,
            } => match value {
                None if *null => false,
                None => {
                    *null = true;
                    *len += 1;
                    true
                }
                Some(value) => {
                    let offset = i128::from(value) - i128::from(*base);
                    if (0..256).contains(&offset) {
                        let offset = offset as usize;
                        let word = &mut bits[offset / 64];
                        let mask = 1_u64 << (offset % 64);
                        if *word & mask != 0 {
                            false
                        } else {
                            *word |= mask;
                            *len += 1;
                            true
                        }
                    } else {
                        let mut sparse = HashSet::with_capacity(len.saturating_mul(2));
                        if *null {
                            sparse.insert(None);
                        }
                        for (word_index, mut word) in bits.iter().copied().enumerate() {
                            while word != 0 {
                                let bit = word.trailing_zeros() as usize;
                                let offset = word_index * 64 + bit;
                                let existing = base
                                    .checked_add(offset as i64)
                                    .expect("dense signed window contains inserted values");
                                sparse.insert(Some(existing));
                                word &= word - 1;
                            }
                        }
                        let inserted = sparse.insert(Some(value));
                        *self = Self::Sparse(sparse);
                        inserted
                    }
                }
            },
            Self::Sparse(values) => values.insert(value),
        }
    }

    #[inline(always)]
    fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::NullOnly => 1,
            Self::Dense { len, .. } => *len,
            Self::Sparse(values) => values.len(),
        }
    }

    #[inline(always)]
    fn dense(value: i64, null: bool) -> Self {
        let base = value.saturating_sub(128);
        let offset = (i128::from(value) - i128::from(base)) as usize;
        let mut bits = [0; 4];
        bits[offset / 64] |= 1_u64 << (offset % 64);
        Self::Dense {
            base,
            bits,
            null,
            len: 1 + usize::from(null),
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Physical signed-integer DISTINCT composition for explicitly opted-in
/// built-ins. Filters and group destinations are evaluated before arguments;
/// any broader expression, callback, type or ordering shape falls back before
/// input is consumed.
fn try_distinct_modifiers(
    input: &mut dyn BatchStream,
    aggregation: &Aggregation,
    context: &ExecutionContext<'_>,
) -> Result<Option<Vec<Row>>> {
    if aggregation.sets.len() != 1
        || aggregation.sets[0].indices().len() > 2
        || aggregation
            .groups
            .iter()
            .any(|group| !group.is_pure_and_total())
        || aggregation
            .outputs
            .iter()
            .any(|output| !matches!(output, AggregateOutput::Function(_)))
    {
        return Ok(None);
    }
    let functions = aggregation.functions().collect::<Vec<_>>();
    if functions.is_empty() {
        return Ok(None);
    }
    let mut strategies = Vec::with_capacity(functions.len());
    for function in &functions {
        let [argument] = function.arguments.as_slice() else {
            return Ok(None);
        };
        let ExprKind::Column(argument_column) = argument.kind else {
            return Ok(None);
        };
        let Some(filter) = modifier_filter(function.filter.as_ref(), argument_column) else {
            return Ok(None);
        };
        if !function.distinct
            || !argument.is_pure_and_total()
            || !signed_i64_type(&argument.data_type)
        {
            return Ok(None);
        }
        let strategy = function
            .function
            .modifier_strategy(std::slice::from_ref(&argument.data_type));
        // BufferedTotal is a stronger capability than this narrow signed LIST
        // adapter requires. Reuse the selected state through the established
        // stable signed path when DISTINCT and ORDER BY name the same column;
        // broader typed shapes remain with the generic buffered route.
        let strategy = if matches!(
            strategy,
            AggregateModifierStrategy::BufferedTotal
                | AggregateModifierStrategy::BufferedOwnedTotal
        ) {
            let selected = context.query.types().bind(&argument.data_type)?;
            if selected.key_representation() != KeyRepresentation::Integer
                || selected.ordering_representation() != OrderingRepresentation::SignedInteger
                || selected.requires_logical_validation()
            {
                return Ok(None);
            }
            AggregateModifierStrategy::DistinctList
        } else {
            strategy
        };
        let order = match strategy {
            AggregateModifierStrategy::DistinctCount if function.order_by.is_empty() => None,
            AggregateModifierStrategy::DistinctList
            | AggregateModifierStrategy::DistinctFirst
            | AggregateModifierStrategy::DistinctLast => {
                let [order] = function.order_by.as_slice() else {
                    return Ok(None);
                };
                let (ExprKind::Column(argument_column), ExprKind::Column(order_column)) =
                    (&argument.kind, &order.expression.kind)
                else {
                    return Ok(None);
                };
                if argument_column != order_column
                    || order.expression.data_type != argument.data_type
                {
                    return Ok(None);
                }
                Some(order.clone())
            }
            _ => return Ok(None),
        };
        strategies.push((strategy, order, argument_column, filter));
    }
    let representations = aggregation
        .groups
        .iter()
        .map(|group| {
            context
                .query
                .types()
                .bind(&group.data_type)
                .map(|data_type| data_type.key_representation())
        })
        .collect::<Result<Vec<_>>>()?;
    if representations.iter().any(|key| !key.has_integer_keys()) {
        return Ok(None);
    }
    aggregation.validate_metadata(context.query)?;
    let group_expressions = aggregation
        .groups
        .iter()
        .map(PreparedExpression::new)
        .collect::<Vec<_>>();
    let mut accumulators = functions
        .iter()
        .zip(strategies)
        .map(
            |(function, (strategy, order, argument_column, filter))| DistinctAccumulator {
                function: function.function.clone(),
                argument_type: function.arguments[0].data_type.clone(),
                strategy,
                argument_column,
                filter,
                order,
                states: Vec::new(),
                retained: Vec::new(),
            },
        )
        .collect::<Vec<_>>();
    let mut domains = Vec::<DistinctDomain>::new();
    for (consumer, accumulator) in accumulators.iter().enumerate() {
        if let Some(domain) = domains.iter_mut().find(|domain| {
            domain.argument_column == accumulator.argument_column
                && domain.filter == accumulator.filter
        }) {
            domain.consumers.push(consumer);
        } else {
            domains.push(DistinctDomain {
                argument_column: accumulator.argument_column,
                filter: accumulator.filter,
                consumers: vec![consumer],
                seen: Vec::new(),
            });
        }
    }
    let set = &aggregation.sets[0];
    let mut groups: Vec<(Row, usize)> = Vec::new();
    let mut index = index::IntegerIndex::default();
    if set.is_empty() {
        context.query.check_rows(1)?;
        index.set_empty(0);
        groups.push((vec![Value::Null; aggregation.groups.len()], 0));
    }
    let mut ordered_units = 0usize;
    while let Some(batch) = input.next(context.query.batch_size())? {
        let columns = group_expressions
            .iter()
            .map(|expression| expression.evaluate_batch(&batch, context))
            .collect::<Result<Vec<_>>>()?;
        let keys = set
            .indices()
            .iter()
            .map(|&ordinal| &columns[ordinal])
            .collect::<Vec<_>>();
        let key_representations = set
            .indices()
            .iter()
            .map(|&ordinal| representations[ordinal])
            .collect::<Vec<_>>();
        let destinations = if set.is_empty() {
            None
        } else {
            Some(index.locate(
                &keys,
                &key_representations,
                batch.len(),
                context.query,
                |row| {
                    context.query.check_rows(groups.len() + 1 + ordered_units)?;
                    let ordinal = groups.len();
                    let values = columns
                        .iter()
                        .enumerate()
                        .map(|(index, column)| {
                            if set.contains(index) {
                                column.get(row).expect("validated grouping column")
                            } else {
                                Value::Null
                            }
                        })
                        .collect();
                    groups.push((values, 0));
                    Ok(ordinal)
                },
            )?)
        };
        for accumulator in &mut accumulators {
            while accumulator.states.len() < groups.len() {
                accumulator.states.push(accumulator.function.create_state(
                    std::slice::from_ref(&accumulator.argument_type),
                    context.query.types(),
                )?);
                accumulator.retained.push(Vec::new());
            }
        }
        for domain in &mut domains {
            while domain.seen.len() < groups.len() {
                domain.seen.push(SignedDistinctSet::Empty);
            }
            let values = batch
                .columns()
                .get(domain.argument_column)
                .ok_or_else(|| Error::Internal("aggregate argument column outside input".into()))?;
            let values = SignedColumnView::new(values);
            let filter = match domain.filter {
                ModifierFilter::All => None,
                ModifierFilter::BooleanColumn(column)
                | ModifierFilter::BooleanAndArgumentPresent(column) => Some(
                    batch
                        .columns()
                        .get(column)
                        .map(BooleanColumnView::new)
                        .ok_or_else(|| {
                            Error::Internal("aggregate filter column outside input".into())
                        })?,
                ),
            };
            for source_row in 0..batch.len() {
                if source_row % 1024 == 0 {
                    context.query.check()?;
                }
                let group = destinations
                    .as_ref()
                    .map_or(0, |destinations| destinations[source_row]);
                let value = match values.at(source_row) {
                    SignedI64At::Value(value) => Some(value),
                    SignedI64At::Null => None,
                    SignedI64At::Unsupported => {
                        return Err(Error::Internal(
                            "signed DISTINCT argument lost physical representation".into(),
                        ));
                    }
                };
                if !modifier_filter_selected(domain.filter, filter.as_ref(), source_row, value)? {
                    continue;
                }
                if !domain.seen[group].insert(value) {
                    continue;
                }
                context.query.check_rows(domain.seen[group].len())?;
                for &consumer in &domain.consumers {
                    let accumulator = &mut accumulators[consumer];
                    match accumulator.strategy {
                        AggregateModifierStrategy::DistinctCount => accumulator.states[group]
                            .update(&[signed_value(value)], context.query)?,
                        AggregateModifierStrategy::DistinctList => {
                            let next = ordered_units.checked_add(3).ok_or_else(|| {
                                Error::Resource("ordered aggregate row count overflow".into())
                            })?;
                            context.query.check_rows(groups.len() + next)?;
                            ordered_units = next;
                            accumulator.retained[group].push(value);
                        }
                        AggregateModifierStrategy::DistinctFirst
                        | AggregateModifierStrategy::DistinctLast => {
                            let replace =
                                accumulator.retained[group].first().is_none_or(|current| {
                                    let comparison = compare_optional_signed(
                                        value,
                                        *current,
                                        accumulator.order.as_ref().expect("ordered strategy"),
                                    );
                                    comparison == Ordering::Less
                                        && accumulator.strategy
                                            == AggregateModifierStrategy::DistinctFirst
                                        || comparison == Ordering::Greater
                                            && accumulator.strategy
                                                == AggregateModifierStrategy::DistinctLast
                                });
                            if replace {
                                if accumulator.retained[group].is_empty() {
                                    context.query.check_rows(groups.len() + ordered_units + 1)?;
                                    ordered_units += 1;
                                    accumulator.retained[group].push(value);
                                } else {
                                    accumulator.retained[group][0] = value;
                                }
                            }
                        }
                        AggregateModifierStrategy::Generic
                        | AggregateModifierStrategy::BufferedTotal
                        | AggregateModifierStrategy::BufferedOwnedTotal
                        | AggregateModifierStrategy::FilteredSum => {
                            unreachable!("admission rejected")
                        }
                    }
                }
            }
        }
    }
    let mut values = Vec::with_capacity(accumulators.len());
    for mut accumulator in accumulators {
        while accumulator.states.len() < groups.len() {
            accumulator.states.push(accumulator.function.create_state(
                std::slice::from_ref(&accumulator.argument_type),
                context.query.types(),
            )?);
            accumulator.retained.push(Vec::new());
        }
        for group in 0..groups.len() {
            if accumulator.strategy == AggregateModifierStrategy::DistinctList {
                let order = accumulator.order.as_ref().expect("ordered list");
                accumulator.retained[group]
                    .sort_by(|left, right| compare_optional_signed(*left, *right, order));
            }
            for value in accumulator.retained[group].drain(..) {
                accumulator.states[group].update(&[signed_value(value)], context.query)?;
            }
        }
        values.push(
            accumulator
                .states
                .into_iter()
                .map(|state| state.finish())
                .collect::<Result<Vec<_>>>()?,
        );
    }
    let rows = groups
        .into_iter()
        .enumerate()
        .map(|(group, (mut row, _))| {
            context.query.check()?;
            row.extend(values.iter().map(|function| function[group].clone()));
            Ok(row)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(rows))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn modifier_filter(filter: Option<&BoundExpr>, argument: usize) -> Option<ModifierFilter> {
    let Some(filter) = filter else {
        return Some(ModifierFilter::All);
    };
    match &filter.kind {
        ExprKind::Column(column) => Some(ModifierFilter::BooleanColumn(*column)),
        ExprKind::Binary(BinaryOp::And, left, right, _)
            if matches!(
                &right.kind,
                ExprKind::Unary(UnaryOp::IsNotNull, value)
                    if matches!(&value.kind, ExprKind::Column(column) if *column == argument)
            ) =>
        {
            let ExprKind::Column(boolean) = left.kind else {
                return None;
            };
            Some(ModifierFilter::BooleanAndArgumentPresent(boolean))
        }
        _ => None,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline(always)]
fn modifier_filter_selected(
    filter: ModifierFilter,
    column: Option<&BooleanColumnView<'_>>,
    row: usize,
    argument: Option<i64>,
) -> Result<bool> {
    match filter {
        ModifierFilter::All => return Ok(true),
        ModifierFilter::BooleanColumn(_) | ModifierFilter::BooleanAndArgumentPresent(_) => {}
    }
    if matches!(filter, ModifierFilter::BooleanAndArgumentPresent(_)) && argument.is_none() {
        return Ok(false);
    }
    match column.and_then(|column| column.at(row)) {
        Some(Some(value)) => Ok(value),
        Some(None) => Ok(false),
        None => Err(Error::Internal(
            "aggregate filter column is not Boolean".into(),
        )),
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline(always)]
fn signed_column_at(column: &Vector, row: usize) -> SignedI64At {
    if let Some(values) = column.flat_nullable_bigints() {
        return match values.value(row) {
            Some(Some(value)) => SignedI64At::Value(value),
            Some(None) => SignedI64At::Null,
            None => SignedI64At::Unsupported,
        };
    }
    if let Some(values) = column.flat_bigints() {
        return values
            .get(row)
            .copied()
            .map_or(SignedI64At::Unsupported, SignedI64At::Value);
    }
    if let Some((parent, selection)) = column.dictionary() {
        return selection
            .get(row)
            .copied()
            .map_or(SignedI64At::Unsupported, |row| {
                signed_column_at(parent, row)
            });
    }
    column.signed_i64_at(row)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn signed_value(value: Option<i64>) -> Value {
    value.map_or(Value::Null, |value| Value::Integer(i128::from(value)))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn compare_optional_signed(
    left: Option<i64>,
    right: Option<i64>,
    order: &crate::planner::logical::OrderExpr,
) -> Ordering {
    compare_signed_integer_order(left, right, order)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Batched, bounded retention for FIRST/LAST argument ORDER BY. This route is
/// intentionally narrower than the scalar ordered driver: all expressions
/// must be pure and total, arguments are unary, and only integer
/// grouping keys accepted by IntegerIndex may enter. Everything observable or
/// broader returns None before input is consumed and keeps the generic path.
fn try_ordered_candidates(
    input: &mut dyn BatchStream,
    aggregation: &Aggregation,
    context: &ExecutionContext<'_>,
) -> Result<Option<Vec<Row>>> {
    if aggregation.sets.iter().any(|set| set.indices().len() > 2)
        || aggregation
            .groups
            .iter()
            .any(|group| !group.is_pure_and_total())
        || aggregation
            .outputs
            .iter()
            .any(|output| !matches!(output, AggregateOutput::Function(_)))
    {
        return Ok(None);
    }
    let functions = aggregation.functions().collect::<Vec<_>>();
    if !functions
        .iter()
        .any(|function| !function.order_by.is_empty())
        || functions.iter().any(|function| {
            let strategy = function.function.ordered_strategy(
                &function
                    .arguments
                    .iter()
                    .map(|arg| arg.data_type.clone())
                    .collect::<Vec<_>>(),
            );
            let candidate = matches!(
                strategy,
                OrderedAggregateStrategy::First | OrderedAggregateStrategy::Last
            ) && !function.distinct
                && function.filter.is_none()
                && function.arguments.len() == 1
                && !function.order_by.is_empty()
                && function.arguments.iter().all(BoundExpr::is_pure_and_total)
                && function
                    .order_by
                    .iter()
                    .all(|order| order.expression.is_pure_and_total());
            !candidate
                && (function.distinct
                    || function.filter.is_some()
                    || !function.order_by.is_empty()
                    || function
                        .arguments
                        .iter()
                        .any(|arg| !arg.is_pure_and_total()))
        })
    {
        return Ok(None);
    }
    let representations = aggregation
        .groups
        .iter()
        .map(|group| {
            context
                .query
                .types()
                .bind(&group.data_type)
                .map(|ty| ty.key_representation())
        })
        .collect::<Result<Vec<_>>>()?;
    if representations.iter().any(|key| !key.has_integer_keys()) {
        return Ok(None);
    }
    aggregation.validate_metadata(context.query)?;
    let group_expressions = aggregation
        .groups
        .iter()
        .map(PreparedExpression::new)
        .collect::<Vec<_>>();
    let argument_expressions = functions
        .iter()
        .map(|function| {
            function
                .arguments
                .iter()
                .map(PreparedExpression::new)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let order_expressions = functions
        .iter()
        .map(|function| {
            function
                .order_by
                .iter()
                .map(|order| PreparedExpression::new(&order.expression))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut accumulators = Vec::with_capacity(functions.len());
    for function in &functions {
        let arguments = function
            .arguments
            .iter()
            .map(|arg| arg.data_type.clone())
            .collect::<Vec<_>>();
        match function.function.ordered_strategy(&arguments) {
            OrderedAggregateStrategy::First | OrderedAggregateStrategy::Last
                if !function.distinct
                    && function.filter.is_none()
                    && function.arguments.len() == 1
                    && !function.order_by.is_empty() =>
            {
                accumulators.push(OrderedAccumulator::Candidate {
                    strategy: function.function.ordered_strategy(&arguments),
                    order: function.order_by.clone(),
                    types: function
                        .order_by
                        .iter()
                        .map(|order| context.query.types().bind(&order.expression.data_type))
                        .collect::<Result<Vec<_>>>()?,
                    values: Vec::new(),
                });
            }
            _ => {
                let Some(state) = function
                    .function
                    .create_grouped_state(&arguments, context.query.types())?
                else {
                    return Ok(None);
                };
                accumulators.push(OrderedAccumulator::State(state));
            }
        }
    }
    let mut groups: Vec<(Row, usize)> = Vec::new();
    let mut indices = aggregation
        .sets
        .iter()
        .enumerate()
        .map(|(set_index, set)| {
            let mut index = index::IntegerIndex::default();
            if set.is_empty() {
                context.query.check_rows(groups.len() + 1)?;
                index.set_empty(groups.len());
                groups.push((vec![Value::Null; aggregation.groups.len()], set_index));
            }
            Ok(index)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut candidate_units = 0usize;
    let needs_group_selection = accumulators
        .iter()
        .any(|accumulator| matches!(accumulator, OrderedAccumulator::State(_)));
    while let Some(batch) = input.next(context.query.batch_size())? {
        let columns = group_expressions
            .iter()
            .map(|expr| expr.evaluate_batch(&batch, context))
            .collect::<Result<Vec<_>>>()?;
        let inputs = argument_expressions
            .iter()
            .map(|expressions| {
                DataChunk::new(
                    expressions
                        .iter()
                        .map(|expr| expr.evaluate_batch(&batch, context))
                        .collect::<Result<_>>()?,
                    batch.len(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let orders = order_expressions
            .iter()
            .map(|expressions| {
                DataChunk::new(
                    expressions
                        .iter()
                        .map(|expr| expr.evaluate_batch(&batch, context))
                        .collect::<Result<_>>()?,
                    batch.len(),
                )
            })
            .collect::<Result<Vec<_>>>()?;
        for (set_index, set) in aggregation.sets.iter().enumerate() {
            let keys = set
                .indices()
                .iter()
                .map(|&index| &columns[index])
                .collect::<Vec<_>>();
            let key_representations = set
                .indices()
                .iter()
                .map(|&index| representations[index])
                .collect::<Vec<_>>();
            let destinations = indices[set_index].locate(
                &keys,
                &key_representations,
                batch.len(),
                context.query,
                |row| {
                    context
                        .query
                        .check_rows(groups.len() + 1 + candidate_units)?;
                    let index = groups.len();
                    let mut values = Vec::with_capacity(aggregation.groups.len() + functions.len());
                    for (i, column) in columns.iter().enumerate() {
                        values.push(if set.contains(i) {
                            column.get(row).expect("validated grouping column")
                        } else {
                            Value::Null
                        });
                    }
                    groups.push((values, set_index));
                    Ok(index)
                },
            )?;
            let selection = needs_group_selection
                .then(|| GroupSelection::new(&destinations, groups.len(), context.query))
                .transpose()?;
            for (function_index, accumulator) in accumulators.iter_mut().enumerate() {
                match accumulator {
                    OrderedAccumulator::State(state) => {
                        let selection = selection
                            .as_ref()
                            .expect("ordinary grouped state requires validated destinations");
                        state.resize(groups.len(), context.query)?;
                        state.update_batch(selection, &inputs[function_index], context.query)?;
                    }
                    OrderedAccumulator::Candidate {
                        strategy,
                        order,
                        types,
                        values,
                    } => {
                        values.resize_with(groups.len(), || None);
                        for (row, &group) in destinations.iter().enumerate() {
                            if row % 1024 == 0 {
                                context.query.check()?;
                            }
                            let replaces = match &values[group] {
                                None => true,
                                Some((_, current)) => match compare_candidate_order(
                                    &orders[function_index],
                                    row,
                                    current,
                                    order,
                                    types,
                                    context,
                                )? {
                                    Ordering::Less => *strategy == OrderedAggregateStrategy::First,
                                    Ordering::Greater => {
                                        *strategy == OrderedAggregateStrategy::Last
                                    }
                                    Ordering::Equal => false,
                                },
                            };
                            if replaces {
                                if values[group].is_none() {
                                    context.query.check_rows(
                                        groups
                                            .len()
                                            .saturating_add(candidate_units)
                                            .saturating_add(1),
                                    )?;
                                    candidate_units += 1;
                                }
                                let argument = inputs[function_index].columns()[0]
                                    .value(row)
                                    .expect("validated candidate argument");
                                let columns = orders[function_index].columns();
                                let key = if let [column] = columns {
                                    if order.len() == 1
                                        && signed_i64_type(&order[0].expression.data_type)
                                        && let Some(value) = signed_integer_order(column, row)
                                    {
                                        CandidateOrder::SignedI64(value)
                                    } else {
                                        CandidateOrder::One(
                                            column.value(row).expect("validated candidate order"),
                                        )
                                    }
                                } else {
                                    CandidateOrder::Many(
                                        columns
                                            .iter()
                                            .map(|column| {
                                                column
                                                    .value(row)
                                                    .expect("validated candidate order")
                                            })
                                            .collect(),
                                    )
                                };
                                values[group] = Some((argument, key));
                            }
                        }
                    }
                }
            }
        }
    }
    for accumulator in accumulators {
        match accumulator {
            OrderedAccumulator::State(mut state) => {
                state.resize(groups.len(), context.query)?;
                let values = state.finish(context.query)?;
                if values.len() != groups.len() {
                    return Err(Error::Internal(
                        "ordered aggregate result has wrong group count".into(),
                    ));
                }
                for ((row, _), value) in groups.iter_mut().zip(values) {
                    row.push(value);
                }
            }
            OrderedAccumulator::Candidate {
                strategy,
                mut values,
                ..
            } => {
                // The candidate capability is admitted only for built-in
                // FIRST/LAST with one already bound argument. Their state is
                // precisely the retained argument, or NULL for an empty
                // group; constructing thousands of one-row states here would
                // add no semantics and dominates high-cardinality groups.
                debug_assert!(matches!(
                    strategy,
                    OrderedAggregateStrategy::First | OrderedAggregateStrategy::Last
                ));
                values.resize_with(groups.len(), || None);
                if values.len() != groups.len() {
                    return Err(Error::Internal(
                        "ordered candidate result has wrong group count".into(),
                    ));
                }
                for ((row, _), candidate) in groups.iter_mut().zip(values) {
                    row.push(candidate.map_or(Value::Null, |(argument, _)| argument));
                }
            }
        }
    }
    let mut rows = Vec::with_capacity(groups.len());
    for (group, (row, _)) in groups.into_iter().enumerate() {
        if group % 1024 == 0 {
            context.query.check()?;
        }
        rows.push(row);
    }
    context.query.check()?;
    Ok(Some(rows))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Compare one vector row with a retained candidate without constructing a
/// temporary Row. The vector seam yields owned scalar values for encoded
/// lanes; candidate allocation occurs only after a strict replacement.
#[inline(always)]
fn compare_candidate_order(
    columns: &DataChunk,
    row: usize,
    current: &CandidateOrder,
    order: &[crate::planner::logical::OrderExpr],
    types: &[BoundType],
    context: &ExecutionContext<'_>,
) -> Result<Ordering> {
    if let CandidateOrder::SignedI64(current) = current {
        let [column] = columns.columns() else {
            unreachable!("signed candidates have exactly one order key");
        };
        let [order] = order else {
            unreachable!("signed candidates have exactly one order expression");
        };
        let value = signed_integer_order(column, row)
            .expect("signed candidate order keeps a stable physical representation");
        return Ok(compare_signed_integer_order(value, *current, order));
    }
    let current: &[Value] = match current {
        CandidateOrder::One(value) => std::slice::from_ref(value),
        CandidateOrder::SignedI64(_) => unreachable!("handled above"),
        CandidateOrder::Many(values) => values,
    };
    for (((column, current), order), data_type) in
        columns.columns().iter().zip(current).zip(order).zip(types)
    {
        let value = column.value(row).expect("validated candidate order");
        let comparison = match (value.is_null(), current.is_null()) {
            (true, true) => Ordering::Equal,
            (true, false) => {
                if order.nulls_first {
                    Ordering::Less
                } else {
                    Ordering::Greater
                }
            }
            (false, true) => {
                if order.nulls_first {
                    Ordering::Greater
                } else {
                    Ordering::Less
                }
            }
            (false, false) => {
                let comparison = data_type.compare(&value, current, context.query)?;
                if order.descending {
                    comparison.reverse()
                } else {
                    comparison
                }
            }
        };
        if comparison != Ordering::Equal {
            return Ok(comparison);
        }
    }
    Ok(Ordering::Equal)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Return a nullable signed coefficient without materializing a `Value`.
/// Encoded vectors resolve through the checked coefficient accessor, retaining
/// their logical NULL and selection semantics without owned scalar values.
#[inline(always)]
fn signed_integer_order(column: &Vector, row: usize) -> Option<Option<i64>> {
    match column.signed_i64_at(row) {
        SignedI64At::Value(value) => Some(Some(value)),
        SignedI64At::Null => Some(None),
        SignedI64At::Unsupported => None,
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn signed_i64_type(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::TinyInt | DataType::SmallInt | DataType::Integer | DataType::BigInt
    )
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
#[inline(always)]
fn compare_signed_integer_order(
    value: Option<i64>,
    current: Option<i64>,
    order: &crate::planner::logical::OrderExpr,
) -> Ordering {
    match (value, current) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => {
            if order.nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (Some(_), None) => {
            if order.nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Some(value), Some(current)) => {
            let comparison = value.cmp(&current);
            if order.descending {
                comparison.reverse()
            } else {
                comparison
            }
        }
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Evaluate effect-free aggregate arguments as columns, then retain aggregate
/// updates in exact row/output order. If speculative column evaluation finds a
/// data error, replay only that untouched batch through the scalar expression
/// order so the reported first error remains source-compatible.
fn try_ungrouped(
    input: &mut dyn BatchStream,
    aggregation: &Aggregation,
    context: &ExecutionContext<'_>,
) -> Result<Option<Vec<Row>>> {
    if !aggregation.groups.is_empty()
        || aggregation.sets.len() != 1
        || !aggregation.sets[0].is_empty()
        || aggregation
            .outputs
            .iter()
            .any(|output| !matches!(output, AggregateOutput::Function(_)))
    {
        return Ok(None);
    }
    let functions = aggregation.functions().collect::<Vec<_>>();
    if functions.iter().any(|function| {
        function.distinct
            || !function.order_by.is_empty()
            || match &function.filter {
                Some(filter) => {
                    !filter.is_pure_and_total()
                        || function
                            .arguments
                            .iter()
                            .any(|argument| !argument.is_pure_and_total())
                }
                None => function
                    .arguments
                    .iter()
                    .any(|argument| !argument.is_effect_free()),
            }
    }) {
        return Ok(None);
    }
    if let [function] = functions.as_slice()
        && let [argument] = function.arguments.as_slice()
        && matches!(argument.kind, ExprKind::Column(_))
    {
        // The established single-aggregate route forwards a physical column
        // directly to `update_column`; rebuilding its prepared-expression and
        // chunk wrappers here only adds dispatch to the common SUM path.
        return Ok(None);
    }
    aggregation.validate_metadata(context.query)?;
    let expressions = functions
        .iter()
        .map(|function| {
            function
                .arguments
                .iter()
                .map(PreparedExpression::new)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let filters = functions
        .iter()
        .map(|function| function.filter.as_ref().map(PreparedExpression::new))
        .collect::<Vec<_>>();
    let filter_columns = functions
        .iter()
        .map(
            |function| match function.filter.as_ref().map(|filter| &filter.kind) {
                Some(ExprKind::Column(column)) => Some(*column),
                _ => None,
            },
        )
        .collect::<Vec<_>>();
    let batch_updates_are_total = functions.iter().all(|function| {
        function.function.batch_update_is_total(
            &function
                .arguments
                .iter()
                .map(|argument| argument.data_type.clone())
                .collect::<Vec<_>>(),
        )
    });
    if !batch_updates_are_total {
        // The generic single-aggregate route retains registered column/batch
        // callbacks. Broader non-total shapes keep row/function error order.
        return Ok(None);
    }
    let mut states = functions
        .iter()
        .map(|function| {
            function.function.create_state(
                &function
                    .arguments
                    .iter()
                    .map(|argument| argument.data_type.clone())
                    .collect::<Vec<_>>(),
                context.query.types(),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let mut argument_rows = expressions
        .iter()
        .map(|arguments| Vec::with_capacity(arguments.len()))
        .collect::<Vec<Row>>();
    while let Some(batch) = input.next(context.query.batch_size())? {
        let mut column_selections: HashMap<usize, Arc<[usize]>> = HashMap::new();
        for column in filter_columns.iter().flatten().copied() {
            if let Entry::Vacant(entry) = column_selections.entry(column) {
                entry.insert(boolean_column_selection(&batch, column, context.query)?.into());
            }
        }
        let evaluated = expressions
            .iter()
            .zip(&filters)
            .zip(&filter_columns)
            .map(|((arguments, filter), filter_column)| {
                let selected;
                let input = if let Some(filter) = filter {
                    let selection = match filter_column {
                        Some(column) => column_selections
                            .get(column)
                            .expect("prepared column selection")
                            .as_ref(),
                        _ => {
                            let selection = filter.select_batch(&batch, context)?;
                            selected = batch.select(&selection)?;
                            return DataChunk::new(
                                arguments
                                    .iter()
                                    .map(|argument| argument.evaluate_batch(&selected, context))
                                    .collect::<Result<_>>()?,
                                selected.len(),
                            );
                        }
                    };
                    selected = batch.select(selection)?;
                    &selected
                } else {
                    &batch
                };
                DataChunk::new(
                    arguments
                        .iter()
                        .map(|argument| argument.evaluate_batch(input, context))
                        .collect::<Result<_>>()?,
                    input.len(),
                )
            })
            .collect::<Result<Vec<_>>>();
        let evaluated = match evaluated {
            Ok(evaluated) => evaluated,
            Err(error) if data_error(&error) => {
                update_scalar_batch(
                    &batch,
                    &expressions,
                    &filters,
                    &mut states,
                    &mut argument_rows,
                    context,
                )?;
                continue;
            }
            Err(error) => return Err(error),
        };
        for (arguments, state) in evaluated.iter().zip(states.iter_mut()) {
            state.update_batch(arguments, context.query)?;
        }
    }
    states
        .into_iter()
        .map(|state| state.finish())
        .collect::<Result<Row>>()
        .map(|row| Some(vec![row]))
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn boolean_column_selection(
    batch: &DataChunk,
    column: usize,
    query: &crate::parallel::QueryContext,
) -> Result<Vec<usize>> {
    let values = batch
        .columns()
        .get(column)
        .ok_or_else(|| Error::Internal("filter column outside aggregate input".into()))?;
    let mut selected = Vec::with_capacity(batch.len());
    for (index, value) in values.values().enumerate() {
        if index % 1024 == 0 {
            query.check()?;
        }
        match value {
            Value::Boolean(true) => selected.push(index),
            Value::Boolean(false) | Value::Null => {}
            _ => return Err(Error::Internal("filter column is not Boolean".into())),
        }
    }
    query.check()?;
    Ok(selected)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn update_scalar_batch(
    batch: &DataChunk,
    expressions: &[Vec<PreparedExpression<'_>>],
    filters: &[Option<PreparedExpression<'_>>],
    states: &mut [Box<dyn crate::function::AggregateState>],
    argument_rows: &mut [Row],
    context: &ExecutionContext<'_>,
) -> Result<()> {
    for row in batch.rows() {
        context.query.check()?;
        for (((arguments, filter), state), values) in expressions
            .iter()
            .zip(filters)
            .zip(states.iter_mut())
            .zip(argument_rows.iter_mut())
        {
            if let Some(filter) = filter {
                match filter.evaluate(&row, context)? {
                    Value::Boolean(true) => {}
                    Value::Boolean(false) | Value::Null => continue,
                    _ => {
                        return Err(Error::Internal(
                            "aggregate filter expression is not Boolean".into(),
                        ));
                    }
                }
            }
            values.clear();
            for argument in arguments {
                values.push(argument.evaluate(&row, context)?);
            }
            state.update(values, context.query)?;
        }
    }
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn data_error(error: &Error) -> bool {
    matches!(
        error,
        Error::Conversion(_)
            | Error::Execution(_)
            | Error::OutOfRange(_)
            | Error::InvalidInput(_)
            | Error::InvalidType(_)
    )
}
