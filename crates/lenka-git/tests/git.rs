// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Tests of the Git plugin with real `git` in temporary directories.
//!
//! The plugin reads the environment of the process, and the code here cannot change it. So each
//! test runs itself again as a child process, with a Git configuration in a temporary directory.
//! The tests never read the real configuration of the user. They do not need Git LFS: the one
//! test for it stops at once on a machine without Git LFS.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use lenka_core::init::{Location, create_project, register_existing};
use lenka_core::{Builder, Host, Paths, Project};
use lenka_git::{EnableOptions, Layout, enable, restore, sync};
use lenka_plugin_api::{Actor, Caller, ProjectScope, TaskId};
use lenka_tasks::{ClaimRef, Finish, NewTask, TaskPatch, TaskService, service_of};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const CHILD_FLAG: &str = "LENKA_GIT_TEST_CHILD";
const WITH_IDENTITY: &str = "[user]\n\tname = Test Person\n\temail = person@example.invalid\n[init]\n\tdefaultBranch = main\n";
const WITH_LFS: &str = "[user]\n\tname = Test Person\n\temail = person@example.invalid\n[init]\n\tdefaultBranch = main\n[filter \"lfs\"]\n\tclean = git-lfs clean -- %f\n\tsmudge = git-lfs smudge -- %f\n\tprocess = git-lfs filter-process\n\trequired = true\n";
const NO_IDENTITY: &str = "[init]\n\tdefaultBranch = main\n";

/// Runs the body in a child process with an isolated Git configuration.
fn isolated(name: &str, config: &str, body: impl FnOnce() -> TestResult) -> TestResult {
    if env::var_os(CHILD_FLAG).is_some() {
        return body();
    }
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("gitconfig");
    fs::write(&file, config)?;
    let mut command = Command::new(env::current_exe()?);
    command
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD_FLAG, "1")
        .env("GIT_CONFIG_GLOBAL", &file)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path());
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_AUTHOR_NAME",
        "GIT_COMMITTER_NAME",
    ] {
        command.env_remove(var);
    }
    let output = command.output()?;
    assert!(
        output.status.success(),
        "{name} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

/// Returns a file URL. On Windows, the form is `file:///C:/dir`, because Git LFS refuses a
/// backslash or a drive letter directly after `file://`.
fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        format!("file:///{text}")
    }
}

fn git(dir: &Path, args: &[&str]) -> TestResult<String> {
    let output = Command::new("git").args(args).current_dir(dir).output()?;
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn git_os<I, S>(dir: &Path, args: I) -> TestResult
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()?
        .status;
    assert!(status.success());
    Ok(())
}

/// One machine: its own user directories, with the Git plugin on.
struct Machine {
    root: PathBuf,
    paths: Paths,
    host: Host,
}

impl Machine {
    fn new(root: &Path) -> TestResult<Self> {
        let paths = Paths::new(root.join("config"), root.join("data"));
        fs::create_dir_all(root.join("config"))?;
        fs::write(paths.plugins_file(), "enabled = [\"git\"]\n")?;
        let host = Builder::new()
            .plugin(lenka_driver_local::plugin())
            .plugin(lenka_artifact_fs::plugin())
            .plugin(lenka_tasks::plugin())
            .plugin(lenka_artifacts::plugin())
            .plugin(lenka_git::plugin())
            .paths(paths.clone())
            .build()?;
        Ok(Self {
            root: root.to_path_buf(),
            paths,
            host,
        })
    }

    fn project(&self, name: &str) -> TestResult<Arc<Project>> {
        Ok(self.host.open_by_name(name)?)
    }
}

fn person() -> Caller {
    Caller {
        actor: Actor::Human,
        label: "human:me".to_owned(),
        worktree: "/worktrees/me".into(),
        agent_key: lenka_core::notices::agent_key(Path::new("/worktrees/me"), None),
        scope: ProjectScope::All,
        default_project: None,
    }
}

fn add_task(service: &TaskService, title: &str) -> TestResult<TaskId> {
    let new = NewTask {
        title: title.to_owned(),
        description: "Text.".to_owned(),
        ..NewTask::default()
    };
    Ok(service.create(&person(), new)?.id())
}

fn change_text(service: &TaskService, id: TaskId, text: &str) -> TestResult {
    let version = service.detail(id, &[])?.meta.state.version;
    let patch = TaskPatch {
        expected_version: version,
        reason: Some("A new text.".to_owned()),
        description: Some(text.to_owned()),
        ..TaskPatch::default()
    };
    service.update(&person(), id, patch)?;
    Ok(())
}

