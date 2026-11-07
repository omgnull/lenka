// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Tests of the Git commands with real processes: a push, a fresh clone, and a conflict.

#![expect(
    clippy::disallowed_methods,
    reason = "A test reads files that it wrote, and it needs no guard."
)]
#[expect(dead_code, reason = "this test binary uses only a part of the helpers")]
mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Machine, TestResult, text};
use serde_json::Value;

const ENABLE_GIT: &str = "enabled = [\"git\"]\n";

struct Pair {
    _temp: tempfile::TempDir,
    first: Machine,
    second: Machine,
    remote: PathBuf,
}

fn pair() -> TestResult<Pair> {
    let temp = tempfile::tempdir()?;
    let first = Machine::new(&temp.path().join("first"))?;
    let second = Machine::new(&temp.path().join("second"))?;
    for machine in [&first, &second] {
        fs::write(machine.root.join("config/plugins.toml"), ENABLE_GIT)?;
    }
    let remote = temp.path().join("remote.git");
    fs::create_dir_all(&remote)?;
    first.git(&remote, &["init", "-q", "--bare"])?;
    Ok(Pair {
        _temp: temp,
        first,
        second,
        remote,
    })
}

fn publish(machine: &Machine, name: &str, remote: &Path) -> TestResult<PathBuf> {
    let repo = machine.project(name)?;
    let url = remote.to_string_lossy().into_owned();
    machine.git(&repo, &["remote", "add", "origin", &url])?;
    machine.git(&repo, &["push", "-q", "origin", "HEAD:refs/heads/main"])?;
    Ok(repo)
}

fn fresh_clone(machine: &Machine, name: &str, remote: &Path) -> TestResult<PathBuf> {
    let target = machine.root.join(name);
    let url = remote.to_string_lossy().into_owned();
    let target_text = target.to_string_lossy().into_owned();
    machine.git(&machine.root, &["clone", "-q", &url, &target_text])?;
    Ok(target)
}

#[test]
fn a_project_survives_a_push_and_a_fresh_clone() -> TestResult {
    let pair = pair()?;
    let repo = publish(&pair.first, "api", &pair.remote)?;
    let first = &pair.first;
    first.ok(&repo, &["git", "enable"])?;

    let task = first.json(
        &repo,
        &["task", "create", "A task", "--description", "The text."],
    )?;
    let task_id = text(&task, "task").to_owned();
    first.json(
        &repo,
        &[
            "task",
            "update",
            &task_id,
            "--version",
            "1",
            "--plan",
            "The plan.",
            "--reason",
            "A plan.",
        ],
    )?;
    let bytes: Vec<u8> = (0..=255).collect();
    fs::write(repo.join("design.bin"), &bytes)?;
    let artifact = first.json(&repo, &["artifact", "create", "design.bin"])?;
    fs::remove_file(repo.join("design.bin"))?;
    let synced = first.json(&repo, &["sync"])?;
    assert_eq!(synced["pushed"], true);
    assert_eq!(synced["conflict"].as_array().map(Vec::len), Some(0));
    assert_eq!(first.git(&repo, &["status", "--porcelain"])?, "");

    let second = &pair.second;
    let clone = fresh_clone(second, "clone", &pair.remote)?;
    let output = second.ok(&clone, &["init"])?;
    assert!(output.contains("Restored the project api"), "{output}");
    assert_eq!(second.git(&clone, &["status", "--porcelain"])?, "");
    assert!(clone.join(".git/lenka-data/.git").is_file());

    let rows = second.json(&clone, &["task", "list"])?;
    assert_eq!(rows["tasks"][0]["untrusted_data"]["title"], "A task");
    let shown = second.json(
        &clone,
        &["task", "show", &task_id, "--include", "plan,versions"],
    )?;
    assert_eq!(shown["task"]["version"], 2);
    assert!(
        text(&shown["untrusted_data"], "plan").contains("The plan."),
        "{shown}"
    );
    let restored = clone.join(format!(
        ".git/lenka-data/artifacts/{}/versions/000001/design.bin",
        text(&artifact, "full_id")
    ));
    assert_eq!(fs::read(restored)?, bytes);
    assert_eq!(second.json(&clone, &["doctor"])?["clean"], true);
    // The restore did not make a second project.
    let ids = (
        first.json(&repo, &["project", "list"])?["projects"][0]["id"].clone(),
        second.json(&clone, &["project", "list"])?["projects"][0]["id"].clone(),
    );
    assert_eq!(ids.0, ids.1);

    // A change in the clone reaches the first machine, and the reindex shows it.
    let added = second.json(&clone, &["task", "create", "From the clone"])?;
    second.json(&clone, &["sync"])?;
    assert_eq!(first.json(&repo, &["sync"])?["merged"], true);
    assert_eq!(
        first.json(&repo, &["task", "show", text(&added, "task")])?["untrusted_data"]["title"],
        "From the clone"
    );
    Ok(())
}

#[test]
fn a_conflict_exits_with_a_failure_and_lists_the_files() -> TestResult {
    let pair = pair()?;
    let repo = publish(&pair.first, "api", &pair.remote)?;
    let first = &pair.first;
    first.ok(&repo, &["git", "enable"])?;
    let task = first.json(&repo, &["task", "create", "A task"])?;
    let id = text(&task, "task").to_owned();
    first.json(&repo, &["sync"])?;

    let second = &pair.second;
    let clone = fresh_clone(second, "clone", &pair.remote)?;
    second.ok(&clone, &["init"])?;

    let change = |machine: &Machine, dir: &Path, words: &str| -> TestResult {
        machine.json(
            dir,
            &[
                "task",
                "update",
                &id,
                "--version",
                "1",
                "--description",
                words,
                "--reason",
                "A change.",
            ],
        )?;
        Ok(())
    };
    change(first, &repo, "From the first machine.")?;
    first.json(&repo, &["sync"])?;
    change(second, &clone, "From the second machine.")?;

    let output = second.run(&clone, &["--json", "sync"])?;
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["pushed"], false);
    let files = report["conflict"].to_string();
    assert!(files.contains("task.md"), "{files}");
    let human = second.run(&clone, &["sync"])?;
    assert!(String::from_utf8_lossy(&human.stdout).contains("conflict"));

    // The merge stopped, so the local data stays valid.
    let shown = second.json(&clone, &["task", "show", &id])?;
    assert!(
        text(&shown["untrusted_data"], "description").contains("second machine"),
        "{shown}"
    );
    assert_eq!(second.json(&clone, &["doctor"])?["clean"], true);
    Ok(())
}

