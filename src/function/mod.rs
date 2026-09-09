mod aggregate;
pub mod operator;
mod scalar;

use std::{collections::BTreeMap, fmt::Debug, sync::Arc};

use crate::{
    common::{DataType, Error, Result, Value},
    parallel::QueryContext,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct FunctionEffects {
    pub volatile: bool,
    pub external_access: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum ArgumentEvaluation {
    Eager,
    FirstNonNull,
}

pub trait ScalarFunction: Debug + Send + Sync {
    fn name(&self) -> &str;
    fn effects(&self) -> FunctionEffects {
        FunctionEffects::default()
    }
    fn argument_evaluation(&self) -> ArgumentEvaluation {
        ArgumentEvaluation::Eager
    }
    fn return_type(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType>;
    fn evaluate(&self, arguments: &[Value], context: &QueryContext) -> Result<Value>;
}

pub trait AggregateState: Send {
    fn update(&mut self, arguments: &[Value], context: &QueryContext) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<Value>;
}

pub trait AggregateFunction: Debug + Send + Sync {
    fn name(&self) -> &str;
    fn return_type(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<DataType>;
    fn create_state(
        &self,
        arguments: &[DataType],
        types: &crate::common::type_registry::TypeRegistry,
    ) -> Result<Box<dyn AggregateState>>;
}

#[derive(Clone, Default)]
pub struct FunctionRegistry {
    scalars: BTreeMap<String, Arc<dyn ScalarFunction>>,
    aggregates: BTreeMap<String, Arc<dyn AggregateFunction>>,
}

impl FunctionRegistry {
    pub fn builtins() -> Self {
        let mut registry = Self::default();
        scalar::register(&mut registry);
        aggregate::register(&mut registry);
        registry
    }
    pub fn register_scalar(&mut self, function: Arc<dyn ScalarFunction>) -> Result<()> {
        let key = function.name().to_ascii_lowercase();
        if self.scalars.contains_key(&key) || self.aggregates.contains_key(&key) {
            return Err(Error::Catalog(format!("function {key} already exists")));
        }
        self.scalars.insert(key, function);
        Ok(())
    }
    pub fn register_aggregate(&mut self, function: Arc<dyn AggregateFunction>) -> Result<()> {
        let key = function.name().to_ascii_lowercase();
        if self.scalars.contains_key(&key) || self.aggregates.contains_key(&key) {
            return Err(Error::Catalog(format!("function {key} already exists")));
        }
        self.aggregates.insert(key, function);
        Ok(())
    }
    pub fn scalar(&self, name: &str) -> Result<Arc<dyn ScalarFunction>> {
        self.scalars
            .get(&name.to_ascii_lowercase())
            .cloned()
            .ok_or_else(|| Error::Catalog(format!("scalar function {name} does not exist")))
    }
    pub fn aggregate(&self, name: &str) -> Option<Arc<dyn AggregateFunction>> {
        self.aggregates.get(&name.to_ascii_lowercase()).cloned()
    }
}