fn finish(service: &TaskService, id: TaskId) -> TestResult {
    service.claim(&person(), Some(id), None, None)?;
    let done = Finish::Done {
        evidence: "cargo test: ok".to_owned(),
        unchecked_reason: None,
    };
    service.finish(&person(), id, &ClaimRef::Implicit, &done)?;
    Ok(())
}

/// A clone of the code with one commit, and a project in its default data directory.
fn repo_with_project(machine: &Machine, name: &str) -> TestResult<PathBuf> {
    let repo = machine.root.join(name);
    fs::create_dir_all(&repo)?;
    git(&repo, &["init", "-q"])?;
    fs::write(repo.join("code.txt"), "code\n")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-q", "-m", "code"])?;
    create_project(&machine.paths, &repo, Some(name), &Location::Default)?;
    Ok(repo)
}

fn common_dir(repo: &Path) -> PathBuf {
    repo.join(".git")
}

fn data_dir(repo: &Path) -> PathBuf {
    repo.join(".git/lenka-data")
}

fn bare(root: &Path) -> TestResult<PathBuf> {
    let path = root.join("remote.git");
    fs::create_dir_all(&path)?;
    git(&path, &["init", "-q", "--bare"])?;
    Ok(path)
}

fn add_remote(repo: &Path, remote: &Path) -> TestResult {
    git_os(
        repo,
        [
            OsStr::new("remote"),
            OsStr::new("add"),
            OsStr::new("origin"),
            remote.as_os_str(),
        ],
    )
}

fn enable_default(
    machine: &Machine,
    repo: &Path,
    name: &str,
) -> TestResult<lenka_git::EnableReport> {
    let project = machine.project(name)?;
    Ok(enable(
        &project,
        Some(&common_dir(repo)),
        &EnableOptions::default(),
    )?)
}

#[test]
fn enable_makes_a_worktree_with_no_change_to_the_checkout() -> TestResult {
    isolated(
        "enable_makes_a_worktree_with_no_change_to_the_checkout",
        WITH_IDENTITY,
        || {
            let temp = tempfile::tempdir()?;
            let machine = Machine::new(temp.path())?;
            let repo = repo_with_project(&machine, "api")?;
            let head = git(&repo, &["rev-parse", "HEAD"])?;
            let branch = git(&repo, &["symbolic-ref", "HEAD"])?;

            let report = enable_default(&machine, &repo, "api")?;
            assert_eq!(report.layout, Layout::Worktree);
            assert!(report.committed);
            assert!(!report.remote_found);

            assert_eq!(git(&repo, &["rev-parse", "HEAD"])?, head);
            assert_eq!(git(&repo, &["symbolic-ref", "HEAD"])?, branch);
            assert_eq!(git(&repo, &["status", "--porcelain"])?, "");
            let data = data_dir(&repo);
            assert!(data.join(".git").is_file());
            let files = git(&data, &["ls-files"])?;
            assert!(files.contains("lenka.toml"), "{files}");
            assert!(files.contains(".gitattributes"), "{files}");
            assert!(!files.contains(".runtime"), "{files}");
            assert_eq!(git(&data, &["status", "--porcelain"])?, "");
            let project = machine.project("api")?;
            assert!(project.config().git.enabled);
            let text = fs::read_to_string(data.join("lenka.toml"))?;
            assert!(text.contains("enabled = true"), "{text}");

            // A repeat call is safe.
            let again = enable_default(&machine, &repo, "api")?;
            assert!(!again.committed);
            assert_eq!(git(&data, &["log", "--format=%s"])?.lines().count(), 2);
            Ok(())
        },
    )
}