#[test]
fn git_commands_need_the_plugin_and_a_person_choice() -> TestResult {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let repo = machine.project("api")?;
    let output = machine.run(&repo, &["git", "enable"])?;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("plugin git is off"), "{stderr}");
    assert!(stderr.contains("plugins.toml"), "{stderr}");
    assert!(!machine.run(&repo, &["sync"])?.status.success());
    assert!(!repo.join(".git/lenka-data/.git").exists());

    fs::write(machine.root.join("config/plugins.toml"), ENABLE_GIT)?;
    assert!(!machine.run(&repo, &["sync"])?.status.success());
    let enabled = machine.json(&repo, &["git", "enable", "--push-on-finish"])?;
    assert_eq!(enabled["layout"], "worktree");
    let settings = fs::read_to_string(repo.join(".git/lenka-data/lenka.toml"))?;
    assert!(!settings.contains("push_on_finish"), "{settings}");
    assert_eq!(
        local_setting(&machine, &repo, "lenka.pushonfinish").as_deref(),
        Some("true")
    );
    assert!(
        !machine
            .run(&repo, &["git", "enable", "--remote", "missing"])?
            .status
            .success()
    );
    Ok(())
}

#[test]
fn a_shared_project_syncs_with_a_remote_url() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = machine.repo("code")?;
    machine.ok(&repo, &["init", "--shared", "--name", "team"])?;
    let url = pair.remote.to_string_lossy().into_owned();
    let enabled = machine.json(&repo, &["git", "enable", "--remote", &url])?;
    assert_eq!(enabled["layout"], "repository");
    assert_eq!(enabled["remote"], "origin");
    let task = machine.json(&repo, &["task", "create", "Shared"])?;
    assert_eq!(machine.json(&repo, &["sync"])?["pushed"], true);
    let files = machine.git(
        &pair.remote,
        &["ls-tree", "-r", "--name-only", "lenka-data"],
    )?;
    assert!(files.contains(text(&task, "full_id")), "{files}");
    Ok(())
}

#[test]
fn the_name_of_another_clone_is_refused_at_a_restore() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    machine.ok(&repo, &["git", "enable"])?;
    machine.json(&repo, &["sync"])?;

    let clone = fresh_clone(machine, "clone", &pair.remote)?;
    let output = machine.run(&clone, &["init"])?;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--name"), "{stderr}");
    machine.ok(&clone, &["init", "--name", "api-2"])?;
    // The first clone keeps its own data directory.
    let rows = machine.json(&repo, &["project", "list"])?;
    assert_eq!(rows["projects"].as_array().map(Vec::len), Some(2));
    assert_eq!(rows["projects"][0]["name"], "api");
    assert_eq!(rows["projects"][0]["default"], true);
    Ok(())
}

/// Sends one extra commit to the data branch, as a hostile clone can do.
fn push_hostile_files(
    machine: &Machine,
    remote: &Path,
    files: &[(&str, &str)],
) -> TestResult<PathBuf> {
    let evil = machine.root.join("evil");
    let (url, target) = (
        remote.to_string_lossy().into_owned(),
        evil.to_string_lossy().into_owned(),
    );
    machine.git(
        &machine.root,
        &["clone", "-q", "-b", "lenka-data", &url, &target],
    )?;
    for (path, text) in files {
        let file = evil.join(path);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&file, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755))?;
        }
        machine.git(&evil, &["add", "-f", path])?;
    }
    machine.git(&evil, &["commit", "-q", "-m", "hostile"])?;
    machine.git(&evil, &["push", "-q", "origin", "lenka-data"])?;
    Ok(evil)
}

#[test]
fn data_from_a_remote_cannot_run_code_or_replay_a_journal() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    machine.ok(&repo, &["git", "enable"])?;
    let task = machine.json(&repo, &["task", "create", "Keep me"])?;
    machine.json(&repo, &["sync"])?;

    let proof = machine.root.join("proof");
    let hook = format!("#!/bin/sh\ntouch {}\n", proof.display());
    let journal = r#"[{"op":"remove_tree","path":"tasks"}]"#;
    push_hostile_files(
        machine,
        &pair.remote,
        &[
            (".runtime/no-hooks/post-merge", &hook),
            (".runtime/pending.json", journal),
        ],
    )?;

    assert_eq!(machine.fails(&repo, &["sync"])?, "invalid_file");
    assert!(!proof.exists(), "a hook from the remote ran");
    let local = repo.join(".git/lenka-data/.runtime/pending.json");
    assert_eq!(fs::metadata(local).map_or(0, |meta| meta.len()), 0);
    let shown = machine.json(&repo, &["task", "show", text(&task, "task")])?;
    assert_eq!(shown["untrusted_data"]["title"], "Keep me");

    // A fresh clone refuses the same branch, and it makes no data directory.
    let second = fresh_clone(&pair.second, "fresh", &pair.remote)?;
    assert_eq!(pair.second.fails(&second, &["init"])?, "invalid_file");
    assert!(
        !second
            .join(".git/lenka-data/.runtime/pending.json")
            .exists()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_symlink_from_a_remote_is_refused() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    machine.ok(&repo, &["git", "enable"])?;
    machine.json(&repo, &["sync"])?;
    let evil = push_hostile_files(machine, &pair.remote, &[("tasks/plain.md", "text")])?;
    std::os::unix::fs::symlink("/etc/passwd", evil.join("tasks/link"))?;
    machine.git(&evil, &["add", "tasks/link"])?;
    machine.git(&evil, &["commit", "-q", "-m", "link"])?;
    machine.git(&evil, &["push", "-q", "origin", "lenka-data"])?;
    assert_eq!(machine.fails(&repo, &["sync"])?, "invalid_file");
    Ok(())
}

/// Returns the Git settings of a clone, as `git config` shows them in the data directory.
fn local_setting(machine: &Machine, repo: &Path, key: &str) -> Option<String> {
    let mut command = std::process::Command::new("git");
    machine.apply_env(&mut command);
    let output = command
        .args(["config", "--local", "--get", key])
        .current_dir(repo.join(".git/lenka-data"))
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn remote_files(machine: &Machine, remote: &Path) -> TestResult<String> {
    machine.git(remote, &["ls-tree", "-r", "--name-only", "lenka-data"])
}

/// Runs `lenka sync`, and returns the JSON report with the exit status.
fn sync_report(machine: &Machine, repo: &Path) -> TestResult<(bool, Value)> {
    let output = machine.run(repo, &["sync", "--json"])?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok((output.status.success(), serde_json::from_str(&text)?))
}

/// Two clones of one project, with the first change on the remote.
fn two_clones(pair: &Pair) -> TestResult<(PathBuf, PathBuf)> {
    let first = publish(&pair.first, "api", &pair.remote)?;
    pair.first.ok(&first, &["git", "enable"])?;
    pair.first.json(&first, &["sync"])?;
    let second = fresh_clone(&pair.second, "clone", &pair.remote)?;
    pair.second.ok(&second, &["init"])?;
    Ok((first, second))
}

#[test]
fn a_stray_file_stays_local_and_other_clones_can_sync() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    machine.ok(&repo, &["git", "enable"])?;
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let data = repo.join(".git/lenka-data");
    let id = text(&task, "full_id");
    for stray in [
        ".DS_Store".to_owned(),
        "notes.txt".to_owned(),
        "tasks/.DS_Store".to_owned(),
        format!("tasks/{id}/scratch.txt"),
    ] {
        fs::write(data.join(&stray), "stray")?;
    }

    let (passed, report) = sync_report(machine, &repo)?;
    assert!(passed, "{report}");
    assert_eq!(report["pushed"], true);
    let skipped = report["skipped"].to_string();
    assert!(skipped.contains("notes.txt"), "{report}");
    assert!(skipped.contains("scratch.txt"), "{report}");
    let files = remote_files(machine, &pair.remote)?;
    assert!(!files.contains("DS_Store"), "{files}");
    assert!(!files.contains("notes.txt"), "{files}");
    assert!(!files.contains("scratch.txt"), "{files}");
    assert_eq!(fs::read_to_string(data.join("notes.txt"))?, "stray");

    let second = fresh_clone(&pair.second, "clone", &pair.remote)?;
    pair.second.ok(&second, &["init"])?;
    let rows = pair.second.json(&second, &["task", "list"])?;
    assert_eq!(rows["tasks"][0]["untrusted_data"]["title"], "A task");
    Ok(())
}

