// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! An open project: its settings, driver, artifact store, and write path.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lenka_plugin_api::{
    ArtifactStore, DocPath, DocStat, ErrorCode, LenkaError, ProjectId, Result, StorageDriver,
    WriteTxn,
};

use crate::clock::Clock;
use crate::host::{EventHook, Peers};
use crate::journal::{self, CrashPoint, Op};
use crate::notices::ProjectEvent;
use crate::secret::{self, SecretPolicy, TextCheck};
use crate::settings::{PROJECT_FILE, ProjectConfig};

/// A group of document changes with its checks.
#[derive(Debug, Default)]
pub struct Commit {
    /// The texts for the secret check.
    pub checks: Vec<TextCheck>,
    /// The policy of the secret check.
    pub secret_policy: SecretPolicy,
    /// The document changes.
    pub ops: Vec<Op>,
}

/// The write lock of a project, with its checks.
#[derive(Debug)]
pub struct Locked<'a> {
    txn: Box<dyn WriteTxn + 'a>,
    project: &'a Project,
}

impl Locked<'_> {
    /// Checks the version that the caller read against the current version.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::StaleVersion`] if the two versions differ.
    pub fn check_version(&self, what: &str, current: u64, expected: u64) -> Result<()> {
        if current == expected {
            return Ok(());
        }
        Err(LenkaError::new(
            ErrorCode::StaleVersion,
            format!(
                "{what} is at version {current}, and the call used version {expected}. Read it again, and repeat the change on the new version."
            ),
        ))
    }

    /// Runs the secret check, and then applies the changes through the journal.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::SecretDetected`] if the check finds a pattern, and nothing is written.
    /// Returns an error if a write fails. The next lock holder repairs the data.
    pub fn commit(&mut self, commit: &Commit) -> Result<()> {
        secret::check(&commit.checks, commit.secret_policy)?;
        journal::run(self.txn.as_mut(), &commit.ops, &self.project.crash)
    }
}

/// An open project.
#[derive(Debug)]
pub struct Project {
    config: Mutex<Arc<ProjectConfig>>,
    config_stamp: Mutex<Option<DocStat>>,
    driver: Arc<dyn StorageDriver>,
    artifacts: Arc<dyn ArtifactStore>,
    clock: Arc<dyn Clock>,
    hooks: Vec<Arc<dyn EventHook>>,
    peers: Arc<Peers>,
    extensions: Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    crash: CrashPoint,
}

impl Project {
    /// Opens a project with a driver and a store, and repairs an unfinished write.
    ///
    /// # Errors
    ///
    /// Returns an error if the settings are missing or not valid.
    pub fn open(
        driver: Arc<dyn StorageDriver>,
        artifacts: Arc<dyn ArtifactStore>,
        clock: Arc<dyn Clock>,
        hooks: Vec<Arc<dyn EventHook>>,
        peers: Arc<Peers>,
    ) -> Result<Arc<Self>> {
        let config = read_config(driver.as_ref())?;
        let stamp = driver.stat(&DocPath::new(PROJECT_FILE)?)?;
        let project = Arc::new(Self {
            config: Mutex::new(Arc::new(config)),
            config_stamp: Mutex::new(stamp),
            driver,
            artifacts,
            clock,
            hooks,
            peers,
            extensions: Mutex::new(HashMap::new()),
            crash: CrashPoint::default(),
        });
        project.repair_if_needed()?;
        Ok(project)
    }

    /// Replays a journal if a stopped process left one.
    ///
    /// # Errors
    ///
    /// Returns an error if the repair fails.
    pub fn repair_if_needed(&self) -> Result<()> {
        if self.driver.journal_read()?.is_some() {
            let mut txn = self.driver.begin()?;
            journal::repair(self.driver.as_ref(), txn.as_mut())?;
        }
        Ok(())
    }

    /// Returns the project ID.
    #[must_use]
    pub fn id(&self) -> ProjectId {
        self.config().id
    }

    /// Returns the project name.
    #[must_use]
    pub fn name(&self) -> String {
        self.config().name.clone()
    }

    /// Returns the current settings.
    #[must_use]
    pub fn config(&self) -> Arc<ProjectConfig> {
        Arc::clone(&lock(&self.config))
    }

    /// Reads the settings from the driver again.
    ///
    /// # Errors
    ///
    /// Returns an error if the settings are not valid. The old settings stay.
    pub fn reload_config(&self) -> Result<()> {
        let config = read_config(self.driver.as_ref())?;
        *lock(&self.config) = Arc::new(config);
        Ok(())
    }

