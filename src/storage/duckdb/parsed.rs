//! Retained ParsedExpression wire objects. No SQL parsing, binding, casts or
//! expression evaluation occurs here. One root owns all materialization budgets.
use super::{
    binary::{Encoder, Reader, corrupt},
    value::ValueCodec,
};
use crate::{
    catalog::expression::{
        StoredArgument, StoredArgumentStyle, StoredExpression, StoredExpressionKind, StoredOperator,
    },
    common::{Error, Result},
    parallel::QueryContext,
};
mod read;
#[cfg(test)]
mod tests;
mod write;

const MAX_NODES: usize = 16_384;
const MAX_IDENTIFIERS: usize = 16 * 1024 * 1024;

struct State<'a> {
    query: &'a QueryContext,
    values: ValueCodec<'a>,
    version: u64,
    nodes: usize,
    pending: usize,
    identifiers: usize,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> State<'a> {
    fn new(version: u64, query: &'a QueryContext) -> Result<Self> {
        Ok(Self {
            query,
            values: ValueCodec::new(version, query.types(), query)?,
            version,
            nodes: MAX_NODES,
            pending: 1,
            identifiers: MAX_IDENTIFIERS,
        })
    }
    fn visit(&mut self, depth: usize) -> Result<()> {
        self.query.check()?;
        if depth > 64 {
            return Err(Error::Resource("native expression depth exceeds 64".into()));
        }
        self.nodes = self
            .nodes
            .checked_sub(1)
            .ok_or_else(|| Error::Resource("native expression node limit".into()))?;
        self.pending = self
            .pending
            .checked_sub(1)
            .ok_or_else(|| Error::Internal("native expression without reserved node".into()))?;
        Ok(())
    }
    fn count(&mut self, count: usize) -> Result<usize> {
        self.query.check()?;
        if count > self.nodes - self.pending {
            return Err(Error::Resource(
                "native expression child count exceeds remaining nodes".into(),
            ));
        }
        self.pending += count;
        Ok(count)
    }
    fn bytes(&mut self, count: usize) -> Result<()> {
        self.query.check()?;
        self.identifiers = self
            .identifiers
            .checked_sub(count)
            .ok_or_else(|| Error::Resource("native expression identifier byte limit".into()))?;
        Ok(())
    }
    fn string(&mut self, reader: &mut Reader) -> Result<String> {
        let count = reader.length()?;
        self.bytes(count)?;
        String::from_utf8(reader.bytes(count)?.to_vec())
            .map_err(|_| corrupt("invalid native expression identifier UTF-8"))
    }
    fn optional_name(&mut self, reader: &mut Reader, field: u16) -> Result<Option<String>> {
        if !reader.optional(field)? {
            return Ok(None);
        }
        let name = self.string(reader)?;
        Ok((!name.is_empty()).then_some(name))
    }
    fn write_name(&mut self, output: &mut Encoder, field: u16, name: Option<&str>) -> Result<()> {
        if let Some(name) = name {
            if name.is_empty() {
                return Err(Error::Bind("empty retained identifier".into()));
            }
            self.bytes(name.len())?;
            output.field(field);
            output.blob(name.as_bytes());
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn read(
    reader: &mut Reader,
    version: u64,
    query: &QueryContext,
) -> Result<StoredExpression> {
    let mut state = State::new(version, query)?;
    let expression = read::expression(reader, 0, &mut state)?;
    expression.validate(query)?;
    Ok(expression)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
pub(super) fn write(
    output: &mut Encoder,
    expression: &StoredExpression,
    version: u64,
    query: &QueryContext,
) -> Result<()> {
    query.check()?;
    expression.validate(query)?;
    let mut state = State::new(version, query)?;
    let mut staged = Encoder::default();
    write::expression(&mut staged, expression, 0, &mut state)?;
    query.check()?;
    output.0.extend(staged.0);
    Ok(())
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn reserve<T>(count: usize) -> Result<Vec<T>> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(count)
        .map_err(|_| Error::Resource("cannot allocate native expression children".into()))?;
    Ok(result)
}
