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
        let within = |value: i128| {
            if step > 0 {
                value < end || (self.inclusive && value == end)
            } else {
                value > end || (self.inclusive && value == end)
            }
        };
        let mut values = Vec::with_capacity(max_rows);
        while values.len() < max_rows && within(state.current) {
            if values.len() % 1024 == 0 {
                context.check()?;
            }
            let value = i64::try_from(state.current)
                .map_err(|_| Error::Internal("range state exceeded BIGINT".into()))?;
            values.push(value);
            state.current += step;
        }
        if values.is_empty() {
            return Ok(None);
        }
        let count = values.len();
        let values = Vector::try_bigints(values.into_iter().map(|value| Ok(Some(value))))?;
        DataChunk::new(vec![values], count).map(Some)
    }
}
