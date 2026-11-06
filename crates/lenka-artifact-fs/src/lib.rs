// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The filesystem artifact store. The bytes stay as they are, and Git LFS can track them.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lenka_core::{Plugin, PluginInfo, Registry};
use lenka_plugin_api::{ArtifactId, ArtifactStore, DocPath, LenkaError, Result, StoredBytes};
use sha2::{Digest, Sha256};

/// The plugin of this store.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsStorePlugin;

impl Plugin for FsStorePlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "artifact-fs",
            description: "Filesystem artifact store",
            sensitive: false,
        }
    }

    fn register(&self, registry: &mut Registry) {
        registry.artifact_store("fs", |root| {
            Ok(Arc::new(FsStore::new(root)) as Arc<dyn ArtifactStore>)
        });
    }
}

/// Returns the plugin of this store.
#[must_use]
pub fn plugin() -> FsStorePlugin {
    FsStorePlugin
}

/// An artifact store in the data directory of a project.
#[derive(Debug)]
pub struct FsStore {
    root: PathBuf,
}

impl FsStore {
    /// Creates a store for a data directory.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    fn version_dir(&self, id: ArtifactId, version: u64) -> PathBuf {
        self.root
            .join("artifacts")
            .join(id.to_string())
            .join("versions")
            .join(format!("{version:06}"))
    }

    fn file_of(&self, id: ArtifactId, version: u64, name: &str) -> Result<PathBuf> {
        // The check on the name keeps a file name from an artifact file inside the store.
        DocPath::new(name)?;
        if name.contains('/') {
            return Err(LenkaError::invalid_argument(
                "An artifact file name cannot hold a path.",
            ));
        }
        Ok(self.version_dir(id, version).join(name))
    }

    /// Refuses a symlink in any part of a path below the data directory.
    fn check_no_symlink(&self, path: &Path) -> Result<()> {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return Err(LenkaError::invalid_argument(
                "An artifact path is outside the data directory.",
            ));
        };
        let mut current = self.root.clone();
        for part in relative {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(LenkaError::invalid_file(
                        "An artifact path holds a symlink, and lenka refuses symlinks in the data directory.",
                    ));
                }
                Ok(_) => {}
                Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
                Err(err) => return Err(LenkaError::io("Read file metadata", &err)),
            }
        }
        Ok(())
    }

    /// Creates a new temporary file beside the target. A rename then replaces the target, so a
    /// write never goes through a symlink, and a stopped write leaves no partial version.
    fn begin_write(&self, target: &Path) -> Result<(PathBuf, File)> {
        self.check_no_symlink(target)?;
        let parent = target.parent().ok_or_else(|| {
            LenkaError::invalid_argument("An artifact path has no parent directory.")
        })?;
        fs::create_dir_all(parent)
            .map_err(|err| LenkaError::io("Create an artifact directory", &err))?;
        let temp = parent.join(format!(".tmp-{}", ArtifactId::generate()));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|err| LenkaError::io("Create the artifact", &err))?;
        Ok((temp, file))
    }

    fn finish_write(&self, temp: &Path, target: &Path, file: &File) -> Result<()> {
        let result = file
            .sync_data()
            .and_then(|()| fs::rename(temp, target))
            .map_err(|err| LenkaError::io("Store the artifact", &err));
        if result.is_err() {
            let _ = fs::remove_file(temp);
        }
        result
    }
}

