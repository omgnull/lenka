// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The local directory storage driver. It makes no remote calls.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

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

/// The default limit of the wait for the write lock. A normal write takes milliseconds, so a
/// longer wait means that the holder has stopped or that it stores a very large file.
const DEFAULT_LOCK_WAIT: Duration = Duration::from_secs(30);

/// The largest limit that `LENKA_LOCK_WAIT_MS` can set.
const MAX_LOCK_WAIT: Duration = Duration::from_secs(3600);

/// The count of names that one write tries for a temporary file.
const TEMP_ATTEMPTS: usize = 64;

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

impl LocalDriver {
    fn open_lock_file(&self) -> Result<File> {
        let runtime = self.root.join(".runtime");
        fs::create_dir_all(&runtime)
            .map_err(|err| LenkaError::io("Create the runtime directory", &err))?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(runtime.join("lock"))
            .map_err(|err| LenkaError::io("Open the lock file", &err))
    }

    /// Refuses a path that is not a regular file. The open of a named pipe waits with no limit.
    fn check_regular_file(&self, path: &DocPath) -> Result<()> {
        match fs::symlink_metadata(self.file_of(path)) {
            Ok(meta) if !meta.is_file() => Err(LenkaError::invalid_file(format!(
                "The path {path} is not a regular file, so lenka cannot read it."
            ))),
            Ok(_) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(LenkaError::io("Read file metadata", &err)),
        }
    }
}

/// Returns the limit of the wait for the write lock. `LENKA_LOCK_WAIT_MS` can change it.
fn lock_wait_limit() -> Duration {
    parse_lock_wait(std::env::var("LENKA_LOCK_WAIT_MS").ok().as_deref())
}

fn parse_lock_wait(text: Option<&str>) -> Duration {
    text.and_then(|text| text.parse().ok())
        .map_or(DEFAULT_LOCK_WAIT, Duration::from_millis)
        .min(MAX_LOCK_WAIT)
}

/// Takes the lock if it is free, and tells if this call got it.
fn try_lock(file: &File) -> Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(err)) => Err(LenkaError::io("Take the project write lock", &err)),
    }
}

impl StorageDriver for LocalDriver {
    fn read(&self, path: &DocPath) -> Result<Option<Vec<u8>>> {
        self.check_no_symlink(path)?;
        self.check_regular_file(path)?;
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
        self.check_regular_file(path)?;
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

    fn read_start(&self, path: &DocPath, max_bytes: u64) -> Result<Vec<u8>> {
        self.check_no_symlink(path)?;
        self.check_regular_file(path)?;
        let file = match File::open(self.file_of(path)) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(LenkaError::io("Open a document", &err)),
        };
        let mut bytes = Vec::new();
        file.take(max_bytes)
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
        begin_with_limit(self, lock_wait_limit())
    }

    fn try_begin(&self) -> Result<Option<Box<dyn WriteTxn + '_>>> {
        let lock_file = self.open_lock_file()?;
        if !try_lock(&lock_file)? {
            return Ok(None);
        }
        Ok(Some(Box::new(LocalTxn {
            driver: self,
            lock_file,
            strict_flush: false,
        })))
    }

    fn journal_read(&self) -> Result<Option<Vec<u8>>> {
        // A journal has no size limit, because a large write must also survive a crash.
        let path = journal_path()?;
        self.check_no_symlink(&path)?;
        self.check_regular_file(&path)?;
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

fn begin_with_limit(driver: &LocalDriver, limit: Duration) -> Result<Box<dyn WriteTxn + '_>> {
    let lock_file = driver.open_lock_file()?;
    let start = Instant::now();
    let mut pause = Duration::from_millis(1);
    while !try_lock(&lock_file)? {
        if start.elapsed() >= limit {
            return Err(LenkaError::new(
                ErrorCode::Busy,
                format!(
                    "Another process has held the project write lock for {} seconds. Try again later, or stop that process.",
                    limit.as_secs()
                ),
            ));
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(50));
    }
    Ok(Box::new(LocalTxn {
        driver,
        lock_file,
        strict_flush: false,
    }))
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
            let new_parts = if self.strict_flush {
                missing_parents(parent)
            } else {
                Vec::new()
            };
            fs::create_dir_all(parent).map_err(|err| LenkaError::io("Create a directory", &err))?;
            // A new directory stays after a power failure only if its parent has a flush.
            for part in new_parts {
                if let Some(above) = part.parent() {
                    sync_dir(above);
                }
            }
        }
        Ok(())
    }
}

