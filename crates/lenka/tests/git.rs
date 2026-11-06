// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Tests of the Git commands with real processes: a push, a fresh clone, and a conflict.

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
    assert!(settings.contains("push_on_finish = true"), "{settings}");
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
