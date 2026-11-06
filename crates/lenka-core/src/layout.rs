// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The file layout of a data directory.
//!
//! The Git plugin uses this list in two directions. A commit holds only these files, and a branch
//! from another clone can hold only these files. So one stray file cannot stop the sync of another
//! clone, and a remote cannot add a file that Git or lenka reads as a control file.

use lenka_plugin_api::{ArtifactId, EntryId, MemoryId, TaskId};

/// The start of the name of a temporary file. A stopped process can leave such a file.
pub const TEMP_PREFIX: &str = ".tmp-";

/// The longest artifact file name.
pub const MAX_ARTIFACT_FILE_NAME: usize = 200;

/// Tells if a path, relative to the data directory and with `/` separators, can be lenka data.
#[must_use]
pub fn is_data_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        ["lenka.toml" | ".gitignore" | ".gitattributes"] => true,
        ["memory", file] => file
            .strip_suffix(".md")
            .is_some_and(|stem| MemoryId::parse(stem).is_ok()),
        ["tasks", id, rest @ ..] => TaskId::parse(id).is_ok() && is_task_path(rest),
        ["artifacts", id, rest @ ..] => ArtifactId::parse(id).is_ok() && is_artifact_path(rest),
        _ => false,
    }
}

fn is_task_path(parts: &[&str]) -> bool {
    match parts {
        ["task.md" | "plan.md" | "state.toml"] => true,
        ["versions", version, "task.md" | "plan.md" | "meta.toml"] => is_version_name(version),
        ["entries", file] => file
            .strip_suffix(".md")
            .is_some_and(|stem| EntryId::parse(stem).is_ok()),
        _ => false,
    }
}

fn is_artifact_path(parts: &[&str]) -> bool {
    match parts {
        ["artifact.toml"] => true,
        ["versions", version, file] => is_version_name(version) && is_artifact_file_name(file),
        _ => false,
    }
}

fn is_version_name(name: &str) -> bool {
    name.len() >= 6 && name.bytes().all(|byte| byte.is_ascii_digit())
}

/// Tells if a name can be the file of an artifact version. The name cannot start with a dot, so an
/// artifact file is never a Git control file or a temporary file.
#[must_use]
pub fn is_artifact_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ARTIFACT_FILE_NAME
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "01a1b2e7-da96-7000-8000-000000000001";

    #[test]
    fn the_files_of_lenka_are_data() {
        for path in [
            "lenka.toml".to_owned(),
            ".gitignore".to_owned(),
            ".gitattributes".to_owned(),
            format!("memory/{ID}.md"),
            format!("tasks/{ID}/task.md"),
            format!("tasks/{ID}/plan.md"),
            format!("tasks/{ID}/state.toml"),
            format!("tasks/{ID}/entries/{ID}.md"),
            format!("tasks/{ID}/versions/000001/task.md"),
            format!("tasks/{ID}/versions/000001/plan.md"),
            format!("tasks/{ID}/versions/1000000/meta.toml"),
            format!("artifacts/{ID}/artifact.toml"),
            format!("artifacts/{ID}/versions/000001/design.v2_final-1.bin"),
        ] {
            assert!(is_data_path(&path), "{path}");
        }
    }

    #[test]
    fn other_paths_are_not_data() {
        for path in [
            String::new(),
            ".git".to_owned(),
            ".gitmodules".to_owned(),
            ".DS_Store".to_owned(),
            ".tmp-1-0".to_owned(),
            ".runtime/pending.json".to_owned(),
            "tasks".to_owned(),
            "tasks/.gitignore".to_owned(),
            "tasks/plain/HEAD".to_owned(),
            "memory/.gitignore".to_owned(),
            "memory/notes.md".to_owned(),
            format!("memory/{ID}.txt"),
            format!("memory/{ID}.md/x"),
            "artifacts/.gitattributes".to_owned(),
            format!("tasks/{ID}/.gitignore"),
            format!("tasks/{ID}/.tmp-1-0"),
            format!("tasks/{ID}/TASK.md"),
            format!("tasks/{ID}/config"),
            format!("tasks/{ID}/entries/note.md"),
            format!("tasks/{ID}/versions/1/task.md"),
            format!("tasks/{ID}/versions/00000a/task.md"),
            format!("tasks/{ID}/versions/000001/.tmp-1-0"),
            format!("tasks/{ID}/versions/000002/task.md/x"),
            format!("artifacts/{ID}/.tmp-1"),
            format!("artifacts/{ID}/versions/000001/.gitattributes"),
            format!("artifacts/{ID}/versions/000001/.tmp-1"),
            format!("artifacts/{ID}/versions/000001/git~1"),
            format!("artifacts/{ID}/versions/000001/a/b"),
        ] {
            assert!(!is_data_path(&path), "{path}");
        }
    }
}
