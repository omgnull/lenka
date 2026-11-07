// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Finds the worktree and the Git common directory with no call to `git`.
//!
//! The hook has a time limit of 10 ms, so it cannot start another process.

use crate::files::{Links, read_text_limited};
use std::fs;
use std::path::{Path, PathBuf};

/// The limit of a small file that Git keeps for a worktree.
const GIT_FILE_LIMIT: u64 = 64 * 1024;

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
    // Windows removes such an end from a plain path, and the plain path then names another file.
    if rest
        .split('\\')
        .any(|part| part.ends_with('.') || part.ends_with(' '))
    {
        return None;
    }
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

/// Returns the main worktree of a linked worktree, only if Git made the link.
///
/// A `.git` file can come from another author, and it can name any directory. So the file must
/// name `<main>/.git/worktrees/<name>`, and the file `gitdir` there must name this `.git` file.
#[must_use]
pub fn main_worktree_of_linked(worktree: &Path) -> Option<PathBuf> {
    let worktree = real_path(worktree).ok()?;
    let dot_git = worktree.join(".git");
    if !fs::symlink_metadata(&dot_git).ok()?.is_file() {
        return None;
    }
    let git_dir = read_git_file(&dot_git, &worktree)?;
    let list = git_dir.parent()?;
    let common = list.parent()?;
    if list.file_name()? != "worktrees" || common.file_name()? != ".git" {
        return None;
    }
    if !is_made_by_git(&worktree) {
        return None;
    }
    common.parent().map(Path::to_path_buf)
}

/// Tells if Git made the `.git` entry of a worktree.
///
/// A `.git` directory is always from Git. A `.git` file can come from another author, and it can
/// name the Git directory of a real clone. So a file counts only in two cases:
///
/// - A linked worktree: the file `gitdir` in the Git directory names this `.git` file.
/// - A submodule: the setting `core.worktree` of the Git directory names this worktree.
#[must_use]
pub fn is_made_by_git(worktree: &Path) -> bool {
    let Ok(worktree) = real_path(worktree) else {
        return false;
    };
    let dot_git = worktree.join(".git");
    let Ok(meta) = fs::symlink_metadata(&dot_git) else {
        return false;
    };
    if meta.is_dir() {
        return true;
    }
    if !meta.is_file() {
        return false;
    }
    let Some(git_dir) = read_git_file(&dot_git, &worktree) else {
        return false;
    };
    let names = |text: Option<String>, base: &Path, target: &Path| {
        text.and_then(|text| {
            let path = Path::new(text.trim());
            let full = if path.is_absolute() {
                path.to_path_buf()
            } else {
                base.join(path)
            };
            real_path(&full).ok()
        })
        .is_some_and(|found| real_path(target).is_ok_and(|target| found == target))
    };
    let back = read_text_limited(&git_dir.join("gitdir"), Links::Refuse, GIT_FILE_LIMIT).ok();
    if names(back, &git_dir, &dot_git) {
        return true;
    }
    let config = read_text_limited(&git_dir.join("config"), Links::Refuse, GIT_FILE_LIMIT).ok();
    names(
        config.as_deref().and_then(core_worktree),
        &git_dir,
        &worktree,
    )
}

/// Gives the value of `core.worktree` in the text of a Git configuration file.
fn core_worktree(text: &str) -> Option<String> {
    let mut in_core = false;
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_core = line.eq_ignore_ascii_case("[core]");
            continue;
        }
        if !in_core {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("worktree") {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .unwrap_or(value);
            // A later value replaces an earlier one, as in Git.
            found = Some(value.to_owned());
        }
    }
    found
}

fn read_git_file(file: &Path, base: &Path) -> Option<PathBuf> {
    let text = read_text_limited(file, Links::Refuse, GIT_FILE_LIMIT).ok()?;
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
    let Ok(text) = read_text_limited(&git_dir.join("commondir"), Links::Refuse, GIT_FILE_LIMIT)
    else {
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
        assert_eq!(plain(r"\\?\C:\work\repo."), None);
        assert_eq!(plain(r"\\?\C:\work \repo"), None);
        assert_eq!(plain(r"\\?\UNC\host\share."), None);
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
    fn only_a_git_file_from_git_counts() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let main = temp.path().join("main");
        let library = temp.path().join("library");
        fs::create_dir_all(&main)?;
        fs::create_dir_all(&library)?;
        git(&library, &["init", "-q"]);
        git(&library, &["commit", "-q", "--allow-empty", "-m", "first"]);
        git(&main, &["init", "-q"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "first"]);
        git(&main, &["worktree", "add", "-q", "../linked"]);
        git(
            &main,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                &library.display().to_string(),
                "lib",
            ],
        );
        let linked = temp.path().join("linked");
        assert!(is_made_by_git(&main));
        assert!(is_made_by_git(&linked));
        assert!(is_made_by_git(&main.join("lib")));
        assert_eq!(main_worktree_of_linked(&linked), Some(real_path(&main)?));
        assert_eq!(main_worktree_of_linked(&main.join("lib")), None);

        // A `.git` file from another author names a real Git directory of each kind.
        let git_dir = main.join(".git");
        for target in [
            git_dir.clone(),
            git_dir.join("worktrees").join("linked"),
            git_dir.join("modules").join("lib"),
        ] {
            let evil = temp.path().join("evil");
            fs::create_dir_all(&evil)?;
            fs::write(evil.join(".git"), format!("gitdir: {}\n", target.display()))?;
            assert!(locate(&evil).is_some(), "{}", target.display());
            assert!(!is_made_by_git(&evil), "{}", target.display());
            assert_eq!(main_worktree_of_linked(&evil), None);
            fs::remove_dir_all(&evil)?;
        }
        Ok(())
    }

    #[test]
    fn the_worktree_setting_comes_from_the_core_section() {
        assert_eq!(
            core_worktree("[core]\n\tworktree = ../../../lib\n").as_deref(),
            Some("../../../lib")
        );
        assert_eq!(
            core_worktree("[Core]\nWorkTree=\"a b\"\n[core]\nworktree = c\n").as_deref(),
            Some("c")
        );
        assert_eq!(core_worktree("[remote \"x\"]\nworktree = a\n"), None);
        assert_eq!(core_worktree("worktree = a\n"), None);
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
