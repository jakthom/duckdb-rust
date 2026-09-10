//! Configuration definitions, immutable statement views and scoped publication.
mod builtin;
mod store;

pub use store::{LockedConfiguration, SnapshotConfiguration};

use crate::{DataType, Error, Result, Value, parallel::QueryContext};
use std::{
    collections::BTreeMap,
    fmt::Debug,
    sync::{Arc, OnceLock},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingScope {
    Global,
    Session,
}

#[derive(Clone, Debug)]
pub struct SettingDefinition {
    pub name: String,
    pub aliases: Vec<String>,
    pub data_type: DataType,
    pub default: Value,
    pub default_scope: SettingScope,
    pub global: bool,
    pub session: bool,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Metadata is immutable after registration. Normalization is pure, accepts
/// the declared logical input type, checks cancellation, and returns that same
/// type or an error. Normalized values must be stable under normalization,
/// including floating-point bit patterns. Defaults retain their declared
/// representation. No callback may
/// publish state, consult other configuration, or access external resources
/// during validation. The query supplies types and cancellation/resources.
pub trait Setting: Debug + Send + Sync {
    fn definition(&self) -> SettingDefinition;
    fn normalize(&self, value: &Value, query: &QueryContext) -> Result<Value>;
}

#[derive(Debug)]
struct RegisteredSetting {
    definition: SettingDefinition,
    adapter: Arc<dyn Setting>,
}

#[derive(Debug, Default)]
pub struct SettingRegistry {
    entries: BTreeMap<String, Arc<RegisteredSetting>>,
    names: BTreeMap<String, String>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SettingRegistry {
    pub fn builtins() -> Self {
        let mut registry = Self::default();
        builtin::register(&mut registry);
        registry
    }
    pub fn register(&mut self, adapter: Arc<dyn Setting>) -> Result<()> {
        let mut definition = adapter.definition();
        definition.name.make_ascii_lowercase();
        for alias in &mut definition.aliases {
            alias.make_ascii_lowercase();
        }
        if definition.name.is_empty()
            || (!definition.global && !definition.session)
            || !scope_allowed(&definition, definition.default_scope)
        {
            return Err(Error::Bind("invalid setting definition".into()));
        }
        let mut names = std::collections::BTreeSet::new();
        for name in std::iter::once(&definition.name).chain(&definition.aliases) {
            if name.is_empty() || !names.insert(name.clone()) || self.names.contains_key(name) {
                return Err(Error::Catalog(format!(
                    "setting name {name} already exists or is invalid"
                )));
            }
        }
        for name in names {
            self.names.insert(name, definition.name.clone());
        }
        self.entries.insert(
            definition.name.clone(),
            Arc::new(RegisteredSetting {
                definition,
                adapter,
            }),
        );
        Ok(())
    }
    fn entry(&self, name: &str) -> Result<&Arc<RegisteredSetting>> {
        self.names
            .get(&name.to_ascii_lowercase())
            .and_then(|key| self.entries.get(key))
            .ok_or_else(|| Error::Catalog(format!("unrecognized configuration parameter {name}")))
    }
    pub fn definition(&self, name: &str) -> Result<&SettingDefinition> {
        Ok(&self.entry(name)?.definition)
    }
    pub fn validate(&self, query: &QueryContext) -> Result<()> {
        query.check()?;
        for entry in self.entries.values() {
            check_value(entry, &entry.definition.default, query)?;
            normalize(entry, &entry.definition.default, query)?;
        }
        Ok(())
    }
    /// Values must already have the declared input type. SQL coercion belongs
    /// to its selected cast adapter. None removes an override; it does not copy
    /// the current fallback value into that scope.
    pub fn bind(
        &self,
        name: &str,
        scope: Option<SettingScope>,
        value: Option<Value>,
        query: &QueryContext,
    ) -> Result<SettingChange> {
        query.check()?;
        let entry = self.entry(name)?;
        let scope = scope.unwrap_or(entry.definition.default_scope);
        if !scope_allowed(&entry.definition, scope) {
            return Err(Error::Unsupported(format!(
                "setting {name} does not support {scope:?} scope"
            )));
        }
        let value = value
            .map(|value| normalize(entry, &value, query))
            .transpose()?;
        query.check()?;
        Ok(SettingChange {
            entry: entry.clone(),
            scope,
            value,
        })
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn scope_allowed(definition: &SettingDefinition, scope: SettingScope) -> bool {
    match scope {
        SettingScope::Global => definition.global,
        SettingScope::Session => definition.session,
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn check_value(entry: &RegisteredSetting, value: &Value, query: &QueryContext) -> Result<()> {
    query
        .types()
        .bind(&entry.definition.data_type)?
        .validate(value, query)
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn normalize(entry: &RegisteredSetting, value: &Value, query: &QueryContext) -> Result<Value> {
    query.check()?;
    check_value(entry, value, query)?;
    let result = entry.adapter.normalize(value, query);
    query.check()?;
    let result = result?;
    check_value(entry, &result, query).map_err(|error| match error {
        Error::Conversion(_) => {
            Error::Internal("setting adapter returned an invalid logical value".into())
        }
        other => other,
    })?;
    Ok(result)
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn validate_normalization(
    entry: &RegisteredSetting,
    value: &Value,
    query: &QueryContext,
) -> Result<()> {
    let normalized = normalize(entry, value, query)?;
    if same_representation(&normalized, value)
        || same_representation(&entry.definition.default, value)
    {
        Ok(())
    } else {
        Err(Error::Internal(
            "configuration value is not normalized".into(),
        ))
    }
}

// Configuration canonicalization concerns retained representation, not SQL
// equality. An unchanged NaN is valid, and a zero's sign must not disappear.
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn same_representation(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Float(left), Value::Float(right)) => left.to_bits() == right.to_bits(),
        (Value::Double(left), Value::Double(right)) => left.to_bits() == right.to_bits(),
        _ => left == right,
    }
}

#[derive(Clone, Debug)]
pub struct SettingChange {
    entry: Arc<RegisteredSetting>,
    scope: SettingScope,
    value: Option<Value>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SettingChange {
    pub fn name(&self) -> &str {
        &self.entry.definition.name
    }
    pub fn scope(&self) -> SettingScope {
        self.scope
    }
    pub fn value(&self) -> Option<&Value> {
        self.value.as_ref()
    }
    pub fn validate(&self, registry: &SettingRegistry, query: &QueryContext) -> Result<()> {
        query.check()?;
        if !Arc::ptr_eq(registry.entry(self.name())?, &self.entry) {
            return Err(Error::Bind(
                "setting change belongs to a different registry".into(),
            ));
        }
        if let Some(value) = &self.value {
            validate_normalization(&self.entry, value, query)?;
        }
        Ok(())
    }
}

pub type SettingValues = BTreeMap<String, Value>;

/// An owned view of one global publication and one session's overrides.
/// Values are canonical registered names. Views survive later changes and
/// connection destruction. No transaction/catalog snapshot is implied.
#[derive(Clone, Debug)]
pub struct SettingsSnapshot {
    registry: Arc<SettingRegistry>,
    global: Arc<SettingValues>,
    session: Arc<SettingValues>,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Default for SettingsSnapshot {
    fn default() -> Self {
        static DEFAULT: OnceLock<SettingsSnapshot> = OnceLock::new();
        DEFAULT
            .get_or_init(|| Self {
                registry: Arc::new(SettingRegistry::builtins()),
                global: Arc::new(BTreeMap::new()),
                session: Arc::new(BTreeMap::new()),
            })
            .clone()
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SettingsSnapshot {
    pub fn new(
        registry: Arc<SettingRegistry>,
        global: Arc<SettingValues>,
        session: Arc<SettingValues>,
        query: &QueryContext,
    ) -> Result<Self> {
        query.check()?;
        registry.validate(query)?;
        for (scope, values) in [
            (SettingScope::Global, &global),
            (SettingScope::Session, &session),
        ] {
            for (name, value) in values.iter() {
                query.check()?;
                let entry = registry.entry(name)?;
                if entry.definition.name != *name || !scope_allowed(&entry.definition, scope) {
                    return Err(Error::Internal(
                        "invalid configuration snapshot key or scope".into(),
                    ));
                }
                validate_normalization(entry, value, query)?;
            }
        }
        Ok(Self {
            registry,
            global,
            session,
        })
    }
    pub fn registry(&self) -> &Arc<SettingRegistry> {
        &self.registry
    }
    pub fn get(&self, name: &str, query: &QueryContext) -> Result<&Value> {
        query.check()?;
        let entry = self.registry.entry(name)?;
        let value = self
            .session
            .get(&entry.definition.name)
            .or_else(|| self.global.get(&entry.definition.name))
            .unwrap_or(&entry.definition.default);
        check_value(entry, value, query)?;
        Ok(value)
    }
    pub fn ordering(
        &self,
        asc: Option<bool>,
        nulls_first: Option<bool>,
        query: &QueryContext,
    ) -> Result<(bool, bool)> {
        let ascending = match asc {
            Some(ascending) => ascending,
            None => match self.get("default_order", query)? {
                Value::Varchar(value) if value == "ASC" || value == "ASCENDING" => true,
                Value::Varchar(value) if value == "DESC" => false,
                _ => return Err(Error::Internal("invalid default_order setting".into())),
            },
        };
        let first = match nulls_first {
            Some(first) => first,
            None => match self.get("default_null_order", query)? {
                Value::Varchar(value) => match value.as_str() {
                    "NULLS_FIRST" => true,
                    "NULLS_LAST" => false,
                    "NULLS_FIRST_ON_ASC_LAST_ON_DESC" => ascending,
                    "NULLS_LAST_ON_ASC_FIRST_ON_DESC" => !ascending,
                    _ => return Err(Error::Internal("invalid default_null_order setting".into())),
                },
                _ => return Err(Error::Internal("invalid default_null_order type".into())),
            },
        };
        Ok((!ascending, first))
    }
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Database-scoped settings provider. Sessions share global publications but
/// own independent overrides. Construction has no I/O or state publication.
pub trait Configuration: Debug + Send + Sync {
    fn name(&self) -> &'static str;
    fn connect(&self) -> Box<dyn ConfigurationSession>;
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
/// Each snapshot observes one atomic global publication and owned local state.
/// apply publishes one validated change atomically; errors/cancellation before
/// publication preserve both scopes. Setting changes are not transactional.
/// Adapters must reject changes bound through a different setting registry.
pub trait ConfigurationSession: Send {
    fn snapshot(&self, query: &QueryContext) -> Result<SettingsSnapshot>;
    fn apply(&mut self, change: &SettingChange, query: &QueryContext) -> Result<()>;
}
