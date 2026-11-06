// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The local directory storage driver. It makes no remote calls.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::UNIX_EPOCH;

use lenka_core::{Plugin, PluginInfo, Registry};
use lenka_plugin_api::{DocPath, DocStat, ErrorCode, LenkaError, Result, StorageDriver, WriteTxn};

/// The plugin of this driver.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalDriverPlugin;

impl Plugin for LocalDriverPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "driver-local",
            description: "Local directory storage, with no remote calls",
            sensitive: false,
        }
    }

    fn register(&self, registry: &mut Registry) {
        registry.driver("local", |root| {
            Ok(Arc::new(LocalDriver::open(root)?) as Arc<dyn StorageDriver>)
        });
    }
}

/// Returns the plugin of this driver.
#[must_use]
pub fn plugin() -> LocalDriverPlugin {
    LocalDriverPlugin
}

/// The path of the journal in the data directory.
const JOURNAL_PATH: &str = ".runtime/pending.json";

/// The largest document that the driver reads.
pub const MAX_DOCUMENT_BYTES: u64 = 16 * 1024 * 1024;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A storage driver on a local directory.
#[derive(Debug)]
pub struct LocalDriver {
    root: PathBuf,
}

impl LocalDriver {
    /// Creates a driver for a data directory.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] if the directory does not exist, and
    /// [`ErrorCode::InvalidFile`] if it is a symlink.
    pub fn open(root: &Path) -> Result<Self> {
        let meta = fs::symlink_metadata(root).map_err(|err| {
            LenkaError::new(
                ErrorCode::NotFound,
                format!("The data directory is not available: {err}. Check the project entry in projects.toml."),
            )
        })?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(LenkaError::invalid_file(
                "The data directory must be a real directory, not a symlink.",
            ));
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    fn file_of(&self, path: &DocPath) -> PathBuf {
        self.root.join(path.as_str())
    }

    /// Refuses a symlink at any part of the path. Lenka refuses symlinks in the data directory.
    fn check_no_symlink(&self, path: &DocPath) -> Result<()> {
        let mut current = self.root.clone();
        for part in path.as_str().split('/') {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(LenkaError::invalid_file(format!(
                        "The path {path} holds a symlink, and lenka refuses symlinks in the data directory. Remove the symlink."
                    )));
                }
                Ok(_) => {}
                Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
                Err(err) => return Err(LenkaError::io("Read file metadata", &err)),
            }
        }
        Ok(())
    }
}

impl StorageDriver for LocalDriver {
    fn read(&self, path: &DocPath) -> Result<Option<Vec<u8>>> {
        self.check_no_symlink(path)?;
        let file = match File::open(self.file_of(path)) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(LenkaError::io("Open a document", &err)),
        };
        let mut bytes = Vec::new();
        let read = file
            .take(MAX_DOCUMENT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| LenkaError::io("Read a document", &err))?;
        if read as u64 > MAX_DOCUMENT_BYTES {
            return Err(LenkaError::invalid_file(format!(
                "The document {path} is larger than the limit of {MAX_DOCUMENT_BYTES} bytes."
            )));
        }
        Ok(Some(bytes))
    }

    fn read_from(&self, path: &DocPath, offset: u64) -> Result<Vec<u8>> {
        self.check_no_symlink(path)?;
        let mut file = match File::open(self.file_of(path)) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(LenkaError::io("Open a document", &err)),
        };
        file.seek(SeekFrom::Start(offset))
            .map_err(|err| LenkaError::io("Seek in a document", &err))?;
        let mut bytes = Vec::new();
        file.take(MAX_DOCUMENT_BYTES * 4)
            .read_to_end(&mut bytes)
            .map_err(|err| LenkaError::io("Read a document", &err))?;
        Ok(bytes)
    }

    fn list(&self, dir: &DocPath) -> Result<Vec<String>> {
        self.check_no_symlink(dir)?;
        let reader = match fs::read_dir(self.file_of(dir)) {
            Ok(reader) => reader,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(LenkaError::io("List a directory", &err)),
        };
        let mut names = Vec::new();
        for item in reader {
            let item = item.map_err(|err| LenkaError::io("List a directory", &err))?;
            let is_symlink = item.file_type().is_ok_and(|kind| kind.is_symlink());
            if is_symlink {
                tracing::warn!(
                    dir = dir.as_str(),
                    "skipped a symlink in the data directory"
                );
                continue;
            }
            if let Ok(name) = item.file_name().into_string() {
                names.push(name);
            }
        }
        names.sort_unstable();
        Ok(names)
    }

    fn stat(&self, path: &DocPath) -> Result<Option<DocStat>> {
        match fs::symlink_metadata(self.file_of(path)) {
            Ok(meta) => {
                let mtime_ns = meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map_or(0, |value| {
                        i64::try_from(value.as_nanos()).unwrap_or(i64::MAX)
                    });
                Ok(Some(DocStat {
                    len: meta.len(),
                    mtime_ns,
                }))
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(LenkaError::io("Read file metadata", &err)),
        }
    }

    fn begin(&self) -> Result<Box<dyn WriteTxn + '_>> {
        let runtime = self.root.join(".runtime");
        fs::create_dir_all(&runtime)
            .map_err(|err| LenkaError::io("Create the runtime directory", &err))?;
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(runtime.join("lock"))
            .map_err(|err| LenkaError::io("Open the lock file", &err))?;
        lock_file
            .lock()
            .map_err(|err| LenkaError::io("Take the project write lock", &err))?;
        Ok(Box::new(LocalTxn {
            driver: self,
            lock_file,
            strict_flush: false,
        }))
    }

    fn journal_read(&self) -> Result<Option<Vec<u8>>> {
        // A journal has no size limit, because a large write must also survive a crash.
        let path = journal_path()?;
        self.check_no_symlink(&path)?;
        match fs::read(self.file_of(&path)) {
            Ok(bytes) if bytes.is_empty() => Ok(None),
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(LenkaError::io("Read the journal", &err)),
        }
    }

    fn local_dir(&self) -> Option<&Path> {
        Some(&self.root)
    }
}