/// Returns the parts of a path that do not exist, with the deepest part first.
fn missing_parents(dir: &Path) -> Vec<PathBuf> {
    dir.ancestors()
        .take_while(|part| !part.exists())
        .map(Path::to_path_buf)
        .collect()
}

/// Flushes a directory, so that a rename stays after a power failure. Windows cannot do this.
fn sync_dir(dir: &Path) {
    if cfg!(unix)
        && let Ok(handle) = File::open(dir)
    {
        let _ = handle.sync_all();
    }
}

/// Creates a new temporary file in a directory. The file cannot exist already, so this call never
/// writes through a symlink that another process put at that name.
fn create_temp(parent: &Path) -> std::io::Result<(PathBuf, File)> {
    let mut last = None;
    for _ in 0..TEMP_ATTEMPTS {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".tmp-{}-{counter}", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(err) if err.kind() == ErrorKind::AlreadyExists => last = Some(err),
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::from(ErrorKind::AlreadyExists)))
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

    fn check_file_path(&self, path: &DocPath) -> Result<()> {
        self.driver.check_no_symlink(path)?;
        match fs::symlink_metadata(self.driver.file_of(path)) {
            Ok(meta) if !meta.is_file() => Err(LenkaError::invalid_file(format!(
                "The path {path} is not a regular file, so lenka cannot write it. Remove that path."
            ))),
            Ok(_) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(LenkaError::io("Read file metadata", &err)),
        }
    }

    fn put(&mut self, path: &DocPath, bytes: &[u8]) -> Result<()> {
        self.driver.check_no_symlink(path)?;
        let file = self.driver.file_of(path);
        self.ensure_parent(&file)?;
        let parent = file.parent().unwrap_or(&self.driver.root);
        let (temp, mut handle) =
            create_temp(parent).map_err(|err| LenkaError::io("Create a temporary file", &err))?;
        let result = (|| {
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
    use lenka_core::Op;
    use lenka_core::journal;

    /// Writes a journal, and replays it as the next lock holder does.
    fn replay(driver: &LocalDriver, lines: &[&str]) -> Result<()> {
        let ops: Vec<Op> = lines
            .iter()
            .map(|line| Op::Append {
                path: "feed.jsonl".to_owned(),
                line: (*line).to_owned(),
            })
            .collect();
        let bytes =
            serde_json::to_vec(&ops).map_err(|err| LenkaError::invalid_file(err.to_string()))?;
        let mut txn = driver.begin()?;
        txn.journal_write(&bytes)?;
        journal::repair(driver, txn.as_mut())?;
        Ok(())
    }

    fn feed_text(temp: &tempfile::TempDir) -> String {
        fs::read_to_string(temp.path().join("feed.jsonl")).unwrap_or_default()
    }

    #[test]
    fn a_replay_keeps_a_line_apart_from_a_torn_line() -> Result<()> {
        let (temp, driver) = driver()?;
        fs::write(temp.path().join("feed.jsonl"), "old\nnew li")
            .map_err(|err| LenkaError::io("t", &err))?;
        replay(&driver, &["new line"])?;
        assert_eq!(feed_text(&temp), "old\nnew li\nnew line\n");
        Ok(())
    }

    #[test]
    fn a_replay_adds_only_the_lines_that_a_stopped_write_did_not_add() -> Result<()> {
        for (start, expected) in [
            ("old\n", "old\nx\ny\n"),
            ("old\nx\n", "old\nx\ny\n"),
            ("old\nx\ny\n", "old\nx\ny\n"),
            ("old\nx\ny", "old\nx\ny\ny\n"),
            ("", "x\ny\n"),
        ] {
            let (temp, driver) = driver()?;
            fs::write(temp.path().join("feed.jsonl"), start)
                .map_err(|err| LenkaError::io("t", &err))?;
            replay(&driver, &["x", "y"])?;
            assert_eq!(feed_text(&temp), expected, "from {start:?}");
        }
        Ok(())
    }

    /// A process that holds the lock must not stop other processes with no limit.
    #[test]
    fn a_held_lock_gives_a_busy_error_and_try_begin_gives_none() -> Result<()> {
        let (temp, driver) = driver()?;
        let other = LocalDriver::open(temp.path())?;
        let _held = other.begin()?;
        assert!(driver.try_begin()?.is_none());
        let limit = Duration::from_millis(50);
        let start = Instant::now();
        let error = begin_with_limit(&driver, limit)
            .err()
            .map(|error| error.code());
        assert_eq!(error, Some(ErrorCode::Busy));
        assert!(start.elapsed() >= limit);
        Ok(())
    }

    #[test]
    fn a_free_lock_is_available_after_a_wait() -> Result<()> {
        let (temp, driver) = driver()?;
        let other = LocalDriver::open(temp.path())?;
        let held = other.begin()?;
        std::thread::scope(|scope| {
            let waiter =
                scope.spawn(|| begin_with_limit(&driver, Duration::from_secs(20)).map(|_| ()));
            std::thread::sleep(Duration::from_millis(30));
            drop(held);
            waiter
                .join()
                .map_err(|_| LenkaError::invalid_argument("a panic"))?
        })?;
        assert!(driver.try_begin()?.is_some());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_named_pipe_gives_an_error_and_no_wait() -> Result<()> {
        let (temp, driver) = driver()?;
        let path = temp.path().join("pipe");
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .map_err(|err| LenkaError::io("t", &err))?;
        assert!(made.success());
        let doc = DocPath::new("pipe")?;
        assert_eq!(
            driver.read(&doc).err().map(|e| e.code()),
            Some(ErrorCode::InvalidFile)
        );
        assert_eq!(
            driver.read_from(&doc, 0).err().map(|e| e.code()),
            Some(ErrorCode::InvalidFile)
        );
        assert_eq!(
            driver.read_start(&doc, 10).err().map(|e| e.code()),
            Some(ErrorCode::InvalidFile)
        );
        let journal = temp.path().join(journal_path()?.as_str());
        fs::create_dir_all(
            journal
                .parent()
                .ok_or_else(|| LenkaError::not_found("parent"))?,
        )
        .map_err(|err| LenkaError::io("t", &err))?;
        let made = std::process::Command::new("mkfifo")
            .arg(&journal)
            .status()
            .map_err(|err| LenkaError::io("t", &err))?;
        assert!(made.success());
        assert_eq!(
            driver.journal_read().err().map(|e| e.code()),
            Some(ErrorCode::InvalidFile)
        );
        Ok(())
    }

    #[test]
    fn the_wait_limit_of_the_environment_has_a_maximum() {
        assert_eq!(parse_lock_wait(None), DEFAULT_LOCK_WAIT);
        assert_eq!(parse_lock_wait(Some("not a number")), DEFAULT_LOCK_WAIT);
        assert_eq!(parse_lock_wait(Some("25")), Duration::from_millis(25));
        assert_eq!(parse_lock_wait(Some("99999999999")), MAX_LOCK_WAIT);
    }

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

    #[cfg(unix)]
    #[test]
    fn a_temporary_file_never_follows_a_planted_symlink()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let outside = temp.path().join("outside");
        fs::write(&outside, "keep")?;
        let root = temp.path().join("data");
        fs::create_dir_all(&root)?;
        let next = TEMP_COUNTER.load(Ordering::Relaxed);
        for counter in next..next + 8 {
            let name = format!(".tmp-{}-{counter}", std::process::id());
            std::os::unix::fs::symlink(&outside, root.join(name))?;
        }
        let (planted, mut file) = create_temp(&root)?;
        file.write_all(b"new")?;
        assert_eq!(fs::read_to_string(&outside)?, "keep");
        assert_eq!(fs::read_to_string(planted)?, "new");
        Ok(())
    }

    #[test]
    fn a_directory_at_a_file_path_fails_before_the_journal()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        fs::create_dir_all(temp.path().join("tasks/blocked.md/x"))?;
        let driver = LocalDriver::open(temp.path())?;
        let txn = driver.begin()?;
        let error = txn
            .check_file_path(&DocPath::new("tasks/blocked.md")?)
            .err()
            .ok_or("the check passed")?;
        assert_eq!(error.code(), ErrorCode::InvalidFile);
        assert!(txn.check_file_path(&DocPath::new("tasks/new.md")?).is_ok());
        Ok(())
    }
}
