use crate::{
    common::{
        DataType, Error, Result,
        vector::{DataChunk, Vector},
    },
    function::table::{
        TableFunction, TableFunctionArgument, TableFunctionBind, TableFunctionBindContext,
        TableFunctionState,
    },
    parallel::QueryContext,
    planner::Field,
};

#[derive(Debug)]
pub(super) struct IntegerRange {
    inclusive: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl IntegerRange {
    pub(super) const fn new(inclusive: bool) -> Self {
        Self { inclusive }
    }
}

#[derive(Debug)]
struct RangeBindData {
    start: i64,
    end: i64,
    step: i64,
    empty: bool,
}

#[derive(Debug)]
struct RangeState {
    current: i128,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl TableFunction for IntegerRange {
    fn name(&self) -> &str {
        if self.inclusive {
            "generate_series"
        } else {
            "range"
        }
    }

    fn bind(
        &self,
        arguments: &[TableFunctionArgument],
        context: &TableFunctionBindContext<'_>,
    ) -> Result<TableFunctionBind> {
        context.query.check()?;
        if arguments.iter().any(|argument| argument.name.is_some()) {
            return Err(Error::Bind(format!(
                "{} does not accept named arguments",
                self.name()
            )));
        }
        if !(1..=3).contains(&arguments.len()) {
            return Err(Error::Bind(format!(
                "{} expects one to three integer arguments",
                self.name()
            )));
        }
        for argument in arguments {
            if !matches!(
                argument.data_type,
                DataType::Null
                    | DataType::TinyInt
                    | DataType::SmallInt
                    | DataType::Integer
                    | DataType::BigInt
                    | DataType::UTinyInt
                    | DataType::USmallInt
                    | DataType::UInteger
            ) {
                return Err(Error::Bind(format!(
                    "{} integer arguments must cast implicitly to BIGINT",
                    self.name()
                )));
            }
        }
        let schema = vec![Field::new(self.name(), DataType::BigInt)];
        if arguments.iter().any(|argument| argument.value.is_null()) {
            return Ok(TableFunctionBind::new(
                schema,
                RangeBindData {
                    start: 0,
                    end: 0,
                    step: 1,
                    empty: true,
                },
            ));
        }
        let values = arguments
            .iter()
            .map(|argument| {
                argument.value.as_i128().and_then(|value| {
                    i64::try_from(value)
                        .map_err(|_| Error::Conversion("range argument exceeds BIGINT".into()))
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let (start, end, step) = match values.as_slice() {
            [end] => (0, *end, 1),
            [start, end] => (*start, *end, 1),
            [start, end, step] => (*start, *end, *step),
            _ => unreachable!("validated range arity"),
        };
        if step == 0 {
            return Err(Error::Bind("range step cannot be zero".into()));
        }
        let empty = (step > 0 && start > end)
            || (step < 0 && start < end)
            || (!self.inclusive && start == end);
        Ok(TableFunctionBind::new(
            schema,
            RangeBindData {
                start,
                end,
                step,
                empty,
            },
        ))
    }

    fn init(
        &self,
        bind: &TableFunctionBind,
        context: &QueryContext,
    ) -> Result<Box<dyn TableFunctionState>> {
        context.check()?;
        let data = bind
            .data()
            .downcast_ref::<RangeBindData>()
            .ok_or_else(|| Error::Internal("range bind data type mismatch".into()))?;
        Ok(Box::new(RangeState {
            current: i128::from(data.start),
        }))
    }

    fn scan(
        &self,
        bind: &TableFunctionBind,
        state: &mut dyn TableFunctionState,
        max_rows: usize,
        context: &QueryContext,
    ) -> Result<Option<DataChunk>> {
        let data = bind
            .data()
            .downcast_ref::<RangeBindData>()
            .ok_or_else(|| Error::Internal("range bind data type mismatch".into()))?;
        let state = state
            .downcast_mut::<RangeState>()
            .ok_or_else(|| Error::Internal("range state type mismatch".into()))?;
        if data.empty {
            return Ok(None);
        }
        let end = i128::from(data.end);
        let step = i128::from(data.step);
        let distance = if step > 0 {
            if state.current > end || (!self.inclusive && state.current == end) {
                return Ok(None);
            }
            end - state.current
        } else {
            if state.current < end || (!self.inclusive && state.current == end) {
                return Ok(None);
            }
            state.current - end
        };
        let magnitude = step.abs();
        let available = if self.inclusive {
            distance / magnitude + 1
        } else {
            // The exclusive endpoint is known to be at least one unit away.
            (distance - 1) / magnitude + 1
        };
        let count = max_rows.min(usize::try_from(available).unwrap_or(usize::MAX));
        if count == 0 {
            return Ok(None);
        }
        let mut values = Vec::with_capacity(count);
        let mut value = state.current as i64;
        for start in (0..count).step_by(1024) {
            context.check()?;
            for _ in start..count.min(start + 1024) {
                values.push(value);
                // Cardinality was proved in i128. Wrapping is observable only
                // after the final emitted endpoint and is not retained as
                // authoritative state.
                value = value.wrapping_add(data.step);
            }
        }
        state.current += step * count as i128;
        let numeric_ascending = count < 2 || step > 0;
        // Range owns an all-valid native BIGINT lane and proved its ordering
        // while producing it. Avoid rebuilding a validity bitmap and rescanning
        // every value at the generic checked-constructor boundary.
        let values = Vector::bigints_prevalidated_with_order(values, numeric_ascending);
        DataChunk::new(vec![values], count).map(Some)
    }
}
