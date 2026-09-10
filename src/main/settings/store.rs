use super::*;
use std::sync::{Mutex, RwLock};

/// Copy-on-write global publications; read snapshots retain an Arc generation.
#[derive(Debug)]
pub struct SnapshotConfiguration {
    registry: Arc<SettingRegistry>,
    global: Arc<RwLock<Arc<SettingValues>>>,
}
impl Default for SnapshotConfiguration {
    fn default() -> Self {
        Self::new(Arc::new(SettingRegistry::builtins()))
    }
}
impl SnapshotConfiguration {
    pub fn new(registry: Arc<SettingRegistry>) -> Self {
        Self {
            registry,
            global: Arc::new(RwLock::new(Arc::new(BTreeMap::new()))),
        }
    }
}
impl Configuration for SnapshotConfiguration {
    fn name(&self) -> &'static str {
        "snapshot-configuration"
    }
    fn connect(&self) -> Box<dyn ConfigurationSession> {
        Box::new(SnapshotSession {
            registry: self.registry.clone(),
            global: self.global.clone(),
            session: Arc::new(BTreeMap::new()),
        })
    }
}
struct SnapshotSession {
    registry: Arc<SettingRegistry>,
    global: Arc<RwLock<Arc<SettingValues>>>,
    session: Arc<SettingValues>,
}
impl ConfigurationSession for SnapshotSession {
    fn snapshot(&self, query: &QueryContext) -> Result<SettingsSnapshot> {
        query.check()?;
        let global = self.global.read().map_err(|_| poisoned())?.clone();
        SettingsSnapshot::new(self.registry.clone(), global, self.session.clone(), query)
    }
    fn apply(&mut self, change: &SettingChange, query: &QueryContext) -> Result<()> {
        change.validate(&self.registry, query)?;
        match change.scope() {
            SettingScope::Global => {
                let mut global = self.global.write().map_err(|_| poisoned())?;
                query.check()?;
                update(Arc::make_mut(&mut global), change);
            }
            SettingScope::Session => {
                query.check()?;
                update(Arc::make_mut(&mut self.session), change);
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
    global: Arc<Mutex<SettingValues>>,
}
impl Default for LockedConfiguration {
    fn default() -> Self {
        Self::new(Arc::new(SettingRegistry::builtins()))
    }
}
impl LockedConfiguration {
    pub fn new(registry: Arc<SettingRegistry>) -> Self {
        Self {
            registry,
            global: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}
impl Configuration for LockedConfiguration {
    fn name(&self) -> &'static str {
        "locked-configuration"
    }
    fn connect(&self) -> Box<dyn ConfigurationSession> {
        Box::new(LockedSession {
            registry: self.registry.clone(),
            global: self.global.clone(),
            session: BTreeMap::new(),
        })
    }
}
struct LockedSession {
    registry: Arc<SettingRegistry>,
    global: Arc<Mutex<SettingValues>>,
    session: SettingValues,
}
impl ConfigurationSession for LockedSession {
    fn snapshot(&self, query: &QueryContext) -> Result<SettingsSnapshot> {
        query.check()?;
        let global = self.global.lock().map_err(|_| poisoned())?.clone();
        SettingsSnapshot::new(
            self.registry.clone(),
            Arc::new(global),
            Arc::new(self.session.clone()),
            query,
        )
    }
    fn apply(&mut self, change: &SettingChange, query: &QueryContext) -> Result<()> {
        change.validate(&self.registry, query)?;
        match change.scope() {
            SettingScope::Global => {
                let mut global = self.global.lock().map_err(|_| poisoned())?;
                query.check()?;
                update(&mut global, change);
            }
            SettingScope::Session => {
                query.check()?;
                update(&mut self.session, change);
            }
        }
        Ok(())
    }
}
fn update(values: &mut SettingValues, change: &SettingChange) {
    if let Some(value) = change.value() {
        values.insert(change.name().into(), value.clone());
    } else {
        values.remove(change.name());
    }
}
fn poisoned() -> Error {
    Error::Internal("configuration lock poisoned".into())
}