#[test]
fn the_hook_commits_at_a_finish_and_skips_user_hooks() -> TestResult {
    isolated(
        "the_hook_commits_at_a_finish_and_skips_user_hooks",
        WITH_IDENTITY,
        || {
            let temp = tempfile::tempdir()?;
            let machine = Machine::new(temp.path())?;
            let repo = repo_with_project(&machine, "api")?;
            enable_default(&machine, &repo, "api")?;

            // A user hook that fails would stop a normal commit.
            let hooks = repo.join(".git/hooks");
            fs::create_dir_all(&hooks)?;
            let marker = temp.path().join("hook-ran");
            let script = format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display());
            for name in [
                "pre-commit",
                "commit-msg",
                "post-commit",
                "post-checkout",
                "pre-push",
            ] {
                let file = hooks.join(name);
                fs::write(&file, &script)?;
                make_executable(&file)?;
            }

            let project = machine.project("api")?;
            let service = service_of(&project)?;
            let id = add_task(&service, "A task")?;
            let data = data_dir(&repo);
            assert_eq!(git(&data, &["log", "--format=%s"])?.lines().count(), 2);
            finish(&service, id)?;

            let log = git(&data, &["log", "-1", "--format=%s%n%an"])?;
            assert!(log.contains(&format!("lenka: finish task {id}")), "{log}");
            assert!(log.contains("Test Person"), "{log}");
            assert!(git(&data, &["ls-files"])?.contains(&id.to_string()));
            assert_eq!(git(&data, &["status", "--porcelain"])?, "");
            assert!(!marker.exists(), "a user hook must not run");
            // The code branch stays clean.
            assert_eq!(git(&repo, &["status", "--porcelain"])?, "");
            Ok(())
        },
    )
}

