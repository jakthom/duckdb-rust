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
    /// Optional byte position from the original statement. This is diagnostic
    /// provenance, not identity; catalog storage must retain it without using
    /// it to reparse or evaluate the expression.
    #[serde(default)]
    pub source_span: Option<StoredSourceSpan>,
    pub kind: StoredExpressionKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSourceSpan {
    pub offset: u64,
    /// Older native formats retain the start but not the span length.
    pub length: Option<u32>,
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
    /// Syntactic operators remain distinct from a function call marked as an
    /// operator. Wire tags and selected function lowering belong to their
    /// respective format/language services, not this owned representation.
    Operator {
        kind: StoredOperator,
        children: Vec<StoredExpression>,
    },
}

/// The initial retained nested syntax subset. This is intentionally expandable,
/// not a claim that every SQL operator or accessor is supported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoredOperator {
    ListConstructor,
    Index,
    Field,
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
/// Explicitly composed execution of a closed catalog expression. Implementations
/// bind using the supplied catalog snapshot and query settings/types, apply the
/// declared assignment target through selected casts, and validate their result.
/// They must reject row/subquery dependencies and volatile/external effects,
/// preserve fatal failures and cancellation, and perform no I/O or publication.
/// The owned result belongs to this operation; callers must not silently repeat
/// execution while copying catalog state or preparing a log.
pub trait StoredExpressionEvaluator: Send + Sync {
    fn evaluate(
        &self,
        expression: &StoredExpression,
        target: &DataType,
        catalog: &dyn super::Catalog,
        query: &QueryContext,
    ) -> Result<Value>;
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl StoredExpression {
    pub fn literal(data_type: DataType, value: Value) -> Self {
        Self {
            alias: None,
            source_span: None,
            kind: StoredExpressionKind::Literal { data_type, value },
        }
    }

    pub fn as_literal(&self) -> Option<(&DataType, &Value)> {
        match &self.kind {
            StoredExpressionKind::Literal { data_type, value } => Some((data_type, value)),
            _ => None,
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
            if expression.source_span.is_some_and(|span| {
                span.length
                    .is_some_and(|length| span.offset.checked_add(u64::from(length)).is_none())
            }) {
                return Err(Error::Resource(
                    "stored expression source span overflow".into(),
                ));
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
                StoredExpressionKind::Operator { kind, children } => {
                    if matches!(kind, StoredOperator::Index | StoredOperator::Field)
                        && children.len() != 2
                    {
                        return Err(Error::Bind("invalid stored accessor arity".into()));
                    }
                    if children.len() > 16_384 - nodes
                        || pending.len() > 16_384 - nodes - children.len()
                    {
                        return Err(Error::Resource("stored expression node limit".into()));
                    }
                    pending.extend(children.iter().rev().map(|child| (child, depth + 1)));
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
