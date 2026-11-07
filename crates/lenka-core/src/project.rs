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

/// The count of tries of a repair in a read, with 10 milliseconds between them.
const REPAIR_TRIES: usize = 20;

/// The age after which a temporary file or an artifact directory with no record has no writer.
pub const LEFTOVER_AGE: std::time::Duration = std::time::Duration::from_secs(600);

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
    format_refusal: Mutex<Option<LenkaError>>,
    driver: Arc<dyn StorageDriver>,
    artifacts: Arc<dyn ArtifactStore>,
    clock: Arc<dyn Clock>,
    hooks: Vec<Arc<dyn EventHook>>,
    peers: Arc<Peers>,
    extensions: Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    crash: CrashPoint,
}

impl Project {
    /// Opens a project with a driver and a store, and tries to repair an unfinished write. If the
    /// repair fails, reads and `doctor` still work, and each write returns the repair error.
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
            format_refusal: Mutex::new(None),
            driver,
            artifacts,
            clock,
            hooks,
            peers,
            extensions: Mutex::new(HashMap::new()),
            crash: CrashPoint::default(),
        });
        if let Err(error) = project.repair_if_needed() {
            tracing::warn!(error = %error, "an unfinished write is not repaired");
        }
        Ok(project)
    }

    /// Replays a journal if a stopped process left one.
    ///
    /// # Errors
    ///
    /// Returns an error if the repair fails.
    pub fn repair_if_needed(&self) -> Result<()> {
        // A live writer finishes in milliseconds, and a stopped holder must not stop a read. The
        // next write repairs what this read did not repair.
        for _ in 0..REPAIR_TRIES {
            if self.driver.journal_read()?.is_none() {
                return Ok(());
            }
            if let Some(mut txn) = self.driver.try_begin()? {
                txn.set_strict_flush(self.config().limits.strict_flush);
                journal::repair(self.driver.as_ref(), txn.as_mut())?;
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        tracing::warn!("another process holds the write lock, so a read skipped a repair");
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
            Ok(config) => {
                *lock(&self.config) = Arc::new(config);
                *lock(&self.format_refusal) = None;
            }
            Err(error) if error.code() == ErrorCode::FormatTooNew => {
                *lock(&self.format_refusal) = Some(error);
            }
            Err(error) => tracing::warn!(error = %error, "kept the old project settings"),
        }
    }

    /// Returns the temporary files that a stopped process left, with paths from the data
    /// directory. A writer changes its file all the time, so an old file has no writer.
    #[must_use]
    pub fn old_temp_files(&self) -> Vec<String> {
        let Some(root) = self.driver.local_dir() else {
            return Vec::new();
        };
        let mut found = Vec::new();
        collect_old_temp_files(root, root, &mut found);
        found.sort();
        found
    }

    /// Removes the temporary files that a stopped process left, under the write lock. These files
    /// are not data, so this write does not use the journal.
    ///
    /// # Errors
    ///
    /// Returns an error if the lock fails or a file cannot be removed.
    pub fn remove_old_temp_files(&self) -> Result<Vec<String>> {
        self.write_locked(|_locked| {
            let found = self.old_temp_files();
            let Some(root) = self.driver.local_dir() else {
                return Ok(found);
            };
            for path in &found {
                std::fs::remove_file(root.join(path))
                    .map_err(|err| LenkaError::io("Remove a temporary file", &err))?;
            }
            Ok(found)
        })
    }

    /// Saves settings that a person edited from `base`.
    ///
    /// A change of the file after the load of `base` stops the save, because the save would
    /// overwrite this change. Tables that this version does not know stay in the file.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::StaleVersion`] if the file differs from `base`, or an error if the
    /// settings are not valid or the write fails.
    pub fn save_config_over(&self, base: &ProjectConfig, config: ProjectConfig) -> Result<()> {
        config.validate()?;
        self.write_locked(|locked| {
            let path = DocPath::new(PROJECT_FILE)?;
            let bytes = self
                .driver
                .read(&path)?
                .ok_or_else(|| LenkaError::not_found("The project directory has no lenka.toml."))?;
            let old = String::from_utf8(bytes)
                .map_err(|_| LenkaError::invalid_file("lenka.toml is not valid UTF-8."))?;
            if ProjectConfig::parse(&old)? != *base {
                return Err(LenkaError::new(
                    ErrorCode::StaleVersion,
                    "Another writer changed lenka.toml after these settings loaded.",
                ));
            }
            locked.commit(&Commit {
                ops: vec![Op::Put {
                    path: PROJECT_FILE.to_owned(),
                    text: config.to_text_over(&old)?,
                }],
                ..Commit::default()
            })
        })?;
        *lock(&self.config) = Arc::new(config);
        Ok(())
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

    /// Stores a cache document that a rebuild can create again. It skips the journal. A busy lock
    /// skips the store, because a read must not wait for a writer.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn put_cache(&self, path: &DocPath, bytes: &[u8]) -> Result<()> {
        match self.driver.try_begin()? {
            Some(mut txn) => txn.put(path, bytes),
            None => Ok(()),
        }
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
        self.reload_config_if_changed();
        if let Some(error) = lock(&self.format_refusal).clone() {
            return Err(error);
        }
        txn.set_strict_flush(self.config().limits.strict_flush);
        journal::repair(self.driver.as_ref(), txn.as_mut())?;
        let mut locked = Locked { txn, project: self };
        work(&mut locked)
    }

    /// Tells if a plugin listens to the events of this project.
    #[must_use]
    pub fn has_event_hooks(&self) -> bool {
        !self.hooks.is_empty()
    }

    /// Sends an event to the plugins. A plugin failure gives a warning text, because
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

/// Tells if a file or a directory had no change for the time in [`LEFTOVER_AGE`].
#[must_use]
pub fn is_leftover(meta: &std::fs::Metadata) -> bool {
    meta.modified()
        .ok()
        .and_then(|time| time.elapsed().ok())
        .is_some_and(|age| age >= LEFTOVER_AGE)
}

fn collect_old_temp_files(root: &std::path::Path, dir: &std::path::Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let (name, path) = (entry.file_name(), entry.path());
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if name != ".git" {
                collect_old_temp_files(root, &path, found);
            }
        } else if name
            .to_string_lossy()
            .starts_with(crate::layout::TEMP_PREFIX)
            && is_leftover(&meta)
            && let Ok(relative) = path.strip_prefix(root)
        {
            found.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
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
