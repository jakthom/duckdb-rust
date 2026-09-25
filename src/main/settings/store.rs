use super::*;
use std::sync::{Mutex, RwLock};

/// Copy-on-write global publications; read snapshots retain an Arc generation.
#[derive(Debug)]
pub struct SnapshotConfiguration {
    registry: Arc<SettingRegistry>,
    global: Arc<RwLock<SnapshotValues>>,
}
#[derive(Clone, Debug)]
struct SnapshotValues {
    values: Arc<SettingValues>,
    generation: u64,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Default for SnapshotConfiguration {
    fn default() -> Self {
        Self::new(Arc::new(SettingRegistry::builtins()))
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl SnapshotConfiguration {
    pub fn new(registry: Arc<SettingRegistry>) -> Self {
        Self {
            registry,
            global: Arc::new(RwLock::new(SnapshotValues {
                values: Arc::new(BTreeMap::new()),
                generation: 0,
            })),
        }
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Configuration for SnapshotConfiguration {
    fn name(&self) -> &'static str {
        "snapshot-configuration"
    }
    fn connect(&self) -> Box<dyn ConfigurationSession> {
        Box::new(SnapshotSession {
            registry: self.registry.clone(),
            global: self.global.clone(),
            session: Arc::new(BTreeMap::new()),
            session_identity: Arc::new(SettingsSessionIdentity),
            session_generation: 0,
        })
    }
}
struct SnapshotSession {
    registry: Arc<SettingRegistry>,
    global: Arc<RwLock<SnapshotValues>>,
    session: Arc<SettingValues>,
    session_identity: Arc<SettingsSessionIdentity>,
    session_generation: u64,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ConfigurationSession for SnapshotSession {
    fn snapshot(&self, query: &QueryContext) -> Result<SettingsSnapshot> {
        query.check()?;
        let global = self.global.read().map_err(|_| poisoned())?.clone();
        SettingsSnapshot::new(
            self.registry.clone(),
            global.values,
            self.session.clone(),
            query,
        )
        .map(|snapshot| {
            snapshot.with_generation(SettingsGeneration {
                global: global.generation,
                session_identity: self.session_identity.clone(),
                session: self.session_generation,
            })
        })
    }
    fn apply(&mut self, change: &SettingChange, query: &QueryContext) -> Result<()> {
        change.validate(&self.registry, query)?;
        match change.scope() {
            SettingScope::Global => {
                let mut global = self.global.write().map_err(|_| poisoned())?;
                query.check()?;
                let generation = global
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| Error::Resource("settings generation exhausted".into()))?;
                update(Arc::make_mut(&mut global.values), change);
                global.generation = generation;
            }
            SettingScope::Session => {
                query.check()?;
                let generation = self
                    .session_generation
                    .checked_add(1)
                    .ok_or_else(|| Error::Resource("settings generation exhausted".into()))?;
                update(Arc::make_mut(&mut self.session), change);
                self.session_generation = generation;
            }
        }
        Ok(())
    }
}

/// An independent store copies the effective maps while holding a mutex;
/// snapshots do not retain a global copy-on-write generation.
#[derive(Debug)]
pub struct LockedConfiguration {
    registry: Arc<SettingRegistry>,
    global: Arc<Mutex<LockedValues>>,
}
#[derive(Debug)]
struct LockedValues {
    values: SettingValues,
    generation: u64,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Default for LockedConfiguration {
    fn default() -> Self {
        Self::new(Arc::new(SettingRegistry::builtins()))
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl LockedConfiguration {
    pub fn new(registry: Arc<SettingRegistry>) -> Self {
        Self {
            registry,
            global: Arc::new(Mutex::new(LockedValues {
                values: BTreeMap::new(),
                generation: 0,
            })),
        }
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Configuration for LockedConfiguration {
    fn name(&self) -> &'static str {
        "locked-configuration"
    }
    fn connect(&self) -> Box<dyn ConfigurationSession> {
        Box::new(LockedSession {
            registry: self.registry.clone(),
            global: self.global.clone(),
            session: BTreeMap::new(),
            session_identity: Arc::new(SettingsSessionIdentity),
            session_generation: 0,
        })
    }
}
struct LockedSession {
    registry: Arc<SettingRegistry>,
    global: Arc<Mutex<LockedValues>>,
    session: SettingValues,
    session_identity: Arc<SettingsSessionIdentity>,
    session_generation: u64,
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl ConfigurationSession for LockedSession {
    fn snapshot(&self, query: &QueryContext) -> Result<SettingsSnapshot> {
        query.check()?;
        let global = self.global.lock().map_err(|_| poisoned())?;
        SettingsSnapshot::new(
            self.registry.clone(),
            Arc::new(global.values.clone()),
            Arc::new(self.session.clone()),
            query,
        )
        .map(|snapshot| {
            snapshot.with_generation(SettingsGeneration {
                global: global.generation,
                session_identity: self.session_identity.clone(),
                session: self.session_generation,
            })
        })
    }
    fn apply(&mut self, change: &SettingChange, query: &QueryContext) -> Result<()> {
        change.validate(&self.registry, query)?;
        match change.scope() {
            SettingScope::Global => {
                let mut global = self.global.lock().map_err(|_| poisoned())?;
                query.check()?;
                let generation = global
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| Error::Resource("settings generation exhausted".into()))?;
                update(&mut global.values, change);
                global.generation = generation;
            }
            SettingScope::Session => {
                query.check()?;
                let generation = self
                    .session_generation
                    .checked_add(1)
                    .ok_or_else(|| Error::Resource("settings generation exhausted".into()))?;
                update(&mut self.session, change);
                self.session_generation = generation;
            }
        }
        Ok(())
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn update(values: &mut SettingValues, change: &SettingChange) {
    if change.name() == "profiling_mode" {
        // DuckDB implements these callbacks over one ClientConfig: changing
        // the mode enables profiling, while resetting it clears that shared
        // state. Keep the compatibility entries coherent without exposing a
        // stale renderer from a previous transition.
        match change.value() {
            Some(Value::Varchar(_)) => {
                if matches!(values.get("enable_profiling"), Some(Value::Null)) {
                    values.remove("enable_profiling");
                }
            }
            None => {
                values.insert("enable_profiling".into(), Value::Null);
            }
            Some(Value::Null) => {}
            Some(_) => unreachable!("validated profiling_mode value"),
        }
    } else if change.name() == "enable_profiling"
        && matches!(change.value(), Some(Value::Varchar(_)))
        && matches!(values.get("profiling_mode"), Some(Value::Null))
    {
        values.remove("profiling_mode");
    }
    if let Some(value) = change.value() {
        values.insert(change.name().into(), value.clone());
    } else {
        values.remove(change.name());
    }
}
#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
fn poisoned() -> Error {
    Error::Internal("configuration lock poisoned".into())
}