#[test]
fn excludes_and_ignore_files_cannot_hide_data() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    machine.ok(&repo, &["git", "enable"])?;
    let data = repo.join(".git/lenka-data");
    fs::create_dir_all(repo.join(".git/info"))?;
    fs::write(repo.join(".git/info/exclude"), "*.log\n*.md\n*.toml\n")?;
    fs::write(data.join(".gitignore"), "*\n")?;
    fs::create_dir_all(data.join("tasks"))?;
    fs::write(data.join("tasks/.gitignore"), "*\n!.gitignore\n")?;

    fs::write(repo.join("trace.log"), "trace bytes")?;
    machine.json(&repo, &["artifact", "create", "trace.log"])?;
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let (passed, report) = sync_report(machine, &repo)?;
    assert!(passed, "{report}");

    let files = remote_files(machine, &pair.remote)?;
    assert!(files.contains("/trace.log"), "{files}");
    assert!(files.contains(text(&task, "full_id")), "{files}");
    assert!(!files.contains("tasks/.gitignore"), "{files}");
    assert!(!files.contains(".runtime"), "{files}");
    Ok(())
}

#[test]
fn a_hostile_ignore_file_cannot_stop_data_from_leaving() -> TestResult {
    let pair = pair()?;
    let (_first, second) = two_clones(&pair)?;
    push_hostile_files(&pair.first, &pair.remote, &[(".gitignore", "*\n")])?;
    let (passed, report) = sync_report(&pair.second, &second)?;
    assert!(passed, "{report}");
    assert_eq!(report["merged"], true, "{report}");

    let task = pair.second.json(&second, &["task", "create", "New"])?;
    let (passed, report) = sync_report(&pair.second, &second)?;
    assert!(passed, "{report}");
    assert_eq!(report["pushed"], true);
    let files = remote_files(&pair.first, &pair.remote)?;
    assert!(files.contains(text(&task, "full_id")), "{files}");
    Ok(())
}

/// Tells that `lenka sync` refuses a hostile tree, and that the project stays usable.
fn assert_tree_refused(files: &[(&str, &str)]) -> TestResult {
    let pair = pair()?;
    let (first, second) = two_clones(&pair)?;
    let task = pair.first.json(&first, &["task", "create", "Keep me"])?;
    pair.first.json(&first, &["sync"])?;
    push_hostile_files(&pair.first, &pair.remote, files)?;

    let code = pair.second.fails(&second, &["sync"])?;
    assert_eq!(code, "invalid_file", "{files:?}");
    assert_eq!(pair.second.json(&second, &["doctor"])?["clean"], true);
    let created = pair
        .second
        .json(&second, &["task", "create", "Still works"])?;
    assert_eq!(created["status"], "todo", "{created}");
    let shown = pair
        .first
        .json(&first, &["task", "show", text(&task, "task")])?;
    assert_eq!(shown["untrusted_data"]["title"], "Keep me");
    Ok(())
}

#[test]
fn a_control_file_below_the_root_is_refused() -> TestResult {
    assert_tree_refused(&[("tasks/.gitignore", "*\n!.gitignore\n")])?;
    assert_tree_refused(&[("memory/.gitignore", "*\n")])?;
    assert_tree_refused(&[("artifacts/.gitattributes", "* text\n")])
}

#[test]
fn a_changed_gitattributes_is_refused() -> TestResult {
    assert_tree_refused(&[(".gitattributes", "memory/** working-tree-encoding=UTF-16\n")])
}

#[test]
fn a_gitmodules_file_is_refused() -> TestResult {
    assert_tree_refused(&[(".gitmodules", "[submodule \"x\"]\n\tpath = x\n\turl = x\n")])
}

#[test]
fn a_temporary_file_from_a_remote_is_refused() -> TestResult {
    assert_tree_refused(&[(".tmp-1-0", "partial")])
}

#[test]
fn a_bare_repository_under_tasks_is_refused() -> TestResult {
    assert_tree_refused(&[
        ("tasks/plain/HEAD", "ref: refs/heads/main\n"),
        ("tasks/plain/config", "[core]\n\tfsmonitor = /bin/false\n"),
        ("tasks/plain/objects/keep", ""),
        ("tasks/plain/refs/keep", ""),
    ])
}

#[test]
fn a_directory_at_a_version_file_path_is_refused() -> TestResult {
    let id = "01a1b2e7-da96-7000-8000-000000000001";
    let path = format!("tasks/{id}/versions/000002/task.md/x");
    assert_tree_refused(&[(&path, "x")])
}

#[test]
fn a_conflict_marker_from_a_remote_is_refused() -> TestResult {
    let id = "01a1b2e7-da96-7000-8000-000000000001";
    let path = format!("tasks/{id}/task.md");
    assert_tree_refused(&[(
        &path,
        "+++\n+++\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> x\n",
    )])
}

#[test]
fn git_settings_stay_in_each_clone() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    let url = pair.remote.to_string_lossy().into_owned();
    machine.git(&repo, &["remote", "add", "corp", &url])?;
    machine.ok(
        &repo,
        &["git", "enable", "--remote", "corp", "--push-on-finish"],
    )?;
    let (passed, report) = sync_report(machine, &repo)?;
    assert!(passed, "{report}");
    assert_eq!(report["remote"], "corp");
    assert_eq!(
        local_setting(machine, &repo, "lenka.pushonfinish").as_deref(),
        Some("true")
    );

    let second = fresh_clone(&pair.second, "clone", &pair.remote)?;
    pair.second.ok(&second, &["init"])?;
    assert_eq!(
        local_setting(&pair.second, &second, "lenka.remote").as_deref(),
        Some("origin")
    );
    assert_eq!(
        local_setting(&pair.second, &second, "lenka.pushonfinish"),
        None
    );
    let task = pair.second.json(&second, &["task", "create", "New"])?;
    let (passed, report) = sync_report(&pair.second, &second)?;
    assert!(passed, "{report}");
    assert_eq!(report["remote"], "origin");
    assert_eq!(report["pushed"], true);
    let files = remote_files(machine, &pair.remote)?;
    assert!(files.contains(text(&task, "full_id")), "{files}");
    let settings = machine.git(&pair.remote, &["show", "lenka-data:lenka.toml"])?;
    assert!(!settings.contains("corp"), "{settings}");
    Ok(())
}

