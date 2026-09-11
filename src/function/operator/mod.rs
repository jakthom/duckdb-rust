//! Owned operator overloads and checked execution, independent of SQL syntax.
mod arithmetic;
mod batch;
pub(crate) mod bitwise;
mod date;
pub mod decimal;
mod string;
pub mod temporal;

pub use arithmetic::NumericArithmetic;
pub use batch::evaluate_operator_rows;
pub use date::DateArithmetic;
pub use string::{Concatenate, DynamicLike, GreedyLike};

use std::{collections::BTreeMap, fmt::Debug, sync::Arc};

use crate::{
    common::{
        DataType, Error, Result, Value,
        cast::{CastMode, CastRegistry},
        type_registry::{BoundType, TypeRegistry},
    },
    function::FunctionEffects,
    parallel::QueryContext,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Operator {
    Plus,
    Negate,
    Add,
    Subtract,
    Multiply,
    Divide,
    IntegerDivide,
    Modulo,
    Concat,
    Like,
    NotLike,
    BitAnd,
    BitOr,
    BitXor,
    BitNot,
    ShiftLeft,
    ShiftRight,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Operator {
    pub fn arity(self) -> usize {
        if matches!(self, Self::Plus | Self::Negate | Self::BitNot) {
            1
        } else {
            2
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperatorSignature {
    pub operator: Operator,
    pub arguments: Vec<DataType>,
    pub result: DataType,
    /// Whether non-NULL arguments can produce NULL, e.g. integer division by 0.
    pub nullable: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Synchronous scalar operation on non-NULL, already typed arguments. Inputs
/// are borrowed; output and retained configuration are owned. Implementations
/// support concurrent callers and observe cancellation during long work.
/// Arithmetic errors are Execution, invalid conversions are Conversion, and
/// resource/interruption failures retain their categories. Effects must be
/// declared; replacement preserves the signature's meaning and NULL rules.
/// Coercion is selected at binding and never performed inside this callback.
pub trait OperatorFunction: Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn supports(&self, signature: &OperatorSignature) -> bool;
    /// Optional parameterized signature construction. Only adapters explicitly
    /// installed with register_family participate; exact signatures override it.
    /// Returned argument types require registered casts and retained type adapters.
    fn specialize(
        &self,
        _operator: Operator,
        _arguments: &[OperatorArgument<'_>],
    ) -> Result<Option<OperatorSignature>> {
        Ok(None)
    }
    fn coercion(&self) -> CastMode {
        CastMode::Implicit
    }
    fn effects(&self) -> FunctionEffects {
        FunctionEffects::default()
    }
    /// Optional result metadata when binding proves an input is a constant
    /// NULL before argument coercion. Some template overloads have no complete
    /// result until binding and use SQL NULL in that case. None preserves the
    /// ordinary signature. Replacements must preserve this binding contract.
    fn null_constant_type(&self, _signature: &OperatorSignature) -> Option<DataType> {
        None
    }
    /// Optional proof that all valid inputs consistent with these constants
    /// produce a value (possibly NULL), without a data-dependent error. None
    /// means an unknown argument. This does not waive cancellation or resource
    /// limits. The default makes no proof; callers must also check effects.
    fn is_total(&self, _signature: &OperatorSignature, _constants: &[Option<&Value>]) -> bool {
        false
    }
    /// Owned results in input order, with the same scalar semantics and NULL
    /// propagation. Inputs are already validated against the bound signature.
    /// Do not retain input borrows. A failed batch is discarded; adapters must
    /// observe cancellation during work. The default invokes this adapter's
    /// scalar method once for each non-NULL argument row.
    fn evaluate_batch(
        &self,
        signature: &OperatorSignature,
        arguments: &crate::common::vector::DataChunk,
        query: &QueryContext,
    ) -> Result<crate::common::vector::Vector> {
        evaluate_operator_rows(self, signature, arguments, query)
    }
    fn evaluate(
        &self,
        signature: &OperatorSignature,
        arguments: &[Value],
        query: &QueryContext,
    ) -> Result<Value>;
}

/// Immutable retained binding. Its constructor validates the selected types;
/// callers cannot change its signature independently of its implementation.
#[derive(Clone, Debug)]
pub struct BoundOperator {
    signature: OperatorSignature,
    arguments: Vec<BoundType>,
    result: BoundType,
    function: Arc<dyn OperatorFunction>,
    effects: FunctionEffects,
    null_constant_type: Option<DataType>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl BoundOperator {
    pub fn signature(&self) -> &OperatorSignature {
        &self.signature
    }
    pub fn adapter(&self) -> &'static str {
        self.function.name()
    }
    pub fn effects(&self) -> FunctionEffects {
        self.effects
    }
    pub fn null_constant_type(&self) -> Option<&DataType> {
        self.null_constant_type.as_ref()
    }
    pub fn is_total(&self, constants: &[Option<&Value>]) -> bool {
        constants.len() == self.arguments.len()
            && constants
                .iter()
                .zip(&self.arguments)
                .all(|(value, data_type)| {
                    value.is_none_or(|value| value.fits_type(data_type.data_type()))
                })
            && self.function.is_total(&self.signature, constants)
    }
    pub fn apply(&self, arguments: &[Value], query: &QueryContext) -> Result<Value> {
        query.check()?;
        if arguments.len() != self.arguments.len() {
            return Err(Error::Internal(
                "operator argument count differs from binding".into(),
            ));
        }
        let mut has_null = false;
        for (value, data_type) in arguments.iter().zip(&self.arguments) {
            if !value.fits_type(data_type.data_type()) {
                return Err(Error::Internal(
                    "operator argument differs from its bound type".into(),
                ));
            }
            if data_type.requires_logical_validation() {
                data_type.validate(value, query)?;
            }
            has_null |= value.is_null();
        }
        if has_null {
            return Ok(Value::Null);
        }
        let result = self.function.evaluate(&self.signature, arguments, query);
        query.check()?;
        let result = result?;
        if (!self.signature.nullable && result.is_null())
            || !result.fits_type(self.result.data_type())
        {
            return Err(Error::Internal(format!(
                "operator {} returned an invalid physical result",
                self.adapter()
            )));
        }
        if self.result.requires_logical_validation() {
            self.result
                .validate(&result, query)
                .map_err(|error| match error {
                    Error::Conversion(_) => {
                        Error::Internal("operator returned an invalid logical result".into())
                    }
                    other => other,
                })?;
        }
        Ok(result)
    }
}

/// Integer token information is a binding input, not a runtime SQL type.
/// Frontends may supply it only for a literal whose value fits its type.
pub struct OperatorArgument<'a> {
    pub data_type: &'a DataType,
    pub integer_literal: Option<i128>,
}

pub struct ResolvedOperator {
    pub function: Arc<BoundOperator>,
    /// One explicit plan cast mode per argument; targets are in the signature.
    pub coercions: Vec<CastMode>,
}

#[derive(Clone, Debug)]
struct Entry {
    signature: OperatorSignature,
    function: Arc<dyn OperatorFunction>,
    coercion: CastMode,
}

/// Exact signatures are unique. Resolution minimizes registered cast costs;
/// equal best costs are ambiguity errors, never registration-order choices.
/// Missing casts or overloads have no hidden built-in fallback.
#[derive(Clone, Debug, Default)]
pub struct OperatorRegistry {
    entries: BTreeMap<(Operator, Vec<DataType>), Entry>,
    families: BTreeMap<String, Arc<dyn OperatorFunction>>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl OperatorRegistry {
    pub fn builtins() -> Self {
        let mut registry = Self::default();
        arithmetic::register(&mut registry);
        bitwise::register(&mut registry);
        date::register(&mut registry);
        temporal::register(&mut registry);
        string::register(&mut registry);
        super::binary_scalar::register_operators(&mut registry);
        super::bignum::register_operators(&mut registry);
        registry
            .register_family("decimal", Arc::new(decimal::DecimalArithmetic))
            .expect("unique decimal operator family");
        registry
    }
    pub fn register_family(
        &mut self,
        name: &str,
        function: Arc<dyn OperatorFunction>,
    ) -> Result<()> {
        if self.families.contains_key(name) {
            return Err(Error::Catalog("operator family already registered".into()));
        }
        self.families.insert(name.to_owned(), function);
        Ok(())
    }
    pub fn replace_family(
        &mut self,
        name: &str,
        function: Arc<dyn OperatorFunction>,
    ) -> Result<()> {
        let entry = self
            .families
            .get_mut(name)
            .ok_or_else(|| Error::Catalog("operator family not registered".into()))?;
        *entry = function;
        Ok(())
    }
    fn specialize(
        &self,
        operator: Operator,
        arguments: &[OperatorArgument<'_>],
    ) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        for function in self.families.values() {
            if let Some(signature) = function.specialize(operator, arguments)? {
                if signature.operator != operator
                    || signature.arguments.len() != operator.arity()
                    || !function.supports(&signature)
                {
                    return Err(Error::Bind(
                        "invalid parameterized operator signature".into(),
                    ));
                }
                entries.push(Entry {
                    signature,
                    function: function.clone(),
                    coercion: function.coercion(),
                });
            }
        }
        Ok(entries)
    }
    pub fn register(
        &mut self,
        signature: OperatorSignature,
        function: Arc<dyn OperatorFunction>,
    ) -> Result<()> {
        let key = (signature.operator, signature.arguments.clone());
        if self.entries.contains_key(&key) {
            return Err(Error::Catalog(
                "operator overload already registered".into(),
            ));
        }
        self.install(signature, function)
    }
    pub fn replace(
        &mut self,
        signature: OperatorSignature,
        function: Arc<dyn OperatorFunction>,
    ) -> Result<()> {
        let key = (signature.operator, signature.arguments.clone());
        let existing = self
            .entries
            .get(&key)
            .ok_or_else(|| Error::Catalog("operator overload is not registered".into()))?;
        if existing.signature != signature {
            return Err(Error::Bind(
                "replacement must preserve the operator signature".into(),
            ));
        }
        if existing.function.null_constant_type(&signature)
            != function.null_constant_type(&signature)
        {
            return Err(Error::Bind(
                "replacement must preserve constant-NULL result metadata".into(),
            ));
        }
        self.install(signature, function)
    }
    fn install(
        &mut self,
        signature: OperatorSignature,
        function: Arc<dyn OperatorFunction>,
    ) -> Result<()> {
        if signature.arguments.len() != signature.operator.arity() || !function.supports(&signature)
        {
            return Err(Error::Bind(
                "operator does not support its declared signature".into(),
            ));
        }
        for data_type in signature.arguments.iter().chain([&signature.result]) {
            crate::common::type_registry::check_metadata(data_type)?;
        }
        let coercion = function.coercion();
        self.entries.insert(
            (signature.operator, signature.arguments.clone()),
            Entry {
                signature,
                function,
                coercion,
            },
        );
        Ok(())
    }
    pub fn bind(
        &self,
        operator: Operator,
        arguments: &[DataType],
        types: &TypeRegistry,
    ) -> Result<BoundOperator> {
        if let Some(entry) = self.entries.get(&(operator, arguments.to_vec())) {
            return Self::bind_entry(entry, types);
        }
        let inputs: Vec<_> = arguments
            .iter()
            .map(|t| OperatorArgument {
                data_type: t,
                integer_literal: None,
            })
            .collect();
        let entries: Vec<_> = self
            .specialize(operator, &inputs)?
            .into_iter()
            .filter(|e| e.signature.arguments == arguments)
            .collect();
        if let [entry] = entries.as_slice() {
            Self::bind_entry(entry, types)
        } else {
            Err(Error::Bind(format!(
                "no unique exact {operator:?} overload for {arguments:?}"
            )))
        }
    }
    fn bind_entry(entry: &Entry, types: &TypeRegistry) -> Result<BoundOperator> {
        let null_constant_type = entry.function.null_constant_type(&entry.signature);
        if let Some(data_type) = &null_constant_type {
            types.bind(data_type)?;
        }
        Ok(BoundOperator {
            signature: entry.signature.clone(),
            arguments: entry
                .signature
                .arguments
                .iter()
                .map(|t| types.bind(t))
                .collect::<Result<_>>()?,
            result: types.bind(&entry.signature.result)?,
            function: entry.function.clone(),
            effects: entry.function.effects(),
            null_constant_type,
        })
    }
    pub fn resolve(
        &self,
        operator: Operator,
        arguments: &[OperatorArgument<'_>],
        casts: &CastRegistry,
        types: &TypeRegistry,
        query: &QueryContext,
    ) -> Result<ResolvedOperator> {
        query.check()?;
        if arguments.len() != operator.arity() {
            return Err(Error::Bind("operator arity".into()));
        }
        for argument in arguments {
            types.bind(argument.data_type)?;
            if let Some(value) = argument.integer_literal
                && (!argument.data_type.is_integer()
                    || !Value::Integer(value).fits_type(argument.data_type))
            {
                return Err(Error::Bind("invalid integer literal binding input".into()));
            }
        }
        let exact: Vec<_> = arguments.iter().map(|a| a.data_type.clone()).collect();
        if let Some(entry) = self.entries.get(&(operator, exact)) {
            return Ok(ResolvedOperator {
                function: Arc::new(Self::bind_entry(entry, types)?),
                coercions: vec![entry.coercion; arguments.len()],
            });
        }
        // All declared operators have one or two arguments. Candidate scoring
        // needs no heap allocation; retain owned modes only for the winner.
        let mut best: Option<(&Entry, u64, [CastMode; 2])> = None;
        let mut ambiguous = false;
        let generated = self.specialize(operator, arguments)?;
        for entry in self
            .entries
            .values()
            .chain(generated.iter())
            .filter(|entry| entry.signature.operator == operator)
        {
            query.check()?;
            let mut cost = 0_u64;
            let mut modes = [CastMode::Implicit; 2];
            let mut converted = 0;
            for (argument, target) in arguments.iter().zip(&entry.signature.arguments) {
                let literal = target.is_integer()
                    && argument.integer_literal.is_some_and(|n| {
                        if target.is_unsigned_integer() {
                            n >= 0 && Value::Unsigned(n as u128).fits_type(target)
                        } else {
                            Value::Integer(n).fits_type(target)
                        }
                    });
                let mode = if literal {
                    CastMode::Explicit
                } else {
                    entry.coercion
                };
                let Some(mut part) =
                    casts.coercion_cost_with_types(argument.data_type, target, mode, types)?
                else {
                    break;
                };
                if argument.data_type.is_decimal() && target.is_decimal() {
                    part = 0;
                }
                if literal && argument.data_type != target {
                    part = part.saturating_sub(90);
                }
                cost += u64::from(part);
                modes[converted] = mode;
                converted += 1;
            }
            if converted != arguments.len() {
                continue;
            }
            match &best {
                Some((_, previous, _)) if cost > *previous => (),
                Some((_, previous, _)) if cost == *previous => ambiguous = true,
                _ => {
                    best = Some((entry, cost, modes));
                    ambiguous = false;
                }
            }
        }
        if ambiguous {
            return Err(Error::Bind(format!("ambiguous {operator:?} overload")));
        }
        let (entry, _, coercions) =
            best.ok_or_else(|| Error::Bind(format!("no applicable {operator:?} overload")))?;
        Ok(ResolvedOperator {
            function: Arc::new(Self::bind_entry(entry, types)?),
            coercions: coercions[..arguments.len()].to_vec(),
        })
    }
    pub fn adapters(&self) -> Vec<(&'static str, &'static str)> {
        self.entries
            .values()
            .map(|e| e.function.name())
            .chain(self.families.values().map(|f| f.name()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|name| ("operators", name))
            .collect()
    }
}
