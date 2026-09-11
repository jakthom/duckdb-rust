//! Owned, unbound catalog expressions. No selected adapter or evaluated default
//! is serialized here. Binding belongs to the caller's chosen language service.
use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};
use serde::{Deserialize, Serialize};

/// A revisable closed-scalar subset of stored expressions. Unsupported syntax
/// must remain unsupported, never an evaluated-value or diagnostic-SQL fallback.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredExpression {
    pub alias: Option<String>,
    pub kind: StoredExpressionKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StoredExpressionKind {
    /// Declared metadata is independent of the physical Value, including typed
    /// NULLs and small integers whose physical representation is full-width.
    Literal { data_type: DataType, value: Value },
    Cast {
        expression: Box<StoredExpression>,
        target: DataType,
        try_cast: bool,
    },
    Function {
        /// Ordered qualification components; none may be silently discarded.
        name: Vec<String>,
        arguments: Vec<StoredArgument>,
        is_operator: bool,
        argument_style: StoredArgumentStyle,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredArgument {
    pub name: Option<String>,
    pub expression: StoredExpression,
}

/// Native legacy child aliases and modern named arguments are distinct binding
/// provenance. A codec must preserve this or reject an unsupported conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoredArgumentStyle {
    LegacyAliases,
    Named,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl StoredExpression {
    pub fn literal(data_type: DataType, value: Value) -> Self {
        Self {
            alias: None,
            kind: StoredExpressionKind::Literal { data_type, value },
        }
    }

    /// Preflight the entire owned tree before any binding callback. The shared
    /// subset bounds depth at 64, nodes at 16,384, and identifier bytes at 16 MiB.
    /// Selected type adapters separately validate literal metadata/payloads and
    /// their resource limits. This validates data, not function existence/effects
    /// or whether a language/format implements a particular retained node.
    pub fn validate(&self, query: &QueryContext) -> Result<()> {
        let mut pending = vec![(self, 0_usize)];
        let mut nodes = 0_usize;
        let mut identifier_bytes = 0_usize;
        while let Some((expression, depth)) = pending.pop() {
            query.check()?;
            nodes += 1;
            if depth > 64 || nodes > 16_384 {
                return Err(Error::Resource("stored expression depth/node limit".into()));
            }
            if let Some(alias) = &expression.alias {
                identifier(alias, &mut identifier_bytes)?;
            }
            match &expression.kind {
                StoredExpressionKind::Literal { data_type, value } => {
                    query.types().bind(data_type)?.validate(value, query)?;
                }
                StoredExpressionKind::Cast {
                    expression, target, ..
                } => {
                    query.types().bind(target)?;
                    pending.push((expression, depth + 1));
                }
                StoredExpressionKind::Function {
                    name, arguments, ..
                } => {
                    if name.is_empty() || name.len() > 64 {
                        return Err(Error::Bind("invalid stored function qualification".into()));
                    }
                    for part in name {
                        identifier(part, &mut identifier_bytes)?;
                    }
                    // Bound work before allocating a worklist for all children.
                    if arguments.len() > 16_384 - nodes
                        || pending.len() > 16_384 - nodes - arguments.len()
                    {
                        return Err(Error::Resource("stored expression node limit".into()));
                    }
                    for argument in arguments.iter().rev() {
                        if let Some(name) = &argument.name {
                            identifier(name, &mut identifier_bytes)?;
                        }
                        pending.push((&argument.expression, depth + 1));
                    }
                }
            }
        }
        query.check()
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn identifier(value: &str, bytes: &mut usize) -> Result<()> {
    if value.is_empty() {
        return Err(Error::Bind("empty stored expression identifier".into()));
    }
    *bytes = bytes
        .checked_add(value.len())
        .filter(|bytes| *bytes <= 16 * 1024 * 1024)
        .ok_or_else(|| Error::Resource("stored expression identifier byte limit".into()))?;
    Ok(())
}