#[test]
fn a_missing_selected_remote_is_an_error() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    machine.ok(&repo, &["git", "enable"])?;
    machine.git(&repo, &["remote", "rename", "origin", "upstream"])?;
    let code = machine.fails(&repo, &["sync"])?;
    assert_eq!(code, "invalid_argument");
    Ok(())
}

#[test]
fn a_remote_settings_file_cannot_change_the_git_settings() -> TestResult {
    let pair = pair()?;
    let (first, second) = two_clones(&pair)?;
    let settings = fs::read_to_string(first.join(".git/lenka-data/lenka.toml"))?;
    let hostile =
        format!("{settings}\n[git]\nenabled = false\nremote = \"evil\"\npush_on_finish = true\n");
    push_hostile_files(&pair.first, &pair.remote, &[("lenka.toml", &hostile)])?;

    let (passed, report) = sync_report(&pair.second, &second)?;
    assert!(passed, "{report}");
    let task = pair.second.json(&second, &["task", "create", "New"])?;
    let (passed, report) = sync_report(&pair.second, &second)?;
    assert!(passed, "{report}");
    assert_eq!(report["remote"], "origin");
    assert_eq!(report["pushed"], true);
    assert_eq!(
        local_setting(&pair.second, &second, "lenka.pushonfinish"),
        None
    );
    let files = remote_files(&pair.first, &pair.remote)?;
    assert!(files.contains(text(&task, "full_id")), "{files}");
    Ok(())
}

#[test]
fn a_remote_cannot_change_the_project_id_or_the_format() -> TestResult {
    let new_id = "id = \"01a1b2e7-da96-7000-8000-000000000001\"";
    for (start, new_line, code) in [
        ("format_version = ", "format_version = 99", "format_too_new"),
        ("id = ", new_id, "invalid_file"),
    ] {
        let pair = pair()?;
        let (first, second) = two_clones(&pair)?;
        let settings = fs::read_to_string(first.join(".git/lenka-data/lenka.toml"))?;
        let line = settings
            .lines()
            .find(|row| row.starts_with(start))
            .ok_or("the settings have no such line")?;
        let hostile = settings.replacen(line, new_line, 1);
        push_hostile_files(&pair.first, &pair.remote, &[("lenka.toml", &hostile)])?;
        assert_eq!(pair.second.fails(&second, &["sync"])?, code);
        assert_eq!(pair.second.json(&second, &["doctor"])?["clean"], true);
    }
    Ok(())
}

#[test]
fn a_sync_reports_a_change_of_the_shared_settings() -> TestResult {
    let pair = pair()?;
    let (first, second) = two_clones(&pair)?;
    let settings = fs::read_to_string(first.join(".git/lenka-data/lenka.toml"))?;
    let changed = format!(
        "{settings}\n[types.review]\nstages = [{{ name = \"look\", actor = \"human\" }}]\n"
    );
    push_hostile_files(&pair.first, &pair.remote, &[("lenka.toml", &changed)])?;
    let (passed, report) = sync_report(&pair.second, &second)?;
    assert!(passed, "{report}");
    assert_eq!(report["settings_changed"][0], "types", "{report}");
    let (_, again) = sync_report(&pair.second, &second)?;
    assert_eq!(again["settings_changed"].as_array().map(Vec::len), Some(0));
    Ok(())
}

#[test]
fn an_open_merge_is_never_committed() -> TestResult {
    let pair = pair()?;
    let (first, second) = two_clones(&pair)?;
    let task = pair.first.json(&first, &["task", "create", "Shared"])?;
    let id = text(&task, "task").to_owned();
    pair.first.json(&first, &["sync"])?;
    pair.second.json(&second, &["sync"])?;
    let other = pair.second.json(&second, &["task", "create", "Other"])?;

    for (machine, repo, words) in [
        (&pair.first, &first, "first text"),
        (&pair.second, &second, "second text"),
    ] {
        machine.json(
            repo,
            &[
                "task",
                "update",
                &id,
                "--version",
                "1",
                "--description",
                words,
                "--reason",
                "Edit.",
            ],
        )?;
    }
    pair.first.json(&first, &["sync"])?;
    let (passed, report) = sync_report(&pair.second, &second)?;
    assert!(!passed, "{report}");
    assert!(
        report["conflict"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );

    let published = pair.first.git(&pair.remote, &["rev-parse", "lenka-data"])?;

    // A person starts the hand merge that the CLI text asks for.
    let data = second.join(".git/lenka-data");
    let mut merge = std::process::Command::new("git");
    pair.second.apply_env(&mut merge);
    let merged = merge
        .args(["merge", "-q", "origin/lenka-data"])
        .current_dir(&data)
        .output()?;
    assert!(!merged.status.success());

    // A finish in that clone stores the task, and it tells that Git did not store the change.
    let done = pair.second.json(
        &second,
        &[
            "task",
            "finish",
            text(&other, "task"),
            "--evidence",
            "It works.",
            "--override",
        ],
    )?;
    assert_eq!(done["status"], "done");
    let warning = done["plugin_warnings"]["untrusted_data"].to_string();
    assert!(warning.contains("open merge"), "{done}");

    assert_eq!(pair.second.fails(&second, &["sync"])?, "invalid_argument");
    let head = pair.first.git(&pair.remote, &["rev-parse", "lenka-data"])?;
    assert_eq!(head, published, "the sync changed the remote");
    let mut grep = std::process::Command::new("git");
    pair.first.apply_env(&mut grep);
    let markers = grep
        .args(["grep", "-l", "<<<<<<<", "lenka-data"])
        .current_dir(&pair.remote)
        .output()?;
    assert!(!markers.status.success(), "the remote has conflict markers");
    Ok(())
}

#[test]
fn a_conflict_marker_in_a_local_file_is_not_committed() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    machine.ok(&repo, &["git", "enable"])?;
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    machine.json(&repo, &["sync"])?;
    let file = repo
        .join(".git/lenka-data/tasks")
        .join(text(&task, "full_id"))
        .join("task.md");
    let bad = format!("{}\n<<<<<<< HEAD\nx\n", fs::read_to_string(&file)?);
    fs::write(&file, bad)?;
    assert_eq!(machine.fails(&repo, &["sync"])?, "invalid_file");
    let tree = machine.git(
        &pair.remote,
        &[
            "show",
            &format!("lenka-data:tasks/{}/task.md", text(&task, "full_id")),
        ],
    )?;
    assert!(!tree.contains("<<<<<<<"), "{tree}");
    Ok(())
}

