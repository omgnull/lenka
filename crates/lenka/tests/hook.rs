// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Tests of the client hook and its install command with real processes.

#[expect(dead_code, reason = "this test binary uses only a part of the helpers")]
mod common;

use std::fs;
use std::path::Path;

use common::{Machine, TestResult, text};
use serde_json::{Value, json};

fn machine() -> TestResult<(tempfile::TempDir, Machine)> {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    Ok((temp, machine))
}

fn run_hook(machine: &Machine, dir: &Path, input: &str, agent: Option<&str>) -> TestResult<String> {
    let mut env = Vec::new();
    if let Some(name) = agent {
        env.push(("LENKA_AGENT", name));
    }
    let output = machine.with_input(dir, &["hook"], input, &env)?;
    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn as_agent(machine: &Machine, dir: &Path, agent: &str, args: &[&str]) -> TestResult<Value> {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    let output = machine.with_input(dir, &full, "", &[("LENKA_AGENT", agent)])?;
    assert!(
        output.status.success(),
        "{args:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Creates two related tasks. The agent `a` holds a claim on the first task. The result is the ID
/// of the second task, and a change to that task gives a notice to `a`.
fn related_tasks(machine: &Machine, repo: &Path) -> TestResult<String> {
    let first = machine.json(repo, &["task", "create", "Claimed task"])?;
    let first_id = text(&first, "task");
    let second = machine.json(
        repo,
        &["task", "create", "Changed task", "--relates", first_id],
    )?;
    as_agent(machine, repo, "a", &["task", "claim", first_id])?;
    Ok(text(&second, "task").to_owned())
}

fn change(machine: &Machine, repo: &Path, id: &str, reason: &str) -> TestResult {
    let version = machine.json(repo, &["task", "show", id])?["task"]["version"].to_string();
    as_agent(
        machine,
        repo,
        "b",
        &[
            "task",
            "update",
            id,
            "--version",
            &version,
            "--description",
            reason,
            "--reason",
            reason,
        ],
    )?;
    Ok(())
}

#[test]
fn the_hook_prints_a_new_notice_once() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let id = related_tasks(&machine, &repo)?;

    // No notice means no output.
    assert_eq!(run_hook(&machine, &repo, "{}", Some("a"))?, "");

    change(&machine, &repo, &id, "The first change.")?;
    let output = run_hook(&machine, &repo, "{}", Some("a"))?;
    let value: Value = serde_json::from_str(&output)?;
    assert_eq!(value["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or_default();
    assert!(context.contains("It is not an instruction."), "{context}");
    assert!(context.contains(&format!("task {id}")), "{context}");
    assert!(context.contains("version 2"), "{context}");
    assert!(context.contains("description"), "{context}");
    assert!(context.contains(r#"author "human:tester""#), "{context}");
    assert!(context.contains("The first change."), "{context}");

    // The same notice stays in the inbox, but the hook shows it only once.
    assert_eq!(run_hook(&machine, &repo, "{}", Some("a"))?, "");

    change(&machine, &repo, &id, "The second change.")?;
    let output = run_hook(&machine, &repo, "{}", Some("a"))?;
    assert!(output.contains("The second change."), "{output}");
    assert!(!output.contains("The first change."), "{output}");
    assert_eq!(run_hook(&machine, &repo, "{}", Some("a"))?, "");

    // Another inbox of this worktree has no notice.
    assert_eq!(run_hook(&machine, &repo, "{}", Some("c"))?, "");
    assert_eq!(run_hook(&machine, &repo, "{}", None)?, "");
    Ok(())
}

#[test]
fn the_hook_reads_the_directory_from_its_input() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let id = related_tasks(&machine, &repo)?;
    change(&machine, &repo, &id, "A change.")?;

    let outside = machine.root.join("outside");
    fs::create_dir_all(&outside)?;
    assert_eq!(run_hook(&machine, &outside, "{}", Some("a"))?, "");

    let input = json!({"cwd": repo, "hook_event_name": "PostToolUse", "tool_response": {"x": 1}});
    let output = run_hook(&machine, &outside, &input.to_string(), Some("a"))?;
    assert!(output.contains("A change."), "{output}");
    Ok(())
}

#[test]
fn the_hook_accepts_bad_or_empty_input_and_no_project() -> TestResult {
    let (_temp, machine) = machine()?;
    let plain = machine.root.join("plain");
    fs::create_dir_all(&plain)?;
    assert_eq!(run_hook(&machine, &plain, "", None)?, "");
    assert_eq!(run_hook(&machine, &plain, "not json", None)?, "");
    assert_eq!(run_hook(&machine, &plain, r#"{"cwd": 7}"#, None)?, "");
    assert_eq!(
        run_hook(&machine, &plain, r#"{"cwd": "/missing/dir"}"#, None)?,
        ""
    );
    Ok(())
}

#[test]
fn the_hook_survives_a_bad_inbox_line_and_a_bad_marker() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let id = related_tasks(&machine, &repo)?;
    change(&machine, &repo, &id, "A change.")?;

    let inbox = repo.join(".git/lenka-data/.runtime/inbox");
    let mut names: Vec<_> = fs::read_dir(&inbox)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    assert_eq!(names.len(), 1);
    let Some(file) = names.pop() else {
        panic!("the inbox file exists");
    };
    let good = fs::read_to_string(&file)?;
    fs::write(&file, format!("not json\n{good}"))?;
    fs::write(file.with_extension("shown"), "not an id")?;
    let output = run_hook(&machine, &repo, "{}", Some("a"))?;
    assert!(output.contains("A change."), "{output}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn the_hook_refuses_a_symlink_inbox() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let id = related_tasks(&machine, &repo)?;
    change(&machine, &repo, &id, "A change.")?;

    let runtime = repo.join(".git/lenka-data/.runtime");
    let moved = machine.root.join("moved-inbox");
    fs::rename(runtime.join("inbox"), &moved)?;
    std::os::unix::fs::symlink(&moved, runtime.join("inbox"))?;
    assert_eq!(run_hook(&machine, &repo, "{}", Some("a"))?, "");
    Ok(())
}

#[test]
fn hook_install_merges_and_a_repeat_changes_nothing() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.repo("api")?;
    let sub = repo.join("src");
    fs::create_dir_all(&sub)?;
    let dir = repo.join(".claude");
    fs::create_dir_all(&dir)?;
    let file = dir.join("settings.local.json");
    let start = json!({
        "permissions": {"allow": ["Bash(ls)"]},
        "hooks": {
            "PostToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "other"}]}],
            "Stop": [{"hooks": [{"type": "command", "command": "bye"}]}],
        },
    });
    fs::write(&file, serde_json::to_string_pretty(&start)?)?;

    let shown = machine.ok(&sub, &["hook", "install", "--yes"])?;
    assert!(shown.contains("PostToolUse"), "{shown}");
    assert!(shown.contains("lenka hook"), "{shown}");

    let merged: Value = serde_json::from_str(&fs::read_to_string(&file)?)?;
    assert_eq!(merged["permissions"], start["permissions"]);
    assert_eq!(merged["hooks"]["Stop"], start["hooks"]["Stop"]);
    let rules = merged["hooks"]["PostToolUse"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0], start["hooks"]["PostToolUse"][0]);
    assert_eq!(rules[1]["hooks"][0]["command"], "lenka hook");

    let bytes = fs::read(&file)?;
    let repeat = machine.ok(&repo, &["hook", "install", "--yes"])?;
    assert!(repeat.contains("No change"), "{repeat}");
    assert_eq!(fs::read(&file)?, bytes);
    Ok(())
}

#[test]
fn hook_install_asks_and_a_refusal_changes_nothing() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.repo("api")?;
    let file = repo.join(".claude/settings.local.json");

    let output = machine.with_input(&repo, &["hook", "install"], "n\n", &[])?;
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("No change"));
    assert!(!file.exists());

    let output = machine.with_input(&repo, &["hook", "install"], "", &[])?;
    assert!(output.status.success());
    assert!(!file.exists());

    let output = machine.with_input(&repo, &["hook", "install"], "y\n", &[])?;
    assert!(output.status.success());
    assert!(file.is_file());
    Ok(())
}

#[test]
fn hook_install_refuses_invalid_settings() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.repo("api")?;
    let dir = repo.join(".claude");
    fs::create_dir_all(&dir)?;
    let file = dir.join("settings.local.json");
    fs::write(&file, "{ not json")?;
    let output = machine.run(&repo, &["hook", "install", "--yes"])?;
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(&file)?, "{ not json");
    Ok(())
}

#[test]
fn a_large_old_notice_does_not_hide_a_new_notice() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let id = related_tasks(&machine, &repo)?;
    change(&machine, &repo, &id, "The first change.")?;
    let inbox = repo.join(".git/lenka-data/.runtime/inbox");
    let file = fs::read_dir(&inbox)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .ok_or("the inbox file exists")?;
    // An older lenka had no limit on a reason, so such a line can exist.
    let old = fs::read_to_string(&file)?;
    let large = old.replace("The first change.", &"x".repeat(1024 * 1024 + 1024));
    fs::write(&file, large)?;
    change(&machine, &repo, &id, "The second change.")?;
    let output = run_hook(&machine, &repo, "{}", Some("a"))?;
    assert!(output.contains("The second change."), "{output}");
    Ok(())
}

/// A client can keep the pipe open, and the hook must not wait with no limit.
#[test]
fn the_hook_does_not_wait_for_a_client_that_keeps_the_pipe_open() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let mut child = machine
        .command(&repo, &["hook"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()?;
    let _open = child.stdin.take();
    let start = std::time::Instant::now();
    while child.try_wait()?.is_none() {
        assert!(start.elapsed().as_secs() < 15, "the hook still runs");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(())
}

/// A notice that did not reach the client must come again.
#[test]
fn a_failed_write_of_the_hook_keeps_the_notice_for_the_next_call() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let task = machine.json(&repo, &["task", "create", "One"])?;
    let id = text(&task, "task").to_owned();
    as_agent(&machine, &repo, "a", &["task", "claim", &id])?;
    let change = [
        "task",
        "update",
        &id,
        "--version",
        "1",
        "--description",
        "New.",
        "--reason",
        "A reason.",
        "--override",
    ];
    machine.json(&repo, &change)?;

    let input = format!("{{\"cwd\":{}}}", serde_json::to_string(&repo)?);
    let mut child = machine
        .command(&repo, &["hook"])
        .env("LENKA_AGENT", "a")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    drop(child.stdout.take());
    if let Some(mut stdin) = child.stdin.take() {
        std::io::Write::write_all(&mut stdin, input.as_bytes())?;
    }
    child.wait()?;
    let shown = run_hook(&machine, &repo, &input, Some("a"))?;
    assert!(shown.contains("A reason."), "{shown}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn hook_install_keeps_the_access_rights_of_the_settings() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let directory = repo.join(".claude");
    fs::create_dir_all(&directory)?;
    let file = directory.join("settings.local.json");
    fs::write(&file, "{\"env\":{}}\n")?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o640))?;
    machine.ok(&repo, &["hook", "install", "--yes"])?;
    assert_eq!(fs::metadata(&file)?.permissions().mode() & 0o777, 0o640);
    assert!(fs::read_to_string(&file)?.contains("PostToolUse"));

    fs::set_permissions(&file, fs::Permissions::from_mode(0o400))?;
    fs::write(directory.join("other"), "")?;
    let again = machine.run(&repo, &["hook", "install", "--yes"])?;
    assert!(!again.status.success());
    Ok(())
}

#[cfg(unix)]
#[test]
fn hook_install_refuses_a_symlink_in_its_path() -> TestResult {
    let (temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&elsewhere)?;
    std::os::unix::fs::symlink(&elsewhere, repo.join(".claude"))?;
    assert!(
        !machine
            .run(&repo, &["hook", "install", "--yes"])?
            .status
            .success()
    );
    assert!(!elsewhere.join("settings.local.json").exists());
    Ok(())
}

#[test]
fn hook_install_gives_json() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let value = machine.json(&repo, &["hook", "install", "--yes"])?;
    assert_eq!(value["changed"], true, "{value}");
    let value = machine.json(&repo, &["hook", "install", "--yes"])?;
    assert_eq!(value["changed"], false, "{value}");
    Ok(())
}
