//! Database and named-connection lifecycle for the integrated SQLLogic runner.

use duckdb_rust::{Connection, Database, Error, Result};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub(crate) struct Sessions {
    database: Option<Database>,
    named_databases: BTreeMap<String, Database>,
    path: Option<PathBuf>,
    read_only: bool,
    scratch: PathBuf,
    connections: BTreeMap<String, (String, Connection)>,
}

#[cfg_attr(feature = "dev", duckdb_dev::instrument)]
impl Sessions {
    pub(crate) fn new(database: &Database, scratch: &Path) -> Self {
        Self {
            database: Some(database.clone()),
            named_databases: BTreeMap::new(),
            path: None,
            read_only: false,
            scratch: scratch.to_path_buf(),
            connections: BTreeMap::new(),
        }
    }

    pub(crate) fn database(&self) -> &Database {
        self.database.as_ref().expect("session database is open")
    }

    pub(crate) fn scratch(&self) -> &Path {
        &self.scratch
    }

    pub(crate) fn connection(&mut self, name: &str) -> Result<&mut Connection> {
        let (connection_name, target) = if name.contains(':') {
            let mut parts = name.split(':');
            let database_name = parts.next().unwrap_or_default();
            let connection_name = parts.next().unwrap_or_default();
            if database_name.is_empty() || connection_name.is_empty() || parts.next().is_some() {
                return Err(Error::Execution(
                    "Expected either connection name or database:connection".into(),
                ));
            }
            if !self.named_databases.contains_key(database_name)
                && self.connections.contains_key(connection_name)
            {
                return Err(Error::Execution(
                    "Database did not exist, but named connection already existed".into(),
                ));
            }
            if !self.named_databases.contains_key(database_name) {
                self.named_databases
                    .insert(database_name.to_string(), Database::memory()?);
            }
            (connection_name, database_name)
        } else {
            (name, "")
        };
        if let Some((existing_target, _)) = self.connections.get(connection_name)
            && existing_target != target
        {
            return Err(Error::Execution(
                "Named connection has been started with different target databases".into(),
            ));
        }
        if !self.connections.contains_key(connection_name) {
            let database = if target.is_empty() {
                self.database
                    .as_ref()
                    .ok_or_else(|| Error::Execution("no open test database".into()))?
            } else {
                self.named_databases
                    .get(target)
                    .expect("named database inserted")
            };
            self.connections.insert(
                connection_name.to_string(),
                (target.to_string(), database.connect()),
            );
        }
        Ok(&mut self
            .connections
            .get_mut(connection_name)
            .expect("connection inserted")
            .1)
    }

    pub(crate) fn reconnect(&mut self) {
        // Pinned SQLLogicTestRunner::Reconnect resets only its default `con`.
        self.connections.remove("");
    }

    pub(crate) fn load(&mut self, path: Option<PathBuf>, read_only: bool) -> Result<()> {
        self.open(path, read_only, true)
    }

    pub(crate) fn restart(&mut self) -> Result<()> {
        // Dropping connections rolls back active transactions. This is required
        // by shutdown_running_transaction_updates.test before the reopen.
        // Pinned RestartCommand also restores the main connection's client
        // configuration and catalog search path after reopening the database.
        let settings = self.connection("")?.settings_snapshot()?;
        self.open(self.path.clone(), self.read_only, false)?;
        self.connection("")?.restore_settings(&settings)
    }