/// A hand edit gets no secret check at its write, so the last place for a check is the push.
#[test]
fn a_sync_does_not_push_a_secret_from_a_hand_edit() -> TestResult {
    let pair = pair()?;
    let (first, _second) = two_clones(&pair)?;
    let task = pair.first.json(&first, &["task", "create", "A task"])?;
    pair.first.json(&first, &["sync"])?;
    let id = text(&task, "task").to_owned();
    let data = first.join(".git/lenka-data");
    let task_dir = fs::read_dir(data.join("tasks"))?
        .filter_map(Result::ok)
        .find(|entry| entry.file_name().to_string_lossy().contains(id.as_str()))
        .ok_or("the task directory exists")?;
    let full_id = task_dir.file_name().to_string_lossy().into_owned();
    let file = task_dir.path().join("task.md");
    let token = format!("{}{}", "ghp_", "a1B2".repeat(10));
    fs::write(&file, format!("{}\n{token}\n", fs::read_to_string(&file)?))?;

    let (passed, report) = sync_report(&pair.first, &first)?;
    assert!(!passed, "{report}");
    assert_eq!(report["pushed"], false, "{report}");
    let error = report["push_error"].as_str().unwrap_or_default();
    assert!(error.contains("allow-secret"), "{report}");
    assert!(error.contains(full_id.as_str()), "{report}");
    let on_remote = pair.first.git(
        &data,
        &[
            "show",
            &format!("origin/lenka-data:tasks/{full_id}/task.md"),
        ],
    )?;
    assert!(!on_remote.contains(&token), "the token reached the remote");

    let output = pair
        .first
        .run(&first, &["sync", "--allow-secret", "--json"])?;
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["pushed"], true, "{report}");
    Ok(())
}

/// A memory file that a test can change by hand. A memory file has no copy in a version folder.
struct EditedMemory {
    file: PathBuf,
    original: String,
    token: String,
}

/// Adds a memory, and puts a token in its file by hand.
fn memory_with_token(machine: &Machine, repo: &Path) -> TestResult<EditedMemory> {
    let memory = machine.json(repo, &["memory", "add", "A note."])?;
    let id = text(&memory, "memory").to_owned();
    let file = fs::read_dir(repo.join(".git/lenka-data/memory"))?
        .filter_map(Result::ok)
        .find(|entry| entry.file_name().to_string_lossy().contains(id.as_str()))
        .ok_or("the memory file exists")?
        .path();
    let original = fs::read_to_string(&file)?;
    let token = format!("{}{}", "ghp_", "a1B2".repeat(10));
    fs::write(&file, format!("{original}\n{token}\n"))?;
    Ok(EditedMemory {
        file,
        original,
        token,
    })
}

/// Returns the full history of the data branch on the remote, or an empty text with no branch.
fn remote_history(machine: &Machine, remote: &Path) -> TestResult<String> {
    if machine
        .git(remote, &["branch", "--list", "lenka-data"])?
        .is_empty()
    {
        return Ok(String::new());
    }
    machine.git(remote, &["log", "-p", "lenka-data"])
}

/// The push sends each local commit, so a secret that a later commit removes must stop it too.
#[test]
fn a_secret_in_an_earlier_local_commit_is_not_pushed() -> TestResult {
    let pair = pair()?;
    let (first, _second) = two_clones(&pair)?;
    let edited = memory_with_token(&pair.first, &first)?;
    let (passed, report) = sync_report(&pair.first, &first)?;
    assert!(!passed, "{report}");
    let error = report["push_error"].as_str().unwrap_or_default();
    assert!(
        error.starts_with("secret_detected: The file memory/"),
        "{report}"
    );
    assert!(
        !error.contains("secret_detected: secret_detected"),
        "{report}"
    );
    assert!(
        error.contains("git reset --soft origin/lenka-data"),
        "{report}"
    );

    // The commit with the token stays, so a change of the file is not enough.
    fs::write(&edited.file, &edited.original)?;
    let (passed, report) = sync_report(&pair.first, &first)?;
    assert!(!passed, "{report}");
    assert_eq!(report["pushed"], false, "{report}");
    let history = remote_history(&pair.first, &pair.remote)?;
    assert!(
        !history.contains(&edited.token),
        "the token reached the remote"
    );

    // The steps in the message remove the commit with the token.
    pair.first.git(
        &first.join(".git/lenka-data"),
        &["reset", "--soft", "origin/lenka-data"],
    )?;
    let (passed, report) = sync_report(&pair.first, &first)?;
    assert!(passed, "{report}");
    assert_eq!(report["pushed"], true, "{report}");
    let history = remote_history(&pair.first, &pair.remote)?;
    assert!(history.contains("A note."), "{history}");
    assert!(
        !history.contains(&edited.token),
        "the token reached the remote"
    );
    Ok(())
}

/// The first push has no tracking ref, so the check reads all objects of the branch.
#[test]
fn a_secret_in_the_first_push_is_not_pushed() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    machine.ok(&repo, &["git", "enable"])?;
    let edited = memory_with_token(machine, &repo)?;
    let (passed, report) = sync_report(machine, &repo)?;
    assert!(!passed, "{report}");
    let error = report["push_error"].as_str().unwrap_or_default();
    assert!(error.contains("git rebase -i --root"), "{report}");

    fs::write(&edited.file, &edited.original)?;
    let (passed, report) = sync_report(machine, &repo)?;
    assert!(!passed, "{report}");
    assert_eq!(report["pushed"], false, "{report}");
    assert!(remote_history(machine, &pair.remote)?.is_empty());
    Ok(())
}

/// The push on a finish has the same check as `lenka sync`.
#[test]
fn a_push_on_finish_does_not_push_a_secret() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    machine.ok(&repo, &["git", "enable", "--push-on-finish"])?;
    machine.json(&repo, &["sync"])?;
    let edited = memory_with_token(machine, &repo)?;
    let finish = |title: &str| -> TestResult<Value> {
        let task = machine.json(&repo, &["task", "create", title])?;
        let id = text(&task, "task").to_owned();
        machine.json(&repo, &["task", "claim", &id])?;
        machine.json(&repo, &["task", "finish", &id, "--evidence", "It works."])
    };

    let done = finish("First")?;
    assert_eq!(done["status"], "done", "{done}");
    assert!(done.to_string().contains("allow-secret"), "{done}");
    fs::write(&edited.file, &edited.original)?;
    let done = finish("Second")?;
    assert!(done.to_string().contains("allow-secret"), "{done}");
    let history = remote_history(machine, &pair.remote)?;
    assert!(
        !history.contains(&edited.token),
        "the token reached the remote"
    );
    Ok(())
}

/// A person can delete the branch on the remote to remove a secret. The tracking ref of the clone
/// then names a commit that the remote no longer has, so the push at a finish must check all
/// objects.
#[test]
fn a_push_on_finish_checks_all_objects_after_the_remote_lost_the_branch() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    machine.ok(&repo, &["git", "enable", "--push-on-finish"])?;
    machine.json(&repo, &["sync"])?;
    let edited = memory_with_token(machine, &repo)?;
    machine.json(&repo, &["sync", "--allow-secret"])?;
    assert!(remote_history(machine, &pair.remote)?.contains(&edited.token));
    machine.git(&pair.remote, &["branch", "-D", "lenka-data"])?;

    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let id = text(&task, "task").to_owned();
    machine.json(&repo, &["task", "claim", &id])?;
    let done = machine.json(&repo, &["task", "finish", &id, "--evidence", "It works."])?;
    assert_eq!(done["status"], "done", "{done}");
    assert!(done.to_string().contains("allow-secret"), "{done}");
    assert!(
        !remote_history(machine, &pair.remote)?.contains(&edited.token),
        "the token reached the remote again"
    );
    Ok(())
}

