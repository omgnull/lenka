// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! A source file that changes between the secret check and the copy.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use lenka_artifacts::{ArtifactService, NewVersion, Source, UpdateMode, artifacts_of};
use lenka_core::init::{Location, create_project};
use lenka_core::{Builder, Paths, Project};
use lenka_plugin_api::{Actor, Caller, LenkaError, ProjectScope, Result};
use lenka_tasks::ClaimRef;

/// The secret check runs before the wait for the lock, and this time lets it end.
const CHECK_TIME: Duration = Duration::from_millis(700);

fn person() -> Caller {
    Caller {
        actor: Actor::Human,
        label: "human:me".to_owned(),
        worktree: PathBuf::from("/worktrees/me"),
        agent_key: lenka_core::notices::agent_key(Path::new("/worktrees/me"), None),
        scope: ProjectScope::All,
        default_project: None,
    }
}

/// Builds a value that the secret check refuses. The value never leaves the test.
fn secret_value() -> String {
    format!("ghp_{}", "a1b2c3d4".repeat(5))
}

fn holds_text(dir: &Path, text: &str) -> bool {
    fs::read_dir(dir).is_ok_and(|items| {
        items.flatten().any(|item| {
            let path = item.path();
            if path.is_dir() {
                holds_text(&path, text)
            } else {
                fs::read(&path).is_ok_and(|bytes| String::from_utf8_lossy(&bytes).contains(text))
            }
        })
    })
}

/// Holds the write lock, runs the call on a thread, and changes the source during its wait.
fn change_during_the_wait<T: Send + 'static>(
    project: &Arc<Project>,
    source: &Path,
    call: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<Result<T>> {
    let worker = project.write_locked(|_| {
        let worker = thread::spawn(call);
        thread::sleep(CHECK_TIME);
        fs::write(source, secret_value()).map_err(|err| LenkaError::io("Write", &err))?;
        Ok(worker)
    })?;
    Ok(worker
        .join()
        .unwrap_or_else(|_| panic!("the call stopped with a panic")))
}

struct Setup {
    _temp: tempfile::TempDir,
    project: Arc<Project>,
    service: Arc<ArtifactService>,
    source: PathBuf,
    data: PathBuf,
}

fn setup() -> Result<Setup> {
    let io = |err| LenkaError::io("Set up the test", &err);
    let temp = tempfile::tempdir().map_err(io)?;
    let root = temp.path().to_path_buf();
    let paths = Paths::new(root.join("config"), root.join("data"));
    let work = root.join("work");
    fs::create_dir_all(&work).map_err(io)?;
    create_project(&paths, &work, Some("api"), &Location::Shared)?;
    let host = Builder::new()
        .plugin(lenka_driver_local::plugin())
        .plugin(lenka_artifact_fs::plugin())
        .plugin(lenka_tasks::plugin())
        .plugin(lenka_artifacts::plugin())
        .paths(paths.clone())
        .build()?;
    let project = host.open_by_name("api")?;
    let service = artifacts_of(&project)?;
    let source = work.join("note.txt");
    fs::write(&source, "Clean text.\n").map_err(io)?;
    Ok(Setup {
        _temp: temp,
        project,
        service,
        source,
        data: paths.shared_project_dir("api"),
    })
}

#[test]
fn a_create_refuses_a_source_that_changed_after_the_check() -> Result<()> {
    let fx = setup()?;
    let (service, path) = (Arc::clone(&fx.service), fx.source.clone());
    let result = change_during_the_wait(&fx.project, &fx.source, move || {
        service.create(&person(), Source::File(&path), None, None)
    })?;
    assert!(result.is_err(), "the changed bytes must not be stored");
    assert!(!holds_text(&fx.data, &secret_value()));
    Ok(())
}

#[test]
fn an_update_refuses_a_source_that_changed_after_the_check() -> Result<()> {
    let fx = setup()?;
    let record = fx
        .service
        .create(&person(), Source::File(&fx.source), None, None)?;
    let (service, path) = (Arc::clone(&fx.service), fx.source.clone());
    let result = change_during_the_wait(&fx.project, &fx.source, move || {
        let request = NewVersion {
            expected_version: 1,
            source: Source::File(&path),
            reason: "A new version.",
            mode: UpdateMode::Keep,
            claim: ClaimRef::Implicit,
        };
        service.update(&person(), record.id, &request)
    })?;
    assert!(result.is_err(), "the changed bytes must not be stored");
    assert!(!holds_text(&fx.data, &secret_value()));
    Ok(())
}