#[cfg(unix)]
fn make_executable(file: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(file, fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_file: &Path) -> std::io::Result<()> {
    Ok(())
}

#[test]
fn a_person_with_no_identity_commits_as_lenka() -> TestResult {
    isolated(
        "a_person_with_no_identity_commits_as_lenka",
        NO_IDENTITY,
        || {
            let temp = tempfile::tempdir()?;
            let machine = Machine::new(temp.path())?;
            let dir = temp.path().join("shared-data");
            let work = temp.path().join("work");
            fs::create_dir_all(&work)?;
            create_project(
                &machine.paths,
                &work,
                Some("shared"),
                &Location::Directory(dir.clone()),
            )?;
            let project = machine.project("shared")?;
            let report = enable(&project, None, &EnableOptions::default())?;
            assert_eq!(report.layout, Layout::Repository);
            assert_eq!(
                git(&dir, &["log", "-1", "--format=%an <%ae>"])?,
                "lenka <lenka@localhost.invalid>"
            );
            assert_eq!(
                git(&dir, &["symbolic-ref", "HEAD"])?,
                "refs/heads/lenka-data"
            );
            Ok(())
        },
    )
}

#[test]
fn the_plugin_stays_off_without_plugins_toml() -> TestResult {
    isolated(
        "the_plugin_stays_off_without_plugins_toml",
        WITH_IDENTITY,
        || {
            let temp = tempfile::tempdir()?;
            let machine = Machine::new(temp.path())?;
            let repo = repo_with_project(&machine, "api")?;
            enable_default(&machine, &repo, "api")?;

            fs::remove_file(machine.paths.plugins_file())?;
            let host = Builder::new()
                .plugin(lenka_driver_local::plugin())
                .plugin(lenka_artifact_fs::plugin())
                .plugin(lenka_tasks::plugin())
                .plugin(lenka_git::plugin())
                .paths(machine.paths.clone())
                .build()?;
            assert!(
                host.plugins()
                    .iter()
                    .any(|item| item.info.name == "git" && !item.enabled)
            );
            let service = service_of(&host.open_by_name("api")?)?;
            let id = add_task(&service, "A task")?;
            finish(&service, id)?;
            let data = data_dir(&repo);
            assert_eq!(git(&data, &["log", "--format=%s"])?.lines().count(), 2);
            assert!(!git(&data, &["status", "--porcelain"])?.is_empty());
            Ok(())
        },
    )
}

#[test]
fn a_directory_inside_a_clone_never_commits_to_the_code() -> TestResult {
    isolated(
        "a_directory_inside_a_clone_never_commits_to_the_code",
        WITH_IDENTITY,
        || {
            let temp = tempfile::tempdir()?;
            let machine = Machine::new(temp.path())?;
            let repo = temp.path().join("code");
            fs::create_dir_all(&repo)?;
            git(&repo, &["init", "-q"])?;
            git(&repo, &["commit", "-q", "--allow-empty", "-m", "code"])?;
            let dir = repo.join("planning");
            create_project(
                &machine.paths,
                &repo,
                Some("plan"),
                &Location::Directory(dir.clone()),
            )?;
            let project = machine.project("plan")?;

            // Without `enable`, a sync refuses. The parent repository stays as it was.
            assert!(sync(&project).is_err());
            let mut config = (*project.config()).clone();
            config.git.enabled = true;
            project.save_config(config)?;
            assert!(sync(&project).is_err());
            assert_eq!(git(&repo, &["log", "--format=%s"])?, "code");

            let report = enable(
                &project,
                Some(&common_dir(&repo)),
                &EnableOptions::default(),
            )?;
            assert_eq!(report.layout, Layout::Repository);
            assert_eq!(git(&repo, &["log", "--format=%s"])?, "code");
            assert_eq!(
                git(&dir, &["log", "--format=%s"])?,
                "lenka: connect the data to Git"
            );
            Ok(())
        },
    )
}

#[test]
fn a_bad_remote_in_the_settings_is_refused() -> TestResult {
    isolated(
        "a_bad_remote_in_the_settings_is_refused",
        WITH_IDENTITY,
        || {
            let temp = tempfile::tempdir()?;
            let machine = Machine::new(temp.path())?;
            let repo = repo_with_project(&machine, "api")?;
            enable_default(&machine, &repo, "api")?;
            let project = machine.project("api")?;
            let mut config = (*project.config()).clone();
            config.git.remote = "--upload-pack=touch /tmp/x".to_owned();
            project.save_config(config)?;
            assert!(sync(&project).is_err());

            let options = EnableOptions {
                remote: Some("missing".to_owned()),
                ..EnableOptions::default()
            };
            assert!(enable(&project, Some(&common_dir(&repo)), &options).is_err());
            Ok(())
        },
    )
}

/// Pushes one project and restores it in a fresh clone on a second machine.
#[test]
fn a_fresh_clone_restores_all_tasks_and_artifact_bytes() -> TestResult {
    isolated(
        "a_fresh_clone_restores_all_tasks_and_artifact_bytes",
        WITH_IDENTITY,
        || {
            let temp = tempfile::tempdir()?;
            let first_root = temp.path().join("first");
            let second_root = temp.path().join("second");
            fs::create_dir_all(&first_root)?;
            fs::create_dir_all(&second_root)?;
            let remote = bare(temp.path())?;

            let first = Machine::new(&first_root)?;
            let repo = repo_with_project(&first, "api")?;
            add_remote(&repo, &remote)?;
            git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/main"])?;
            enable_default(&first, &repo, "api")?;

            let project = first.project("api")?;
            let service = service_of(&project)?;
            let done = add_task(&service, "The done task")?;
            let open = add_task(&service, "The open task")?;
            change_text(&service, open, "A second text.")?;
            finish(&service, done)?;
            let artifacts = lenka_artifacts::artifacts_of(&project)?;
            let bytes: Vec<u8> = (0..=255).cycle().take(4096).collect();
            let record = artifacts.create(
                &person(),
                lenka_artifacts::Source::Bytes {
                    name: "design.bin",
                    media_type: "application/octet-stream",
                    bytes: &bytes,
                },
                None,
                None,
            )?;

            let report = sync(&project)?;
            assert!(report.committed);
            assert!(report.pushed);
            assert!(!report.has_conflict());
            assert_eq!(report.remote.as_deref(), Some("origin"));

            // A fresh clone on another machine.
            let clone = second_root.join("clone");
            git_os(
                &second_root,
                [
                    OsStr::new("clone"),
                    OsStr::new("-q"),
                    remote.as_os_str(),
                    clone.as_os_str(),
                ],
            )?;
            let head = git(&clone, &["rev-parse", "HEAD"])?;
            let second = Machine::new(&second_root)?;
            let Some(data) = restore(&clone)? else {
                panic!("the clone has the data branch");
            };
            assert_eq!(data, data_dir(&lenka_core::gitdir::real_path(&clone)?));
            assert_eq!(git(&clone, &["rev-parse", "HEAD"])?, head);
            assert_eq!(git(&clone, &["status", "--porcelain"])?, "");

            let created = register_existing(&second.paths, &clone, None, &data)?;
            assert_eq!(created.name, "api");
            let restored = second.project("api")?;
            assert_eq!(restored.id(), project.id());
            let restored_tasks = service_of(&restored)?;
            assert_eq!(
                restored_tasks.detail(done, &[])?.meta.state.status,
                lenka_tasks::Status::Done
            );
            let detail = restored_tasks.detail(open, &["versions"])?;
            assert_eq!(detail.meta.state.version, 2);
            assert!(detail.description.contains("A second text."));
            assert!(restored_tasks.read_version(open, 1)?.0.contains("Text."));
            let restored_artifacts = lenka_artifacts::artifacts_of(&restored)?;
            assert_eq!(restored_artifacts.read_bytes(record.id, None)?, bytes);
            assert!(
                lenka_tasks::artifact_view(&restored, &restored_artifacts.record(record.id)?)
                    .downloaded
            );

            // New work in the fresh clone reaches the first clone.
            let added = add_task(&restored_tasks, "From the clone")?;
            assert!(sync(&restored)?.pushed);
            let report = sync(&project)?;
            assert!(report.merged);
            service.reindex(&person())?;
            assert_eq!(
                service.detail(added, &[])?.meta.front.title,
                "From the clone"
            );

            // A second restore is safe, and finds the same directory.
            assert_eq!(restore(&clone)?, Some(data));
            Ok(())
        },
    )
}

#[test]
fn git_lfs_sends_the_artifact_bytes_and_a_fresh_clone_gets_them() -> TestResult {
    isolated(
        "git_lfs_sends_the_artifact_bytes_and_a_fresh_clone_gets_them",
        WITH_LFS,
        || {
            let temp = tempfile::tempdir()?;
            let has_lfs = Command::new("git")
                .args(["lfs", "version"])
                .current_dir(temp.path())
                .output()
                .is_ok_and(|output| output.status.success());
            if !has_lfs {
                return Ok(());
            }
            let first_root = temp.path().join("first");
            let second_root = temp.path().join("second");
            fs::create_dir_all(&first_root)?;
            fs::create_dir_all(&second_root)?;
            let remote = bare(temp.path())?;
            let url = file_url(&remote);

            let first = Machine::new(&first_root)?;
            let repo = repo_with_project(&first, "api")?;
            git(&repo, &["remote", "add", "origin", &url])?;
            git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/main"])?;
            enable_default(&first, &repo, "api")?;

            let project = first.project("api")?;
            let artifacts = lenka_artifacts::artifacts_of(&project)?;
            let bytes: Vec<u8> = (0..=255).cycle().take(4096).collect();
            let record = artifacts.create(
                &person(),
                lenka_artifacts::Source::Bytes {
                    name: "design.bin",
                    media_type: "application/octet-stream",
                    bytes: &bytes,
                },
                None,
                None,
            )?;
            assert!(sync(&project)?.pushed);

            // The branch holds a pointer file, and the remote holds the bytes.
            let files = git(
                &remote,
                &["ls-tree", "-r", "--name-only", lenka_git::BRANCH],
            )?;
            let Some(file) = files.lines().find(|line| line.ends_with("design.bin")) else {
                panic!("the branch has the artifact: {files}");
            };
            let stored = git(&remote, &["show", &format!("{}:{file}", lenka_git::BRANCH)])?;
            assert!(stored.starts_with("version https://git-lfs"), "{stored}");
            assert!(remote.join("lfs/objects").is_dir());

            let clone = second_root.join("clone");
            git(
                &second_root,
                &["clone", "-q", &url, &clone.to_string_lossy()],
            )?;
            let second = Machine::new(&second_root)?;
            let Some(data) = restore(&clone)? else {
                panic!("the clone has the data branch");
            };
            register_existing(&second.paths, &clone, None, &data)?;
            let restored = lenka_artifacts::artifacts_of(&second.project("api")?)?;
            assert_eq!(restored.read_bytes(record.id, None)?, bytes);
            Ok(())
        },
    )
}

#[test]
fn restore_finds_nothing_without_the_branch() -> TestResult {
    isolated(
        "restore_finds_nothing_without_the_branch",
        WITH_IDENTITY,
        || {
            let temp = tempfile::tempdir()?;
            let repo = temp.path().join("code");
            fs::create_dir_all(&repo)?;
            git(&repo, &["init", "-q"])?;
            git(&repo, &["commit", "-q", "--allow-empty", "-m", "code"])?;
            assert_eq!(restore(&repo)?, None);
            assert!(!repo.join(".git/lenka-data").exists());
            let plain = temp.path().join("plain");
            fs::create_dir_all(&plain)?;
            assert_eq!(restore(&plain)?, None);
            Ok(())
        },
    )
}

/// Two clones change the same task. The second sync stops with a conflict.
#[test]
fn a_conflict_stops_the_merge_and_lists_the_files() -> TestResult {
    isolated(
        "a_conflict_stops_the_merge_and_lists_the_files",
        WITH_IDENTITY,
        || {
            let temp = tempfile::tempdir()?;
            let first_root = temp.path().join("first");
            let second_root = temp.path().join("second");
            fs::create_dir_all(&first_root)?;
            fs::create_dir_all(&second_root)?;
            let remote = bare(temp.path())?;

            let first = Machine::new(&first_root)?;
            let repo = repo_with_project(&first, "api")?;
            add_remote(&repo, &remote)?;
            git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/main"])?;
            enable_default(&first, &repo, "api")?;
            let project = first.project("api")?;
            let service = service_of(&project)?;
            let id = add_task(&service, "A task")?;
            assert!(sync(&project)?.pushed);

            let clone = second_root.join("clone");
            git_os(
                &second_root,
                [
                    OsStr::new("clone"),
                    OsStr::new("-q"),
                    remote.as_os_str(),
                    clone.as_os_str(),
                ],
            )?;
            let second = Machine::new(&second_root)?;
            let Some(data) = restore(&clone)? else {
                panic!("the clone has the data branch");
            };
            register_existing(&second.paths, &clone, None, &data)?;
            let restored = second.project("api")?;
            let restored_tasks = service_of(&restored)?;

            change_text(&service, id, "The text of the first clone.")?;
            change_text(&restored_tasks, id, "The text of the second clone.")?;
            assert!(sync(&project)?.pushed);

            let head = git(&data, &["rev-parse", "HEAD"])?;
            let report = sync(&restored)?;
            assert!(report.has_conflict());
            assert!(!report.pushed);
            assert!(!report.merged);
            assert!(
                report.conflict.iter().any(|file| file.ends_with("task.md")),
                "{:?}",
                report.conflict
            );
            assert!(
                report
                    .conflict
                    .iter()
                    .any(|file| file.contains(&id.to_string())),
                "{:?}",
                report.conflict
            );

            // The merge stopped, so the files show the local text, with no markers.
            let local = git(&data, &["rev-parse", "HEAD"])?;
            assert_ne!(local, head, "the sync commits local data first");
            assert_eq!(git(&data, &["status", "--porcelain"])?, "");
            assert!(!data.join(".git").is_dir());
            let text = fs::read_to_string(data.join(format!("tasks/{id}/task.md")))?;
            assert!(text.contains("The text of the second clone."), "{text}");
            assert!(!text.contains("<<<<<<<"), "{text}");
            Ok(())
        },
    )
}

/// A shared project lives in its own repository, with no clone of the code.
#[test]
fn a_shared_project_uses_its_own_repository_and_a_remote_url() -> TestResult {
    isolated(
        "a_shared_project_uses_its_own_repository_and_a_remote_url",
        WITH_IDENTITY,
        || {
            let temp = tempfile::tempdir()?;
            let remote = bare(temp.path())?;
            let machine = Machine::new(temp.path())?;
            let work = temp.path().join("work");
            fs::create_dir_all(&work)?;
            let created = create_project(&machine.paths, &work, Some("shared"), &Location::Shared)?;
            let project = machine.project("shared")?;
            let options = EnableOptions {
                remote: remote.to_str().map(str::to_owned),
                push_on_finish: true,
            };
            let report = enable(&project, None, &options)?;
            assert_eq!(report.layout, Layout::Repository);
            assert_eq!(report.remote, "origin");
            assert!(report.remote_found);
            assert!(project.config().git.push_on_finish);

            let service = service_of(&project)?;
            let id = add_task(&service, "A task")?;
            finish(&service, id)?;

            // The finish pushed, so the remote has the task with no sync call.
            let files = git(&remote, &["ls-tree", "-r", "--name-only", "lenka-data"])?;
            assert!(files.contains(&format!("tasks/{id}/task.md")), "{files}");
            assert!(!files.contains(".runtime"), "{files}");
            assert!(created.directory.join(".git").is_dir());

            // A sync with no change is safe.
            let report = sync(&project)?;
            assert!(report.pushed);
            assert!(!report.committed);
            assert!(!report.merged);
            Ok(())
        },
    )
}

#[test]
fn a_sync_with_no_remote_only_commits() -> TestResult {
    isolated("a_sync_with_no_remote_only_commits", WITH_IDENTITY, || {
        let temp = tempfile::tempdir()?;
        let machine = Machine::new(temp.path())?;
        let repo = repo_with_project(&machine, "api")?;
        enable_default(&machine, &repo, "api")?;
        let project = machine.project("api")?;
        let service = service_of(&project)?;
        add_task(&service, "A task")?;
        let report = sync(&project)?;
        assert!(report.committed);
        assert!(!report.pushed);
        assert_eq!(report.remote, None);
        Ok(())
    })
}