#[test]
fn a_failed_push_still_reports_the_merge_and_rebuilds_the_index() -> TestResult {
    let pair = pair()?;
    let (first, second) = two_clones(&pair)?;
    pair.first
        .json(&first, &["task", "create", "From the first clone"])?;
    pair.first.json(&first, &["sync"])?;
    let local = pair.second.json(&second, &["task", "create", "Local"])?;

    let hook = pair.remote.join("hooks/pre-receive");
    fs::create_dir_all(pair.remote.join("hooks"))?;
    fs::write(&hook, "#!/bin/sh\nexit 1\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))?;
    }

    let (passed, report) = sync_report(&pair.second, &second)?;
    assert!(!passed, "{report}");
    assert_eq!(report["merged"], true, "{report}");
    assert_eq!(report["pushed"], false, "{report}");
    assert!(report["push_error"].is_string(), "{report}");
    let feed = fs::read_to_string(second.join(".git/lenka-data/.runtime/feed.jsonl"))?;
    assert!(feed.contains("reindex"), "{feed}");
    let rows = pair.second.json(&second, &["task", "list"])?.to_string();
    assert!(rows.contains("From the first clone"), "{rows}");
    assert!(rows.contains(text(&local, "task")), "{rows}");
    Ok(())
}

#[test]
fn a_failed_commit_is_in_the_result_of_a_finish() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    machine.ok(&repo, &["git", "enable"])?;
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    machine.git(
        &repo.join(".git/lenka-data"),
        &["checkout", "-q", "--detach"],
    )?;
    let done = machine.json(
        &repo,
        &[
            "task",
            "finish",
            text(&task, "task"),
            "--evidence",
            "It works.",
            "--override",
        ],
    )?;
    assert_eq!(done["status"], "done");
    assert!(
        done["plugin_warnings"]["untrusted_data"].is_array(),
        "{done}"
    );
    Ok(())
}

#[test]
fn a_changed_id_in_the_settings_stops_the_project() -> TestResult {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let repo = machine.project("api")?;
    let file = repo.join(".git/lenka-data/lenka.toml");
    let settings = fs::read_to_string(&file)?;
    let line = settings
        .lines()
        .find(|row| row.starts_with("id = "))
        .unwrap_or("");
    fs::write(
        &file,
        settings.replacen(line, "id = \"01a1b2e7-da96-7000-8000-000000000001\"", 1),
    )?;
    assert_eq!(
        machine.fails(&repo, &["task", "list"])?,
        "project_not_available"
    );
    Ok(())
}

#[test]
fn a_stuck_write_leaves_reads_and_doctor_working() -> TestResult {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let repo = machine.project("api")?;
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let data = repo.join(".git/lenka-data");
    // The journal names a file, and that path is a directory, so a replay cannot finish.
    fs::create_dir_all(data.join("tasks/blocked.md"))?;
    fs::write(
        data.join(".runtime/pending.json"),
        r#"[{"op":"put","path":"tasks/blocked.md","text":"x"}]"#,
    )?;

    let shown = machine.json(&repo, &["task", "show", text(&task, "task")])?;
    assert_eq!(shown["untrusted_data"]["title"], "A task");
    let output = machine.run(&repo, &["doctor", "--json"])?;
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout)?;
    let stuck = report["report"]["unfinished_write"].to_string();
    assert!(stuck.contains("tasks/blocked.md"), "{report}");
    let code = machine.fails(&repo, &["task", "create", "Refused"])?;
    assert_eq!(code, "io");
    fs::remove_dir(data.join("tasks/blocked.md"))?;
    machine.json(&repo, &["task", "create", "Accepted"])?;
    Ok(())
}

#[test]
fn a_conflict_marker_in_an_entry_does_not_stop_a_commit() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    machine.ok(&repo, &["git", "enable"])?;
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    machine.json(
        &repo,
        &[
            "task",
            "entry",
            text(&task, "task"),
            "--kind",
            "comment",
            "--text",
            "<<<<<<< a quote of a merge",
        ],
    )?;
    let (passed, report) = sync_report(machine, &repo)?;
    assert!(passed, "{report}");
    assert_eq!(report["pushed"], true);
    Ok(())
}

#[test]
fn an_artifact_that_is_a_git_lfs_pointer_is_refused() -> TestResult {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let repo = machine.project("api")?;
    let pointer = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize 12\n",
        "ab".repeat(32)
    );
    fs::write(repo.join("x.bin"), pointer)?;
    assert_eq!(
        machine.fails(&repo, &["artifact", "create", "x.bin"])?,
        "invalid_argument"
    );
    Ok(())
}

#[test]
fn a_temporary_file_from_a_stopped_process_stays_out_of_git() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = publish(machine, "api", &pair.remote)?;
    machine.ok(&repo, &["git", "enable"])?;
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let data = repo.join(".git/lenka-data");
    fs::write(data.join(".tmp-1-0"), "partial")?;
    let inner = format!("tasks/{}/.tmp-1-1", text(&task, "full_id"));
    fs::write(data.join(&inner), "partial")?;

    let (passed, report) = sync_report(machine, &repo)?;
    assert!(passed, "{report}");
    // Such a file is not a choice of a person, so the report does not list it.
    assert_eq!(
        report["skipped"].as_array().map(Vec::len),
        Some(0),
        "{report}"
    );
    let files = remote_files(machine, &pair.remote)?;
    assert!(!files.contains(".tmp-"), "{files}");

    let second = fresh_clone(&pair.second, "clone", &pair.remote)?;
    pair.second.ok(&second, &["init"])?;
    Ok(())
}

/// Edits a task file by hand, and returns the full task ID. The file keeps its size and its time,
/// so only a compare of the text finds this edit.
fn hand_edit(dir: &Path, short: &str, from: &str, to: &str) -> TestResult<String> {
    let full = fs::read_dir(dir.join("tasks"))?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| name.contains(short))
        .ok_or("the task directory exists")?;
    let file = dir.join("tasks").join(&full).join("task.md");
    let old = fs::read_to_string(&file)?;
    assert!(old.contains(from), "{old}");
    assert_eq!(from.len(), to.len());
    let time = fs::metadata(&file)?.modified()?;
    fs::write(&file, old.replace(from, to))?;
    fs::File::options()
        .write(true)
        .open(&file)?
        .set_modified(time)?;
    Ok(full)
}