fn journal_path() -> Result<DocPath> {
    DocPath::new(JOURNAL_PATH)
}

/// A write transaction. The OS releases the lock when the file closes, also after a crash.
#[derive(Debug)]
struct LocalTxn<'a> {
    driver: &'a LocalDriver,
    lock_file: File,
    strict_flush: bool,
}

impl Drop for LocalTxn<'_> {
    fn drop(&mut self) {
        if let Err(err) = self.lock_file.unlock() {
            tracing::warn!(error = %err, "could not release the project write lock");
        }
    }
}

impl LocalTxn<'_> {
    fn ensure_parent(&self, file: &Path) -> Result<()> {
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).map_err(|err| LenkaError::io("Create a directory", &err))?;
        }
        Ok(())
    }
}

/// Flushes a directory, so that a rename stays after a power failure. Windows cannot do this.
fn sync_dir(dir: &Path) {
    if cfg!(unix)
        && let Ok(handle) = File::open(dir)
    {
        let _ = handle.sync_all();
    }
}

impl WriteTxn for LocalTxn<'_> {
    fn journal_write(&mut self, bytes: &[u8]) -> Result<()> {
        let path = journal_path()?;
        self.driver.check_no_symlink(&path)?;
        let file = self.driver.file_of(&path);
        let is_new = !file.exists();
        let mut handle = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&file)
            .map_err(|err| LenkaError::io("Open the journal", &err))?;
        handle
            .write_all(bytes)
            .and_then(|()| handle.sync_data())
            .map_err(|err| LenkaError::io("Write the journal", &err))?;
        if is_new && let Some(parent) = file.parent() {
            sync_dir(parent);
        }
        Ok(())
    }

    fn journal_clear(&mut self) -> Result<()> {
        let path = journal_path()?;
        self.driver.check_no_symlink(&path)?;
        let handle = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(self.driver.file_of(&path))
            .map_err(|err| LenkaError::io("Open the journal", &err))?;
        drop(handle);
        Ok(())
    }

    fn set_strict_flush(&mut self, strict: bool) {
        self.strict_flush = strict;
    }

    fn check_path(&self, path: &DocPath) -> Result<()> {
        self.driver.check_no_symlink(path)
    }

    fn put(&mut self, path: &DocPath, bytes: &[u8]) -> Result<()> {
        self.driver.check_no_symlink(path)?;
        let file = self.driver.file_of(path);
        self.ensure_parent(&file)?;
        let parent = file.parent().unwrap_or(&self.driver.root);
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".tmp-{}-{counter}", std::process::id()));
        let result = (|| {
            let mut handle = File::create(&temp)?;
            handle.write_all(bytes)?;
            if self.strict_flush {
                handle.sync_data()?;
            }
            fs::rename(&temp, &file)
        })();
        if let Err(err) = result {
            let _ = fs::remove_file(&temp);
            return Err(LenkaError::io("Write a document", &err));
        }
        if self.strict_flush {
            sync_dir(parent);
        }
        Ok(())
    }

    fn append(&mut self, path: &DocPath, bytes: &[u8]) -> Result<()> {
        self.driver.check_no_symlink(path)?;
        let file = self.driver.file_of(path);
        self.ensure_parent(&file)?;
        let mut handle = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .map_err(|err| LenkaError::io("Open a document", &err))?;
        handle
            .write_all(bytes)
            .map_err(|err| LenkaError::io("Add to a document", &err))?;
        if self.strict_flush {
            handle
                .sync_data()
                .map_err(|err| LenkaError::io("Flush a document", &err))?;
        }
        Ok(())
    }

    fn remove(&mut self, path: &DocPath) -> Result<()> {
        self.driver.check_no_symlink(path)?;
        match fs::remove_file(self.driver.file_of(path)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(LenkaError::io("Remove a document", &err)),
        }
    }

    fn remove_tree(&mut self, path: &DocPath) -> Result<()> {
        self.driver.check_no_symlink(path)?;
        match fs::remove_dir_all(self.driver.file_of(path)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(LenkaError::io("Remove a directory", &err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driver() -> Result<(tempfile::TempDir, LocalDriver)> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let driver = LocalDriver::open(temp.path())?;
        Ok((temp, driver))
    }

    #[test]
    fn a_large_journal_can_be_read_back() -> Result<()> {
        let (_temp, driver) = driver()?;
        let bytes = vec![b'x'; (MAX_DOCUMENT_BYTES as usize) + 1];
        {
            let mut txn = driver.begin()?;
            txn.journal_write(&bytes)?;
        }
        assert_eq!(
            driver.journal_read()?.map(|read| read.len()),
            Some(bytes.len())
        );
        {
            let mut txn = driver.begin()?;
            txn.journal_clear()?;
        }
        assert_eq!(driver.journal_read()?, None);
        Ok(())
    }

    #[test]
    fn put_read_append_remove() -> Result<()> {
        let (_temp, driver) = driver()?;
        let doc = DocPath::new("tasks/a/task.md")?;
        assert_eq!(driver.read(&doc)?, None);
        {
            let mut txn = driver.begin()?;
            txn.put(&doc, b"one")?;
            txn.append(&DocPath::new("feed.jsonl")?, b"line\n")?;
        }
        assert_eq!(driver.read(&doc)?.as_deref(), Some(&b"one"[..]));
        assert_eq!(driver.list(&DocPath::new("tasks/a")?)?, vec!["task.md"]);
        assert_eq!(driver.read_from(&DocPath::new("feed.jsonl")?, 2)?, b"ne\n");
        {
            let mut txn = driver.begin()?;
            txn.remove(&doc)?;
            txn.remove(&doc)?;
            txn.remove_tree(&DocPath::new("tasks")?)?;
        }
        assert_eq!(driver.read(&doc)?, None);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_refused() -> Result<()> {
        let (temp, driver) = driver()?;
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, "x").map_err(|err| LenkaError::io("write", &err))?;
        std::os::unix::fs::symlink(&outside, temp.path().join("link"))
            .map_err(|err| LenkaError::io("symlink", &err))?;
        let Err(error) = driver.read(&DocPath::new("link")?) else {
            panic!("a symlink must fail");
        };
        assert_eq!(error.code(), ErrorCode::InvalidFile);
        assert!(driver.list(&DocPath::new("tasks")?)?.is_empty());
        Ok(())
    }

    #[test]
    fn a_large_document_is_refused() -> Result<()> {
        let (temp, driver) = driver()?;
        let file = File::create(temp.path().join("big")).map_err(|e| LenkaError::io("c", &e))?;
        file.set_len(MAX_DOCUMENT_BYTES + 1)
            .map_err(|e| LenkaError::io("len", &e))?;
        assert!(driver.read(&DocPath::new("big")?).is_err());
        Ok(())
    }
}
