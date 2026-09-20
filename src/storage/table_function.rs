//! Physical guard for registered table-function state and output boundaries.
use crate::{
    common::{Error, Result, type_registry::BoundType, vector::DataChunk},
    function::table::{BoundTableFunction, TableFunctionState},
    parallel::QueryContext,
};

pub struct TableFunctionScan<'a> {
    source: &'a BoundTableFunction,
    context: &'a QueryContext,
    validators: Vec<BoundType>,
    state: Option<Box<dyn TableFunctionState>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> TableFunctionScan<'a> {
    pub fn open(source: &'a BoundTableFunction, context: &'a QueryContext) -> Result<Self> {
        context.check()?;
        let validators = source
            .schema()
            .iter()
            .map(|field| context.types().bind(&field.data_type))
            .collect::<Result<Vec<_>>>()?;
        let state = source.function().init(source.bind(), context)?;
        if let Err(error) = context.check() {
            let _ = source.function().cleanup(source.bind(), state, context);
            return Err(error);
        }
        Ok(Self {
            source,
            context,
            validators,
            state: Some(state),
        })
    }

    pub fn next(&mut self, requested: usize) -> Result<Option<DataChunk>> {
        if self.state.is_none() {
            return Ok(None);
        }
        let max_rows = match self.context.batch_demand(requested) {
            Ok(max_rows) => max_rows,
            Err(error) => {
                let _ = self.finish();
                return Err(error);
            }
        };
        let state = self.state.as_mut().expect("live table function state");
        let result = self.source.function().scan_with_request(
            self.source.bind(),
            state.as_mut(),
            self.source.request(),
            max_rows,
            self.context,
        );
        let chunk = match result {
            Ok(Some(chunk)) => chunk,
            Ok(None) => {
                self.finish()?;
                return Ok(None);
            }
            Err(error) => {
                let _ = self.finish();
                return Err(error);
            }
        };
        if chunk.is_empty() || chunk.len() > max_rows {
            let _ = self.finish();
            return Err(Error::Internal(
                "table function violated batch cardinality".into(),
            ));
        }
        if chunk.columns().len() != self.validators.len() {
            let _ = self.finish();
            return Err(Error::Internal(
                "table function output differs from bound schema".into(),
            ));
        }
        for (column, expected) in chunk.columns().iter().zip(&self.validators) {
            if column.data_type() != expected.data_type() {
                let _ = self.finish();
                return Err(Error::Internal(
                    "table function output differs from bound schema".into(),
                ));
            }
            if expected.requires_logical_validation() {
                for value in column.values() {
                    if let Err(error) = expected.validate(&value, self.context) {
                        let _ = self.finish();
                        return Err(match error {
                            Error::Conversion(_) => Error::Internal(
                                "table function returned an invalid logical value".into(),
                            ),
                            other => other,
                        });
                    }
                }
            }
        }
        if let Err(error) = self.context.check() {
            let _ = self.finish();
            return Err(error);
        }
        Ok(Some(chunk))
    }

    fn finish(&mut self) -> Result<()> {
        let Some(state) = self.state.take() else {
            return Ok(());
        };
        self.source
            .function()
            .cleanup(self.source.bind(), state, self.context)
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Drop for TableFunctionScan<'_> {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
