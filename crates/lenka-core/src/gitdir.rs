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

/// Finds the Git location of a directory. A directory outside Git gives `None`.
#[must_use]
pub fn locate(start: &Path) -> Option<GitLocation> {
    let start = fs::canonicalize(start).ok()?;
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
    fs::canonicalize(full).ok()
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
    fs::canonicalize(&full).unwrap_or(full)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(first.worktree, fs::canonicalize(&main)?);
        Ok(())
    }

    #[test]
    fn an_empty_git_directory_is_not_a_repository() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let plain = temp.path().join("plain");
        fs::create_dir_all(plain.join(".git"))?;
        let found = locate(&plain);
        assert_ne!(
            found.map(|item| item.worktree),
            Some(fs::canonicalize(&plain)?)
        );
        Ok(())
    }
}