    /// Reads the settings again if the file changed, because an admin can edit it at any time. An
    /// invalid file keeps the old settings, and the next `lenka doctor` reports it.
    pub fn reload_config_if_changed(&self) {
        let Ok(path) = DocPath::new(PROJECT_FILE) else {
            return;
        };
        let Ok(stamp) = self.driver.stat(&path) else {
            return;
        };
        let mut known = lock(&self.config_stamp);
        if *known == stamp {
            return;
        }
        *known = stamp;
        match read_config(self.driver.as_ref()) {
            Ok(config) => *lock(&self.config) = Arc::new(config),
            Err(error) => tracing::warn!(error = %error, "kept the old project settings"),
        }
    }

    /// Saves new settings through the journal.
    ///
    /// # Errors
    ///
    /// Returns an error if the settings are not valid or the write fails.
    pub fn save_config(&self, config: ProjectConfig) -> Result<()> {
        config.validate()?;
        let text = config.to_text()?;
        self.write_locked(|locked| {
            locked.commit(&Commit {
                ops: vec![Op::Put {
                    path: PROJECT_FILE.to_owned(),
                    text: text.clone(),
                }],
                ..Commit::default()
            })
        })?;
        *lock(&self.config) = Arc::new(config);
        Ok(())
    }

    /// Returns the storage driver.
    #[must_use]
    pub fn driver(&self) -> &dyn StorageDriver {
        self.driver.as_ref()
    }

    /// Returns the artifact store.
    #[must_use]
    pub fn artifacts(&self) -> &dyn ArtifactStore {
        self.artifacts.as_ref()
    }

    /// Returns the clock.
    #[must_use]
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// Returns the lookup for other projects.
    #[must_use]
    pub fn peers(&self) -> &Peers {
        &self.peers
    }

    /// Stores a cache document that a rebuild can create again. It skips the journal.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn put_cache(&self, path: &DocPath, bytes: &[u8]) -> Result<()> {
        self.driver.begin()?.put(path, bytes)
    }

    /// Returns the test control for stops in a write.
    #[must_use]
    pub fn crash_point(&self) -> &CrashPoint {
        &self.crash
    }

    /// Takes the write lock, repairs an unfinished write, and runs the closure.
    ///
    /// # Errors
    ///
    /// Returns the error of the closure, or an error from the lock or the repair.
    pub fn write_locked<T>(&self, work: impl FnOnce(&mut Locked<'_>) -> Result<T>) -> Result<T> {
        let mut txn = self.driver.begin()?;
        txn.set_strict_flush(self.config().limits.strict_flush);
        journal::repair(self.driver.as_ref(), txn.as_mut())?;
        let mut locked = Locked { txn, project: self };
        work(&mut locked)
    }

    /// Sends an event to the plugins. A plugin failure gives a warning text, not an error, because
    /// the data write is already complete.
    #[must_use]
    pub fn emit(&self, event: &ProjectEvent) -> Vec<String> {
        self.hooks
            .iter()
            .filter_map(|hook| hook.on_event(self, event).err())
            .map(|err| err.to_string())
            .collect()
    }

    /// Returns a per-project service, and creates it on first use.
    ///
    /// # Errors
    ///
    /// Returns the error of the create function.
    pub fn extension<T: Any + Send + Sync>(
        self: &Arc<Self>,
        create: impl FnOnce(&Arc<Self>) -> Result<T>,
    ) -> Result<Arc<T>> {
        let key = TypeId::of::<T>();
        if let Some(found) = lock(&self.extensions).get(&key) {
            return Arc::clone(found)
                .downcast::<T>()
                .map_err(|_| extension_error());
        }
        let created = Arc::new(create(self)?);
        let mut map = lock(&self.extensions);
        let entry = map
            .entry(key)
            .or_insert_with(|| Arc::clone(&created) as Arc<dyn Any + Send + Sync>);
        Arc::clone(entry)
            .downcast::<T>()
            .map_err(|_| extension_error())
    }
}

fn extension_error() -> LenkaError {
    LenkaError::new(ErrorCode::Io, "A project service has the wrong type.")
}

fn read_config(driver: &dyn StorageDriver) -> Result<ProjectConfig> {
    let path = DocPath::new(PROJECT_FILE)?;
    let bytes = driver
        .read(&path)?
        .ok_or_else(|| LenkaError::not_found("The project directory has no lenka.toml."))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| LenkaError::invalid_file("lenka.toml is not valid UTF-8."))?;
    ProjectConfig::parse(&text)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