#[test]
fn a_hand_edit_is_a_version_before_a_sync_so_clones_do_not_conflict() -> TestResult {
    let pair = pair()?;
    let (first, second) = two_clones(&pair)?;
    let task = pair.first.json(
        &first,
        &["task", "create", "A task", "--description", "Old text."],
    )?;
    let id = text(&task, "task").to_owned();
    pair.first.json(&first, &["sync"])?;
    pair.second.json(&second, &["sync"])?;
    // This read stores an index snapshot with the current file stamps.
    pair.first.json(&first, &["task", "show", &id])?;

    hand_edit(
        &first.join(".git/lenka-data"),
        &id,
        "Old text.",
        "New text.",
    )?;
    let (done, _) = sync_report(&pair.first, &first)?;
    assert!(done);
    let files = remote_files(&pair.first, &pair.remote)?;
    assert!(files.contains("versions/000002/meta.toml"), "{files}");

    // Both clones now read the task and sync. No clone needs a second version 2.
    pair.second.json(&second, &["sync"])?;
    pair.second.json(&second, &["task", "show", &id])?;
    pair.first.json(&first, &["task", "show", &id])?;
    for (machine, repo) in [(&pair.first, &first), (&pair.second, &second)] {
        let (done, report) = sync_report(machine, repo)?;
        assert!(done, "{report}");
    }
    Ok(())
}

#[test]
fn a_merged_change_stops_a_finish_until_the_holder_reads_it() -> TestResult {
    let pair = pair()?;
    let (first, second) = two_clones(&pair)?;
    let task = pair.first.json(&first, &["task", "create", "A task"])?;
    let id = text(&task, "task").to_owned();
    pair.first.json(&first, &["sync"])?;
    pair.second.json(&second, &["sync"])?;
    pair.second.json(&second, &["task", "claim", &id])?;
    pair.second.json(&second, &["sync"])?;
    pair.first.json(&first, &["sync"])?;

    pair.first.json(
        &first,
        &[
            "task",
            "update",
            &id,
            "--version",
            "1",
            "--description",
            "Do not ship this.",
            "--reason",
            "The requirement changed.",
        ],
    )?;
    pair.first.json(&first, &["sync"])?;
    let (done, report) = sync_report(&pair.second, &second)?;
    assert!(done, "{report}");
    assert_eq!(report["merged"], Value::Bool(true));

    let refused = pair
        .second
        .fails(&second, &["task", "finish", &id, "--evidence", "It works."])?;
    assert!(refused.contains("unread_changes"), "{refused}");
    pair.second.json(&second, &["task", "show", &id])?;
    pair.second
        .json(&second, &["task", "finish", &id, "--evidence", "It works."])?;
    Ok(())
}

/// A worktree of the person can be absent for a short time, and its record must stay.
#[test]
fn enable_keeps_the_record_of_an_absent_worktree_of_the_person() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = machine.project("api")?;
    let worktree = machine.root.join("wt");
    let moved = machine.root.join("wt-moved");
    let path = worktree.to_string_lossy().into_owned();
    machine.git(&repo, &["worktree", "add", "-q", "-b", "feature", &path])?;
    fs::rename(&worktree, &moved)?;

    let task = machine.json(&repo, &["task", "create", "A task"])?;
    machine.ok(&repo, &["git", "enable"])?;
    fs::rename(&moved, &worktree)?;
    machine.git(&worktree, &["status", "-s"])?;

    // A new restore finds the record of its own absent worktree, and it can use the branch.
    let data = repo.join(".git/lenka-data");
    fs::remove_dir_all(&data)?;
    let restored = machine.json(&repo, &["init"])?;
    assert_eq!(restored["restored"], true, "{restored}");
    machine.json(&repo, &["task", "show", text(&task, "task")])?;
    machine.git(&worktree, &["status", "-s"])?;
    Ok(())
}

/// `enable` does not move the data. A stop at any step keeps the data at its normal path, and a
/// writer in another process cannot lose a write.
#[cfg(unix)]
#[test]
fn a_stopped_enable_keeps_the_data_and_can_run_again() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let pair = pair()?;
    let machine = &pair.first;
    let repo = machine.project("api")?;
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let id = text(&task, "task").to_owned();

    let real = machine.git(&repo, &["--exec-path"])?;
    let real = format!("{}/git", real.trim());
    let bin = machine.root.join("bin");
    fs::create_dir_all(&bin)?;
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n'{real}' \"$@\"\nstatus=$?\ncase \" $* \" in *\" worktree add \"*) kill -9 $PPID;; esac\nexit $status\n"
        ),
    )?;
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))?;
    let search = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let stopped = machine
        .command(&repo, &["git", "enable"])
        .env("PATH", search)
        .output()?;
    assert!(!stopped.status.success());

    machine.json(&repo, &["task", "show", &id])?;
    machine.ok(&repo, &["git", "enable"])?;
    machine.json(&repo, &["task", "show", &id])?;
    let data = repo.join(".git/lenka-data");
    let files = machine.git(&data, &["ls-files"])?;
    assert!(files.contains("lenka.toml"), "{files}");
    let names: Vec<_> = fs::read_dir(repo.join(".git"))?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names.iter().any(|name| name.contains(".new-")),
        "{names:?}"
    );
    Ok(())
}

/// A push from a finish cannot block an agent call for as long as the remote takes.
#[cfg(unix)]
#[test]
fn a_stuck_push_in_an_agent_call_stops_at_the_time_limit() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let pair = pair()?;
    let machine = &pair.first;
    let repo = machine.project("api")?;
    machine.git(
        &repo,
        &["remote", "add", "origin", "ssh://example.invalid/data.git"],
    )?;
    machine.ok(&repo, &["git", "enable", "--push-on-finish"])?;
    let stub = machine.root.join("stuck-ssh");
    fs::write(&stub, "#!/bin/sh\nsleep 20\n")?;
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755))?;

    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let id = text(&task, "task").to_owned();
    machine.json(&repo, &["task", "claim", &id])?;
    let start = std::time::Instant::now();
    let output = machine
        .command(
            &repo,
            &["--json", "task", "finish", &id, "--evidence", "It works."],
        )
        .env("GIT_SSH_COMMAND", &stub)
        .env("LENKA_GIT_PUSH_LIMIT_MS", "200")
        .output()?;
    assert!(start.elapsed().as_secs() < 15, "{:?}", start.elapsed());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains("lenka stopped it"), "{stdout}\n{stderr}");
    Ok(())
}

/// The SSH client ignores `GIT_TERMINAL_PROMPT`, so an agent call turns off its prompt.
#[cfg(unix)]
#[test]
fn a_push_in_an_agent_call_turns_off_the_ssh_prompt() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let pair = pair()?;
    let machine = &pair.first;
    let repo = machine.project("api")?;
    machine.git(
        &repo,
        &["remote", "add", "origin", "ssh://example.invalid/data.git"],
    )?;
    machine.ok(&repo, &["git", "enable", "--push-on-finish"])?;
    let bin = machine.root.join("bin");
    fs::create_dir_all(&bin)?;
    let record = machine.root.join("ssh-args");
    fs::write(
        bin.join("ssh"),
        format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 1\n", record.display()),
    )?;
    fs::set_permissions(bin.join("ssh"), fs::Permissions::from_mode(0o755))?;
    let search = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let id = text(&task, "task").to_owned();
    machine.json(&repo, &["task", "claim", &id])?;
    machine
        .command(
            &repo,
            &["--json", "task", "finish", &id, "--evidence", "It works."],
        )
        .env("PATH", search)
        .env_remove("GIT_SSH_COMMAND")
        .env_remove("GIT_SSH")
        .output()?;
    let args = fs::read_to_string(&record)?;
    assert!(args.contains("BatchMode=yes"), "{args}");
    Ok(())
}

