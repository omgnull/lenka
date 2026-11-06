// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Finds the worktree and the Git common directory with no call to `git`.
//!
//! The hook has a time limit of 10 ms, so it cannot start another process.

use std::fs;
use std::path::{Path, PathBuf};

/// The Git location of a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitLocation {
    /// The top directory of the worktree.
    pub worktree: PathBuf,
    /// The Git common directory, which all worktrees of a clone share.
    pub common_dir: PathBuf,
}

/// Returns the absolute form of an existing path, in a form that other programs can read.
///
/// On Windows, [`fs::canonicalize`] gives a path with the prefix `\\\\?\\`, and Git cannot use
/// that form. This function removes the prefix.
///
/// # Errors
///
/// Returns an error if the path does not exist.
pub fn real_path(path: &Path) -> std::io::Result<PathBuf> {
    let real = fs::canonicalize(path)?;
    Ok(match real.to_str().and_then(without_verbatim_prefix) {
        Some(plain) => PathBuf::from(plain),
        None => real,
    })
}

/// Removes the Windows prefix for an extended path, from a drive path or a network path.
fn without_verbatim_prefix(text: &str) -> Option<String> {
    let rest = text.strip_prefix(r"\\?\")?;
    if let Some(network) = rest.strip_prefix(r"UNC\") {
        return Some(format!(r"\\{network}"));
    }
    let bytes = rest.as_bytes();
    let is_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    is_drive.then(|| rest.to_owned())
}

/// Finds the Git location of a directory. A directory outside Git gives `None`.
#[must_use]
pub fn locate(start: &Path) -> Option<GitLocation> {
    let start = real_path(start).ok()?;
    for dir in start.ancestors() {
        let dot_git = dir.join(".git");
        let Ok(meta) = fs::symlink_metadata(&dot_git) else {
            continue;
        };
        // Git needs a HEAD file in a real Git directory, so an empty `.git` directory is not one.
        if meta.is_dir() && dot_git.join("HEAD").is_file() {
            return Some(GitLocation {
                worktree: dir.to_path_buf(),
                common_dir: dot_git,
            });
        }
        if meta.is_file() {
            let git_dir = read_git_file(&dot_git, dir)?;
            return Some(GitLocation {
                worktree: dir.to_path_buf(),
                common_dir: common_dir_of(&git_dir),
            });
        }
    }
    None
}

fn read_git_file(file: &Path, base: &Path) -> Option<PathBuf> {
    let text = fs::read_to_string(file).ok()?;
    let target = text.strip_prefix("gitdir:")?.trim();
    let path = Path::new(target);
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    real_path(&full).ok()
}

fn common_dir_of(git_dir: &Path) -> PathBuf {
    let Ok(text) = fs::read_to_string(git_dir.join("commondir")) else {
        return git_dir.to_path_buf();
    };
    let path = Path::new(text.trim());
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        git_dir.join(path)
    };
    real_path(&full).unwrap_or(full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_path_prefix_goes_away() {
        let plain = |text| without_verbatim_prefix(text);
        assert_eq!(plain(r"\\?\C:\work\repo").as_deref(), Some(r"C:\work\repo"));
        assert_eq!(
            plain(r"\\?\UNC\host\share").as_deref(),
            Some(r"\\host\share")
        );
        assert_eq!(plain(r"\\?\Volume{abc}\repo"), None);
        assert_eq!(plain(r"C:\work"), None);
        assert_eq!(plain("/work/repo"), None);
    }
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(args)
            .current_dir(dir)
            .output()
            .map(|out| out.status.success());
        assert_eq!(status.ok(), Some(true), "git {args:?} must pass");
    }

    #[test]
    fn locate_finds_the_same_common_dir_from_two_worktrees() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let main = temp.path().join("main");
        fs::create_dir_all(main.join("sub"))?;
        git(&main, &["init", "-q"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "first"]);
        git(&main, &["worktree", "add", "-q", "../second"]);

        let first = locate(&main.join("sub"));
        let second = locate(&temp.path().join("second"));
        let (Some(first), Some(second)) = (first, second) else {
            panic!("both directories are in Git");
        };
        assert_eq!(first.common_dir, second.common_dir);
        assert_ne!(first.worktree, second.worktree);
        assert_eq!(first.worktree, real_path(&main)?);
        Ok(())
    }

    #[test]
    fn an_empty_git_directory_is_not_a_repository() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let plain = temp.path().join("plain");
        fs::create_dir_all(plain.join(".git"))?;
        let found = locate(&plain);
        assert_ne!(found.map(|item| item.worktree), Some(real_path(&plain)?));
        Ok(())
    }
}
