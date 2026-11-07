// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::{LenkaError, Result};
use crate::ids::ArtifactId;

/// A validated relative path of a document in a project.
///
/// Each part holds only ASCII letters, digits, `.`, `_`, and `-`. The parts `.` and `..`
/// are not valid, and neither is `.git` in any case. A driver can therefore map this path to a
/// file without any escape, and a replay of a journal cannot change a Git configuration.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DocPath(String);

impl DocPath {
    /// Creates a path from text.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::InvalidArgument`] if a part is not valid.
    pub fn new(text: impl Into<String>) -> Result<Self> {
        let text = text.into();
        if text.is_empty() || text.len() > 1024 || !text.split('/').all(is_valid_part) {
            return Err(LenkaError::invalid_argument(
                "A document path has a part that is not valid.",
            ));
        }
        Ok(Self(text))
    }

    /// Returns this path with one more part.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::InvalidArgument`] if the part is not valid.
    pub fn join(&self, part: &str) -> Result<Self> {
        Self::new(format!("{}/{part}", self.0))
    }

    /// Returns the path as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_valid_part(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && !part.eq_ignore_ascii_case(".git")
        && !part.eq_ignore_ascii_case("git~1")
        && part.len() <= 255
        && part
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

impl fmt::Debug for DocPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DocPath({})", self.0)
    }
}

impl fmt::Display for DocPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The size and the change time of a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocStat {
    /// The size in bytes.
    pub len: u64,
    /// The change time in nanoseconds. Only equality has a meaning.
    pub mtime_ns: i64,
}

/// A write transaction. It holds the project write lock until it is dropped.
pub trait WriteTxn: fmt::Debug {
    /// Stores the journal of a write, and flushes it to the disk before it returns.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    fn journal_write(&mut self, bytes: &[u8]) -> Result<()>;

    /// Empties the journal. This needs no flush, because a replay of a complete journal is safe.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    fn journal_clear(&mut self) -> Result<()>;

    /// Selects if each `put` and `append` also flushes to the disk.
    fn set_strict_flush(&mut self, strict: bool);

    /// Checks that a later write to this path can pass, before the journal holds the write.
    ///
    /// # Errors
    ///
    /// Returns an error if the path holds a symlink.
    fn check_path(&self, path: &DocPath) -> Result<()>;

    /// Checks that a later `put` or `append` to this path can pass, before the journal holds the
    /// write.
    ///
    /// # Errors
    ///
    /// Returns an error if the path holds a symlink, or if the target is not a regular file.
    fn check_file_path(&self, path: &DocPath) -> Result<()>;

    /// Replaces a document in one atomic step.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    fn put(&mut self, path: &DocPath, bytes: &[u8]) -> Result<()>;

    /// Adds bytes to the end of a document.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    fn append(&mut self, path: &DocPath, bytes: &[u8]) -> Result<()>;

    /// Removes a document. A missing document is not an error.
    ///
    /// # Errors
    ///
    /// Returns an error if the removal fails.
    fn remove(&mut self, path: &DocPath) -> Result<()>;

    /// Removes a document directory and its contents. A missing directory is not an error.
    ///
    /// # Errors
    ///
    /// Returns an error if the removal fails.
    fn remove_tree(&mut self, path: &DocPath) -> Result<()>;
}

/// The storage contract of a project. The core runs its checks above this trait.
pub trait StorageDriver: Send + Sync + fmt::Debug {
    /// Reads a document. A missing document gives `None`.
    ///
    /// # Errors
    ///
    /// Returns an error if the document is a symlink, is too large, or cannot be read.
    fn read(&self, path: &DocPath) -> Result<Option<Vec<u8>>>;

    /// Reads the bytes of an append-only document after an offset.
    ///
    /// # Errors
    ///
    /// Returns an error if the document cannot be read.
    fn read_from(&self, path: &DocPath, offset: u64) -> Result<Vec<u8>>;

    /// Reads at most this count of bytes from the start of a document. A missing document gives an
    /// empty result.
    ///
    /// # Errors
    ///
    /// Returns an error if the document cannot be read.
    fn read_start(&self, path: &DocPath, max_bytes: u64) -> Result<Vec<u8>>;