    fn open(&mut self, path: Option<PathBuf>, read_only: bool, fresh: bool) -> Result<()> {
        let path = path
            .filter(|path| !path.as_os_str().is_empty() && path.as_os_str() != ":memory:")
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    self.scratch.join(path)
                }
            });
        if let Some(path) = &path {
            if path
                .components()
                .any(|component| component == std::path::Component::ParentDir)
            {
                return Err(Error::Execution("test database path contains '..'".into()));
            }
            let absolute = path.clone();
            let parent = absolute
                .parent()
                .ok_or_else(|| Error::Execution("database path has no parent".into()))?;
            std::fs::create_dir_all(parent)?;
            let parent = parent.canonicalize()?;
            if !parent.starts_with(&self.scratch) {
                return Err(Error::Unsupported(
                    "test database path outside its scratch directory".into(),
                ));
            }
            if std::fs::symlink_metadata(&absolute).is_ok_and(|metadata| metadata.is_symlink()) {
                return Err(Error::Unsupported("test database path is a symlink".into()));
            }
        }

        self.connections.clear();
        self.database = None;
        if fresh
            && !read_only
            && let Some(path) = &path
        {
            for suffix in ["", ".wal", ".wal.checkpoint"] {
                let mut file = path.as_os_str().to_os_string();
                file.push(suffix);
                if let Err(error) = std::fs::remove_file(&file)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    return Err(error.into());
                }
            }
        }
        let database = match &path {
            Some(path) if read_only => Database::open_read_only(path)?,
            Some(path) => Database::open_logged(path)?,
            None if read_only => {
                return Err(Error::Unsupported(
                    "read-only in-memory test database".into(),
                ));
            }
            None => Database::memory()?,
        };
        self.database = Some(database);
        self.path = path;
        self.read_only = read_only;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duckdb_rust::Value;

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn restart_restores_main_session_and_database_settings() -> Result<()> {
        let scratch = tempfile::tempdir()?;
        let database = Database::memory()?;
        let mut sessions = Sessions::new(&database, scratch.path());
        sessions.connection("")?.execute(
            "CREATE SCHEMA analytics;
             SET search_path='analytics';
             SET SESSION default_order='DESC';
             SET GLOBAL default_null_order='FIRST'",
        )?;

        sessions.restart()?;
        assert_eq!(
            sessions
                .connection("")?
                .query(
                    "SELECT current_setting('search_path'),
                            current_setting('default_order'),
                            current_setting('default_null_order')"
                )?
                .rows,
            vec![vec![
                Value::Varchar("analytics".into()),
                Value::Varchar("DESC".into()),
                Value::Varchar("NULLS_FIRST".into()),
            ]]
        );
        assert_eq!(
            sessions
                .connection("new_named")?
                .query(
                    "SELECT current_setting('search_path'),
                            current_setting('default_order'),
                            current_setting('default_null_order')"
                )?
                .rows,
            vec![vec![
                Value::Varchar(String::new()),
                Value::Varchar("ASCENDING".into()),
                Value::Varchar("NULLS_FIRST".into()),
            ]]
        );
        Ok(())
    }

    #[cfg_attr(feature = "dev", duckdb_dev::instrument)]
    #[test]
    fn writable_file_sessions_use_a_logged_wal_before_restart() -> Result<()> {
        let scratch = tempfile::tempdir()?;
        let scratch_path = scratch.path().canonicalize()?;
        let database = Database::memory()?;
        let mut sessions = Sessions::new(&database, &scratch_path);
        let path = scratch_path.join("lifecycle.duckdb");
        sessions.load(Some(path.clone()), false)?;
        sessions
            .connection("writer")?
            .execute("CREATE TABLE state(i INTEGER); INSERT INTO state VALUES (1), (2)")?;
        sessions.connection("reader")?.execute("BEGIN")?;
        assert_eq!(
            sessions
                .connection("reader")?
                .query("SELECT count(*) FROM state")?
                .rows,
            vec![vec![Value::Integer(2)]]
        );
        sessions
            .connection("writer")?
            .execute("INSERT INTO state VALUES (3)")?;
        sessions.reconnect();
        assert_eq!(
            sessions
                .connection("reader")?
                .query("SELECT count(*) FROM state")?
                .rows,
            vec![vec![Value::Integer(2)]]
        );
        assert_eq!(
            sessions
                .connection("")?
                .query("SELECT count(*) FROM state")?
                .rows,
            vec![vec![Value::Integer(3)]]
        );
        assert!(std::fs::metadata(path.with_extension("duckdb.wal"))?.len() > 0);

        sessions.restart()?;
        assert_eq!(
            sessions
                .connection("")?
                .query("SELECT sum(i) FROM state")?
                .rows,
            vec![vec![Value::Integer(6)]]
        );

        sessions.load(Some(path.clone()), true)?;
        assert_eq!(
            sessions
                .connection("")?
                .query("SELECT sum(i) FROM state")?
                .rows,
            vec![vec![Value::Integer(6)]]
        );
        for sql in [
            "INSERT INTO state VALUES (4)",
            "CREATE TABLE blocked(i INTEGER)",
        ] {
            assert!(matches!(
                sessions.connection("")?.execute(sql),
                Err(Error::Transaction(_))
            ));
        }

        sessions.load(Some(path), false)?;
        assert!(
            sessions
                .connection("")?
                .query("SELECT * FROM state")
                .is_err()
        );
        Ok(())
    }
}