fn digest_text(hasher: Sha256) -> String {
    let digest = hasher.finalize();
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

impl ArtifactStore for FsStore {
    fn put_file(
        &self,
        id: ArtifactId,
        version: u64,
        name: &str,
        source: &Path,
    ) -> Result<StoredBytes> {
        let target = self.file_of(id, version, name)?;
        let mut input =
            File::open(source).map_err(|err| LenkaError::io("Open the artifact source", &err))?;
        let (temp, mut output) = self.begin_write(&target)?;
        let mut hasher = Sha256::new();
        let mut size = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let count = match input.read(&mut buffer) {
                Ok(count) => count,
                Err(err) => {
                    let _ = fs::remove_file(&temp);
                    return Err(LenkaError::io("Read the artifact source", &err));
                }
            };
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
            if let Err(err) = output.write_all(&buffer[..count]) {
                let _ = fs::remove_file(&temp);
                return Err(LenkaError::io("Write the artifact", &err));
            }
            size += count as u64;
        }
        self.finish_write(&temp, &target, &output)?;
        Ok(StoredBytes {
            size,
            sha256: digest_text(hasher),
        })
    }

    fn put_bytes(
        &self,
        id: ArtifactId,
        version: u64,
        name: &str,
        bytes: &[u8],
    ) -> Result<StoredBytes> {
        let target = self.file_of(id, version, name)?;
        let (temp, mut output) = self.begin_write(&target)?;
        if let Err(err) = output.write_all(bytes) {
            let _ = fs::remove_file(&temp);
            return Err(LenkaError::io("Write the artifact", &err));
        }
        self.finish_write(&temp, &target, &output)?;
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        Ok(StoredBytes {
            size: bytes.len() as u64,
            sha256: digest_text(hasher),
        })
    }

    fn get(&self, id: ArtifactId, version: u64, name: &str) -> Result<Vec<u8>> {
        let file = self.file_of(id, version, name)?;
        self.check_no_symlink(&file)?;
        let meta = fs::symlink_metadata(&file)
            .map_err(|err| LenkaError::io("Read the artifact metadata", &err))?;
        if meta.file_type().is_symlink() {
            return Err(LenkaError::invalid_file(
                "An artifact file is a symlink, and lenka refuses symlinks in the data directory.",
            ));
        }
        fs::read(&file).map_err(|err| LenkaError::io("Read the artifact", &err))
    }

    fn get_limited(
        &self,
        id: ArtifactId,
        version: u64,
        name: &str,
        max_bytes: u64,
    ) -> Result<Vec<u8>> {
        let path = self.file_of(id, version, name)?;
        self.check_no_symlink(&path)?;
        let file =
            fs::File::open(&path).map_err(|err| LenkaError::io("Read the artifact", &err))?;
        let mut bytes = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|err| LenkaError::io("Read the artifact", &err))?;
        if bytes.len() as u64 > max_bytes {
            return Err(LenkaError::invalid_file(format!(
                "The artifact is larger than the limit of {max_bytes} bytes."
            )));
        }
        Ok(bytes)
    }

    fn local_path(&self, id: ArtifactId, version: u64, name: &str) -> Option<PathBuf> {
        self.file_of(id, version, name).ok()
    }

    fn delete_version(&self, id: ArtifactId, version: u64) -> Result<()> {
        self.check_no_symlink(&self.version_dir(id, version))?;
        match fs::remove_dir_all(self.version_dir(id, version)) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(LenkaError::io("Delete an artifact version", &err)),
        }
    }

    fn delete_all(&self, id: ArtifactId) -> Result<()> {
        self.check_no_symlink(&self.root.join("artifacts").join(id.to_string()))?;
        match fs::remove_dir_all(self.root.join("artifacts").join(id.to_string())) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
            Err(err) => Err(LenkaError::io("Delete an artifact", &err)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_a_known_hash() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let store = FsStore::new(temp.path());
        let id = ArtifactId::generate();
        let stored = store.put_bytes(id, 1, "a.txt", b"abc")?;
        assert_eq!(stored.size, 3);
        assert_eq!(
            stored.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(store.get(id, 1, "a.txt")?, b"abc");

        let source = temp.path().join("source.bin");
        fs::write(&source, b"abc").map_err(|err| LenkaError::io("write", &err))?;
        assert_eq!(store.put_file(id, 2, "b.bin", &source)?, stored);

        store.delete_version(id, 1)?;
        assert!(store.get(id, 1, "a.txt").is_err());
        store.delete_all(id)?;
        assert!(store.get(id, 2, "b.bin").is_err());
        Ok(())
    }

    #[test]
    fn a_limited_read_stops_at_the_limit() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let store = FsStore::new(temp.path());
        let id = ArtifactId::generate();
        store.put_bytes(id, 1, "a.txt", b"abc")?;
        assert_eq!(store.get_limited(id, 1, "a.txt", 3)?, b"abc");
        let error = store.get_limited(id, 1, "a.txt", 2).err();
        assert_eq!(
            error.map(|err| err.code()),
            Some(lenka_plugin_api::ErrorCode::InvalidFile)
        );
        Ok(())
    }

    #[test]
    fn a_name_with_a_path_is_refused() {
        let temp = std::env::temp_dir();
        let store = FsStore::new(&temp);
        let id = ArtifactId::generate();
        for name in ["../evil", "a/b", ".."] {
            assert!(store.put_bytes(id, 1, name, b"x").is_err(), "{name}");
        }
    }
}
