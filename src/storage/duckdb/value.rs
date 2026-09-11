//! Native `Value::SerializeInternal` metadata, not parsed expressions or column
//! slots. Declared types, including typed NULLs, never pass through a SQL cast.
use super::binary::{Encoder, Reader, corrupt};
use crate::{
    common::type_registry::{BoundType, TypeRegistry},
    common::{DataType, Error, NestedPayload, NestedType, NestedValue, Result, Value},
    parallel::QueryContext,
};
use std::collections::HashMap;
mod metadata;
mod read;
#[cfg(test)]
mod tests;
mod write;

const MAX_NODES: usize = 16_777_216;
const MAX_BYTES: usize = 64 * 1024 * 1024;

struct State<'a> {
    version: u64,
    types: &'a TypeRegistry,
    query: &'a QueryContext,
    nodes: usize,
    bytes: usize,
    bindings: HashMap<DataType, BoundType>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> State<'a> {
    fn new(version: u64, types: &'a TypeRegistry, query: &'a QueryContext) -> Result<Self> {
        query.check()?;
        super::write_support::new_headers(version)?;
        Ok(Self {
            version,
            types,
            query,
            nodes: MAX_NODES,
            bytes: MAX_BYTES,
            bindings: HashMap::new(),
        })
    }
    fn visit(&mut self, depth: usize) -> Result<()> {
        self.query.check()?;
        if depth > 64 {
            return Err(Error::Resource("native Value nesting exceeds 64".into()));
        }
        self.charge_nodes(1)
    }
    fn charge_nodes(&mut self, count: usize) -> Result<()> {
        self.query.check()?;
        self.nodes = self.nodes.checked_sub(count).ok_or_else(|| {
            Error::Resource("native Value exceeds 16 million logical visits".into())
        })?;
        Ok(())
    }
    fn charge_bytes(&mut self, count: usize) -> Result<()> {
        self.query.check()?;
        self.bytes = self
            .bytes
            .checked_sub(count)
            .ok_or_else(|| Error::Resource("native Value exceeds 64 MiB materialization".into()))?;
        Ok(())
    }
    fn binding(&mut self, ty: &DataType) -> Result<BoundType> {
        self.query.check()?;
        if let Some(bound) = self.bindings.get(ty) {
            return Ok(bound.clone());
        }
        let bound = self.types.bind(ty)?;
        self.bindings.insert(ty.clone(), bound.clone());
        Ok(bound)
    }
    fn blob(&mut self, reader: &mut Reader) -> Result<Vec<u8>> {
        let length = reader.length()?;
        self.charge_bytes(length)?;
        Ok(reader.bytes(length)?.to_vec())
    }
    fn string(&mut self, reader: &mut Reader) -> Result<String> {
        String::from_utf8(self.blob(reader)?).map_err(|_| corrupt("invalid native Value UTF-8"))
    }
    fn write_blob(&mut self, output: &mut Encoder, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_NODES {
            return Err(Error::Resource("native Value string exceeds 16 MiB".into()));
        }
        self.charge_bytes(bytes.len())?;
        output.blob(bytes);
        self.query.check()
    }
    fn version_type(&self, ty: &DataType) -> Result<()> {
        let minimum = match ty {
            DataType::Nested(nested) => match nested.as_ref() {
                NestedType::Variant => 68,
                NestedType::Tuple(_) => 69,
                NestedType::Struct(fields) if fields.is_empty() => 69,
                _ => 64,
            },
            _ => 64,
        };
        if self.version < minimum {
            return Err(Error::Unsupported(format!(
                "native Value family {} requires storage version {minimum}, got {}",
                ty.family(),
                self.version
            )));
        }
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Decode a root with explicit metadata. Children may inherit the declared
/// parent type; explicit old-format child types remain accepted and checked.
pub(in crate::storage::duckdb) fn read_typed(
    reader: &mut Reader,
    version: u64,
    types: &TypeRegistry,
    query: &QueryContext,
) -> Result<(DataType, Value)> {
    ValueCodec::new(version, types, query)?.read_typed(reader)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Append only after the entire selected validation and encoding succeeds.
pub(in crate::storage::duckdb) fn write_typed(
    output: &mut Encoder,
    declared: &DataType,
    value: &Value,
    version: u64,
    types: &TypeRegistry,
    query: &QueryContext,
) -> Result<()> {
    ValueCodec::new(version, types, query)?.write_typed(output, declared, value)
}

/// One enclosing parsed-expression root owns one session. Literal values share
/// metadata/payload budgets and retained selected bindings instead of receiving
/// a fresh allowance for each expression leaf. A failed session cannot resume;
/// its owner must abandon the complete enclosing root.
pub(in crate::storage::duckdb) struct ValueCodec<'a> {
    state: State<'a>,
    failed: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl<'a> ValueCodec<'a> {
    pub(in crate::storage::duckdb) fn new(
        version: u64,
        types: &'a TypeRegistry,
        query: &'a QueryContext,
    ) -> Result<Self> {
        Ok(Self {
            state: State::new(version, types, query)?,
            failed: false,
        })
    }
    fn active(&self) -> Result<()> {
        if self.failed {
            return Err(Error::Internal(
                "cannot reuse a failed native Value codec session".into(),
            ));
        }
        Ok(())
    }
    /// Resolved CAST metadata shares this root's type/payload budget. UNBOUND
    /// parsed type expressions remain unsupported here, not eagerly resolved.
    pub(in crate::storage::duckdb) fn read_type(
        &mut self,
        reader: &mut Reader,
    ) -> Result<DataType> {
        self.active()?;
        let result = metadata::read_for_cast(reader, &mut self.state).and_then(|ty| {
            self.state.binding(&ty)?;
            self.state.query.check()?;
            Ok(ty)
        });
        self.failed = result.is_err();
        result
    }
    pub(in crate::storage::duckdb) fn write_type(
        &mut self,
        output: &mut Encoder,
        ty: &DataType,
    ) -> Result<()> {
        self.active()?;
        let mut staged = Encoder::default();
        let result = (|| {
            self.state.binding(ty)?;
            metadata::write_for_cast(&mut staged, ty, &mut self.state)?;
            self.state.query.check()
        })();
        self.failed = result.is_err();
        result?;
        output.0.extend(staged.0);
        Ok(())
    }
    pub(in crate::storage::duckdb) fn read_typed(
        &mut self,
        reader: &mut Reader,
    ) -> Result<(DataType, Value)> {
        self.active()?;
        let result = read::value(reader, None, 0, &mut self.state)
            .and_then(|value| self.state.query.check().map(|()| value));
        self.failed = result.is_err();
        result
    }
    /// Each member stages its bytes, while the enclosing expression owner must
    /// additionally stage the whole root. Budget use is never rolled back.
    pub(in crate::storage::duckdb) fn write_typed(
        &mut self,
        output: &mut Encoder,
        declared: &DataType,
        value: &Value,
    ) -> Result<()> {
        self.active()?;
        let mut staged = Encoder::default();
        let result = write::value(&mut staged, declared, value, true, 0, &mut self.state)
            .and_then(|()| self.state.query.check());
        self.failed = result.is_err();
        result?;
        output.0.extend(staged.0);
        Ok(())
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn reserve<T>(count: usize) -> Result<Vec<T>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| Error::Resource("cannot allocate native Value children".into()))?;
    Ok(values)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Only errors from our local wire constructors are data corruption. Selected
/// adapters, cancellation, allocation and unsupported categories are untouched.
fn wire_error(error: Error) -> Error {
    match error {
        Error::Conversion(message) | Error::InvalidInput(message) | Error::OutOfRange(message) => {
            corrupt(message)
        }
        error => error,
    }
}