#[test]
fn a_value_of_the_environment_does_not_change_the_target_of_a_push() -> TestResult {
    let pair = pair()?;
    let repo = publish(&pair.first, "api", &pair.remote)?;
    pair.first.ok(&repo, &["git", "enable"])?;
    let output = pair
        .first
        .command(&repo, &["sync"])
        .env("GIT_NAMESPACE", "other")
        .output()?;
    assert!(output.status.success());
    let refs = pair.first.git(&pair.remote, &["show-ref"])?;
    assert!(refs.contains(" refs/heads/lenka-data"), "{refs}");
    assert!(!refs.contains("namespaces"), "{refs}");
    Ok(())
}

fn shared_with_remote(machine: &Machine, remote: &Path) -> TestResult<Value> {
    let url = remote.to_string_lossy().into_owned();
    machine.json(&machine.root, &["init", "--shared", "--name", "api"])?;
    machine.json(
        &machine.root,
        &["--project", "api", "git", "enable", "--remote", &url],
    )
}

/// A second machine must not start separate data on one remote, because the data cannot merge.
#[test]
fn a_second_machine_learns_how_to_use_the_shared_data() -> TestResult {
    let pair = pair()?;
    let url = pair.remote.to_string_lossy().into_owned();
    shared_with_remote(&pair.first, &pair.remote)?;
    let task = pair.first.json(
        &pair.first.root,
        &["--project", "api", "task", "create", "A task"],
    )?;
    pair.first
        .ok(&pair.first.root, &["--project", "api", "sync"])?;
    // A repeat call on the first machine is still safe.
    shared_with_remote_again(&pair.first, &url)?;

    let second = &pair.second;
    second.json(&second.root, &["init", "--shared", "--name", "api"])?;
    let refused = second.run(
        &second.root,
        &["--project", "api", "git", "enable", "--remote", &url],
    )?;
    assert!(!refused.status.success());
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(
        message.contains("git clone --branch lenka-data"),
        "{message}"
    );

    // The steps in the message work.
    let directory = second.root.join("shared-data");
    let directory_text = directory.to_string_lossy().into_owned();
    second.git(
        &second.root,
        &[
            "clone",
            "-q",
            "--branch",
            "lenka-data",
            &url,
            &directory_text,
        ],
    )?;
    second.json(
        &second.root,
        &["init", "--dir", &directory_text, "--name", "from-remote"],
    )?;
    second.json(
        &second.root,
        &[
            "--project",
            "from-remote",
            "task",
            "show",
            text(&task, "task"),
        ],
    )?;
    Ok(())
}

fn shared_with_remote_again(machine: &Machine, url: &str) -> TestResult {
    machine.json(
        &machine.root,
        &["--project", "api", "git", "enable", "--remote", url],
    )?;
    Ok(())
}

/// A remote can get a branch after the local enable. The sync then gives the steps.
#[test]
fn a_sync_with_data_from_another_start_gives_the_steps() -> TestResult {
    let pair = pair()?;
    shared_with_remote(&pair.first, &pair.remote)?;
    shared_with_remote(&pair.second, &pair.remote)?;
    pair.first
        .ok(&pair.first.root, &["--project", "api", "sync"])?;
    let output = pair
        .second
        .run(&pair.second.root, &["--project", "api", "sync"])?;
    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(
        message.contains("git clone --branch lenka-data"),
        "{message}"
    );
    Ok(())
}

/// A plugin that is off must not run Git for the data of a remote.
#[test]
fn init_does_not_restore_while_the_git_plugin_is_off() -> TestResult {
    let pair = pair()?;
    let repo = publish(&pair.first, "api", &pair.remote)?;
    pair.first.ok(&repo, &["git", "enable"])?;
    pair.first.ok(&repo, &["sync"])?;
    let second = &pair.second;
    fs::remove_file(second.root.join("config/plugins.toml"))?;
    let clone = fresh_clone(second, "api", &pair.remote)?;
    second.git(&clone, &["fetch", "-q", "origin"])?;
    let created = second.json(&clone, &["init", "--name", "api"])?;
    assert_eq!(created["restored"], false, "{created}");
    let worktrees = second.git(&clone, &["worktree", "list"])?;
    assert_eq!(worktrees.lines().count(), 1, "{worktrees}");
    Ok(())
}

/// A branch with no project file is not lenka data, so a new project must not start on it.
#[test]
fn a_branch_with_no_project_file_stops_each_init() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = machine.repo("api")?;
    let tree = machine.git(&repo, &["hash-object", "-w", "-t", "tree", "/dev/null"])?;
    let commit = machine.git(&repo, &["commit-tree", tree.trim(), "-m", "Not lenka data"])?;
    machine.git(&repo, &["branch", "lenka-data", commit.trim()])?;
    for _ in 0..2 {
        let output = machine.run(&repo, &["init"])?;
        assert!(!output.status.success());
        let message = String::from_utf8_lossy(&output.stderr);
        assert!(message.contains("it is not lenka data"), "{message}");
    }
    assert!(!repo.join(".git/lenka-data/lenka.toml").exists());
    Ok(())
}

/// A file on the disk is newer than the branch, and a file that only the branch has comes back.
#[test]
fn enable_keeps_newer_files_and_adds_files_of_the_branch() -> TestResult {
    let pair = pair()?;
    let machine = &pair.first;
    let repo = machine.project("api")?;
    machine.ok(&repo, &["git", "enable"])?;
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let memory = machine.json(&repo, &["memory", "add", "A note."])?;
    machine.ok(&repo, &["sync"])?;
    let data = repo.join(".git/lenka-data");
    let full = |dir: &str, short: &str| -> TestResult<PathBuf> {
        let found = fs::read_dir(data.join(dir))?
            .flatten()
            .find(|entry| entry.file_name().to_string_lossy().contains(short))
            .ok_or("no such file")?;
        Ok(found.path())
    };
    let task_dir = full("tasks", text(&task, "task"))?;
    let memory_file = full("memory", text(&memory, "memory"))?;

    fs::remove_file(data.join(".git"))?;
    machine.git(&repo, &["worktree", "prune"])?;
    fs::remove_file(&memory_file)?;
    let newer = fs::read_to_string(task_dir.join("task.md"))?.replace("A task", "A newer title");
    fs::write(task_dir.join("task.md"), &newer)?;

    machine.ok(&repo, &["git", "enable"])?;
    assert!(memory_file.exists(), "the file of the branch came back");
    assert_eq!(fs::read_to_string(task_dir.join("task.md"))?, newer);
    Ok(())
}