    /// Lists the names directly under a directory, sorted. A missing directory gives no names.
    /// Symlinks are not in the result.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    fn list(&self, dir: &DocPath) -> Result<Vec<String>>;

    /// Returns the size and the change time of a document.
    ///
    /// # Errors
    ///
    /// Returns an error if the document cannot be read.
    fn stat(&self, path: &DocPath) -> Result<Option<DocStat>>;

    /// Takes the project write lock, and blocks until the lock is free.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::Busy`] if another process holds the lock for too long, or
    /// another error if the lock cannot be taken.
    fn begin(&self) -> Result<Box<dyn WriteTxn + '_>>;

    /// Starts a write transaction if the write lock is free now. A read uses this for optional
    /// work, because a read must not wait for a writer.
    ///
    /// # Errors
    ///
    /// Returns an error if the check of the lock fails.
    fn try_begin(&self) -> Result<Option<Box<dyn WriteTxn + '_>>>;

    /// Reads the journal. No journal, or an empty journal, gives `None`.
    ///
    /// # Errors
    ///
    /// Returns an error if the journal cannot be read.
    fn journal_read(&self) -> Result<Option<Vec<u8>>>;

    /// Returns the local directory of the data, for drivers that have one.
    fn local_dir(&self) -> Option<&Path>;
}

/// The result of a write to an artifact store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBytes {
    /// The size in bytes.
    pub size: u64,
    /// The SHA-256 hash, in lowercase hexadecimal.
    pub sha256: String,
}

/// The contract of an artifact store. The bytes stay as they are.
pub trait ArtifactStore: Send + Sync + fmt::Debug {
    /// Stores a version from a local file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be copied.
    fn put_file(
        &self,
        id: ArtifactId,
        version: u64,
        name: &str,
        source: &Path,
    ) -> Result<StoredBytes>;

    /// Stores a version from memory.
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes cannot be written.
    fn put_bytes(
        &self,
        id: ArtifactId,
        version: u64,
        name: &str,
        bytes: &[u8],
    ) -> Result<StoredBytes>;

    /// Reads the bytes of a version.
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes cannot be read.
    fn get(&self, id: ArtifactId, version: u64, name: &str) -> Result<Vec<u8>>;

    /// Calculates the SHA-256 hash of the bytes of a version, with a small part in memory at each
    /// time.
    ///
    /// # Errors
    ///
    /// Returns an error if the bytes cannot be read.
    fn sha256(&self, id: ArtifactId, version: u64, name: &str) -> Result<String>;

    /// Reads the bytes of a version, and stops after `max_bytes`. A store must not read a larger
    /// file into memory.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::InvalidFile`] if the version is larger than the limit, or an
    /// error if the bytes cannot be read.
    fn get_limited(
        &self,
        id: ArtifactId,
        version: u64,
        name: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>>;

    /// Returns a local path of a version, for stores that have one.
    fn local_path(&self, id: ArtifactId, version: u64, name: &str) -> Option<PathBuf>;

    /// Deletes the bytes of one version.
    ///
    /// # Errors
    ///
    /// Returns an error if the delete fails.
    fn delete_version(&self, id: ArtifactId, version: u64) -> Result<()>;

    /// Deletes the bytes of all versions.
    ///
    /// # Errors
    ///
    /// Returns an error if the delete fails.
    fn delete_all(&self, id: ArtifactId) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_path_rejects_escapes() {
        for bad in [
            "", "..", "a/../b", "/abs", "a//b", "a/./b", "a\\b", "a b", "a/b/", "ä",
        ] {
            assert!(DocPath::new(bad).is_err(), "{bad} must fail");
        }
        assert!(DocPath::new(".runtime/inbox/abc-1.jsonl").is_ok());
        assert!(DocPath::new(".gitignore").is_ok());
        for bad in [".git/config", "a/.GIT/hooks", "GIT~1/config"] {
            assert!(DocPath::new(bad).is_err(), "{bad} must fail");
        }
    }
}
