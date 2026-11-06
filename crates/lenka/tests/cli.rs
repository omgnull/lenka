// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Tests of the command line with real processes.

mod common;

use std::fs;

use common::{Machine, TestResult, text};
use serde_json::Value;

fn machine() -> TestResult<(tempfile::TempDir, Machine)> {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    Ok((temp, machine))
}

#[test]
fn init_in_a_git_repo_creates_the_data_and_edits_no_file() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.repo("api")?;
    fs::write(repo.join("CLAUDE.md"), "keep\n")?;
    machine.git(&repo, &["add", "."])?;
    machine.git(&repo, &["commit", "-q", "-m", "docs"])?;

    let text_out = machine.ok(&repo, &["init"])?;
    assert!(text_out.contains("Created the project api"), "{text_out}");
    assert!(text_out.contains("CLAUDE.md or AGENTS.md"), "{text_out}");
    assert!(
        text_out.contains("claude mcp add lenka -- lenka mcp"),
        "{text_out}"
    );

    assert_eq!(fs::read_to_string(repo.join("CLAUDE.md"))?, "keep\n");
    assert_eq!(machine.git(&repo, &["status", "--porcelain"])?, "");
    assert!(repo.join(".git/lenka-data/lenka.toml").is_file());
    assert!(repo.join(".git/lenka-clone.toml").is_file());

    let list = machine.json(&repo, &["project", "list"])?;
    assert_eq!(list["projects"][0]["name"], "api");
    assert_eq!(list["projects"][0]["default"], true);

    // A repeat run keeps the same project.
    let id = list["projects"][0]["id"].clone();
    machine.ok(&repo, &["init"])?;
    assert_eq!(
        machine.json(&repo, &["project", "list"])?["projects"][0]["id"],
        id
    );
    Ok(())
}

#[test]
fn a_task_flows_through_the_command_line() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;

    let created = machine.json(
        &repo,
        &[
            "task",
            "create",
            "First task",
            "--description",
            "The text.",
            "--acceptance",
            "- [ ] It works.",
            "--label",
            "x",
            "--priority",
            "high",
        ],
    )?;
    let id = text(&created, "task").to_owned();
    assert_eq!(id.len(), 8);
    assert_eq!(created["version"], 1);

    let second = machine.json(
        &repo,
        &["task", "create", "Second task", "--blocked-by", &id],
    )?;
    let second_id = text(&second, "task").to_owned();

    let rows = machine.json(&repo, &["task", "list", "--ready"])?;
    assert_eq!(rows["count"], 1);
    assert_eq!(rows["tasks"][0]["untrusted_data"]["title"], "First task");
    let blocked = machine.json(&repo, &["task", "list", "--blocked"])?;
    assert_eq!(blocked["tasks"][0]["id"], second_id.as_str());
    let human = machine.ok(&repo, &["task", "list"])?;
    assert!(
        human.contains("First task") && human.contains("Second task"),
        "{human}"
    );

    let related = machine.json(&repo, &["task", "related", &second_id])?;
    assert_eq!(related["blockers"][0]["id"], id.as_str());

    let claim = machine.json(&repo, &["task", "claim"])?;
    assert_eq!(text(&claim, "task"), id);
    let handle = text(&claim, "claim").to_owned();
    assert!(!handle.is_empty());
    let again = machine.json(
        &repo,
        &["task", "claim", &id, "--claim", &handle, "--minutes", "5"],
    )?;
    assert_eq!(text(&again, "claim"), handle);

    let entry = machine.json(
        &repo,
        &[
            "task",
            "entry",
            &id,
            "--kind",
            "decision",
            "--text",
            "- Use the short form.",
        ],
    )?;
    let entry_id = text(&entry, "entry").to_owned();
    let replaced = machine.json(
        &repo,
        &[
            "task",
            "entry",
            &id,
            "--kind",
            "decision",
            "--text",
            "A new one.",
            "--replaces",
            &entry_id,
        ],
    )?;
    assert_ne!(text(&replaced, "entry"), entry_id);

    let shown = machine.json(&repo, &["task", "show", &id, "--include", "entries,plan"])?;
    assert_eq!(shown["untrusted_data"]["title"], "First task");
    assert_eq!(
        shown["untrusted_data"]["entries"].as_array().map(Vec::len),
        Some(2)
    );
    let human = machine.ok(&repo, &["task", "show", &id])?;
    assert!(human.contains("The text."), "{human}");

    // An unchecked criterion stops the finish.
    assert_eq!(
        machine.fails(
            &repo,
            &["task", "finish", &id, "--evidence", "cargo test: ok"]
        )?,
        "invalid_argument"
    );
    let version = shown["task"]["version"].to_string();
    machine.json(
        &repo,
        &["task", "update", &id, "--version", &version, "--check", "0"],
    )?;
    assert_eq!(
        machine.fails(
            &repo,
            &["task", "update", &id, "--version", "99", "--title", "X"]
        )?,
        "stale_version"
    );
    let done = machine.json(
        &repo,
        &["task", "finish", &id, "--evidence", "cargo test: ok"],
    )?;
    assert_eq!(done["status"], "done");

    let ready = machine.json(&repo, &["task", "list", "--ready"])?;
    assert_eq!(ready["tasks"][0]["id"], second_id.as_str());

    machine.json(&repo, &["task", "delete", &second_id])?;
    assert_eq!(
        machine.fails(&repo, &["task", "show", &second_id])?,
        "not_found"
    );

    let pause_task = machine.json(&repo, &["task", "create", "Third"])?;
    let pause_id = text(&pause_task, "task");
    machine.json(&repo, &["task", "claim", pause_id])?;
    let paused = machine.json(&repo, &["task", "pause", pause_id])?;
    assert_eq!(paused["status"], "paused");

    // The reindex and the doctor see the same data.
    assert_eq!(machine.json(&repo, &["reindex"])?["tasks"], 2);
    assert_eq!(machine.json(&repo, &["doctor"])?["clean"], true);
    Ok(())
}

#[test]
fn only_a_person_option_overrides_the_secret_check() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let secret = format!("ghp_{}", "a1b2c3d4".repeat(5));
    let description = format!("The token is {secret}");

    let output = machine.run(
        &repo,
        &["task", "create", "Secret", "--description", &description],
    )?;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("secret_detected"), "{stderr}");
    assert!(
        !stderr.contains(&secret),
        "the error must hide the value: {stderr}"
    );

    let created = machine.json(
        &repo,
        &[
            "task",
            "create",
            "Secret",
            "--description",
            &description,
            "--allow-secret",
        ],
    )?;
    let id = text(&created, "task");

    assert_eq!(
        machine.fails(
            &repo,
            &[
                "task",
                "entry",
                id,
                "--kind",
                "comment",
                "--text",
                &description
            ]
        )?,
        "secret_detected"
    );
    machine.json(
        &repo,
        &[
            "task",
            "entry",
            id,
            "--kind",
            "comment",
            "--text",
            &description,
            "--allow-secret",
        ],
    )?;
    assert_eq!(
        machine.fails(
            &repo,
            &[
                "task",
                "update",
                id,
                "--version",
                "1",
                "--plan",
                &description,
                "--reason",
                "r"
            ]
        )?,
        "secret_detected"
    );
    machine.json(
        &repo,
        &[
            "task",
            "update",
            id,
            "--version",
            "1",
            "--plan",
            &description,
            "--reason",
            "r",
            "--allow-secret",
        ],
    )?;
    // The MCP form has no such parameter, so a tool call cannot pass it.
    assert_eq!(
        machine.fails(&repo, &["memory", "add", &description])?,
        "secret_detected"
    );
    Ok(())
}

#[test]
fn person_options_change_type_coordinator_status_and_claims() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let created = machine.json(&repo, &["task", "create", "A task"])?;
    let id = text(&created, "task");

    let updated = machine.json(
        &repo,
        &[
            "task",
            "update",
            id,
            "--version",
            "1",
            "--type",
            "bug",
            "--reason",
            "This is a bug.",
            "--coordinated",
            "true",
        ],
    )?;
    let version = updated["version"].to_string();
    let shown = machine.json(&repo, &["task", "show", id])?;
    assert_eq!(shown["task"]["type"], "bug");
    let front = fs::read_to_string(repo.join(format!(
        ".git/lenka-data/tasks/{}/task.md",
        text(&shown["task"], "full_id")
    )))?;
    assert!(front.contains("coordinated = true"), "{front}");
    assert_eq!(
        machine.fails(
            &repo,
            &[
                "task",
                "update",
                id,
                "--version",
                "99",
                "--coordinated",
                "false"
            ]
        )?,
        "stale_version"
    );
    machine.json(
        &repo,
        &[
            "task",
            "update",
            id,
            "--version",
            &version,
            "--coordinated",
            "false",
        ],
    )?;

    let parked = machine.json(&repo, &["task", "status", id, "backlog"])?;
    assert_eq!(parked["status"], "backlog");
    assert_eq!(
        machine.fails(&repo, &["task", "status", id, "done"])?,
        "invalid_argument"
    );
    machine.json(&repo, &["task", "status", id, "todo"])?;

    // Another worktree identity holds the claim, and a person overrides it.
    let output = machine.with_input(
        &repo,
        &["--json", "task", "claim", id],
        "",
        &[("LENKA_AGENT", "other")],
    )?;
    assert!(output.status.success());
    assert_eq!(
        machine.fails(&repo, &["task", "status", id, "backlog"])?,
        "claim_lost"
    );
    machine.json(&repo, &["task", "status", id, "backlog", "--override"])?;
    machine.json(&repo, &["task", "status", id, "todo"])?;

    let output = machine.with_input(
        &repo,
        &["--json", "task", "claim", id],
        "",
        &[("LENKA_AGENT", "other")],
    )?;
    assert!(output.status.success());
    let done = machine.json(
        &repo,
        &["task", "finish", id, "--evidence", "ok", "--override"],
    )?;
    assert_eq!(done["status"], "done");
    Ok(())
}

#[test]
fn a_person_advances_with_the_override_and_goes_back() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let settings = repo.join(".git/lenka-data/lenka.toml");
    let text_old = fs::read_to_string(&settings)?;
    fs::write(
        &settings,
        format!(
            "{text_old}\n[types.flow]\nstages = [{{name = \"one\", actor = \"any\"}}, {{name = \"two\", actor = \"any\"}}]\n"
        ),
    )?;
    let created = machine.json(&repo, &["task", "create", "A task", "--type", "flow"])?;
    let id = text(&created, "task");

    let output = machine.with_input(
        &repo,
        &["--json", "task", "claim", id],
        "",
        &[("LENKA_AGENT", "other")],
    )?;
    assert!(output.status.success());
    assert_eq!(
        machine.fails(&repo, &["task", "advance", id, "--evidence", "ok"])?,
        "claim_lost"
    );
    let moved = machine.json(
        &repo,
        &["task", "advance", id, "--evidence", "ok", "--override"],
    )?;
    assert_eq!(moved["stage"], "two");

    machine.json(&repo, &["task", "claim", id])?;
    let back = machine.json(
        &repo,
        &[
            "task",
            "advance",
            id,
            "--evidence",
            "A test failed.",
            "--back-to",
            "one",
        ],
    )?;
    assert_eq!(back["stage"], "one");
    Ok(())
}

#[test]
fn plugins_show_the_state_and_the_trust_label() -> TestResult {
    let (_temp, machine) = machine()?;
    let dir = machine.root.clone();
    let human = machine.ok(&dir, &["plugins"])?;
    for line in human.lines().filter(|line| !line.starts_with("The file")) {
        assert!(line.contains("fully trusted, no sandbox"), "{line}");
    }
    let rows = machine.json(&dir, &["plugins"])?;
    let rows = rows["plugins"].as_array().cloned().unwrap_or_default();
    let names: Vec<&str> = rows.iter().map(|row| text(row, "name")).collect();
    for name in [
        "driver-local",
        "artifact-fs",
        "tasks",
        "artifacts",
        "memory",
    ] {
        assert!(names.contains(&name), "{names:?}");
    }
    assert!(
        rows.iter()
            .all(|row| row["trust"] == "fully trusted, no sandbox")
    );

    fs::write(
        machine.root.join("config/plugins.toml"),
        "disabled = [\"memory\"]\n",
    )?;
    let rows = machine.json(&dir, &["plugins"])?;
    let state = rows["plugins"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["name"] == "memory"))
        .map(|row| text(row, "state").to_owned());
    assert_eq!(state.as_deref(), Some("off"));
    let repo = machine.project("api")?;
    assert_eq!(
        machine.fails(&repo, &["memory", "search", "x"])?,
        "not_found"
    );
    Ok(())
}

#[test]
fn artifacts_memory_and_call_work() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    fs::write(repo.join("design.txt"), "one")?;
    let created = machine.json(&repo, &["artifact", "create", "design.txt"])?;
    let id = text(&created, "artifact").to_owned();
    assert_eq!(created["version"], 1);
    fs::write(repo.join("design.txt"), "two")?;
    let updated = machine.json(
        &repo,
        &[
            "artifact",
            "update",
            &id,
            "design.txt",
            "--version",
            "1",
            "--reason",
            "A new design.",
        ],
    )?;
    assert_eq!(updated["version"], 2);
    let rows = machine.json(&repo, &["artifact", "list"])?;
    assert_eq!(rows["artifacts"][0]["untrusted_data"]["name"], "design.txt");
    assert_eq!(rows["artifacts"][0]["version"], 2);

    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let task_id = text(&task, "task");
    machine.json(
        &repo,
        &[
            "task",
            "update",
            task_id,
            "--version",
            "1",
            "--artifact-add",
            &id,
        ],
    )?;
    assert_eq!(
        machine.fails(&repo, &["artifact", "delete", &id])?,
        "delete_refused"
    );
    machine.json(
        &repo,
        &[
            "task",
            "update",
            task_id,
            "--version",
            "2",
            "--artifact-remove",
            &id,
        ],
    )?;
    machine.json(&repo, &["artifact", "delete", &id])?;

    let added = machine.json(
        &repo,
        &[
            "memory",
            "add",
            "The port is 8080.",
            "--tag",
            "ops",
            "--task",
            task_id,
        ],
    )?;
    let memory = text(&added, "memory").to_owned();
    let found = machine.json(&repo, &["memory", "search", "port"])?;
    assert_eq!(
        found["memories"][0]["untrusted_data"]["text"],
        "The port is 8080."
    );
    assert_eq!(
        found["memories"][0]["untrusted_data"]["author"],
        "human:tester"
    );
    let human = machine.ok(&repo, &["memory", "search", "--tag", "ops"])?;
    assert!(human.contains("The port is 8080."), "{human}");
    machine.json(&repo, &["memory", "delete", &memory])?;
    assert_eq!(machine.json(&repo, &["memory", "search"])?["count"], 0);

    let projects = machine.json(&repo, &["call", "project.list"])?;
    assert_eq!(projects["projects"][0]["name"], "api");
    let listed = machine.json(&repo, &["call", "task.list", r#"{"limit": 1}"#])?;
    assert_eq!(listed["count"], 1);
    assert_eq!(machine.fails(&repo, &["call", "no.tool"])?, "not_found");
    Ok(())
}

#[test]
fn the_project_comes_from_the_option_then_the_environment_then_the_clone() -> TestResult {
    let (_temp, machine) = machine()?;
    let first = machine.project("first")?;
    let second = machine.project("second")?;
    machine.json(&first, &["task", "create", "In first"])?;
    machine.json(&second, &["task", "create", "In second"])?;

    let title = |value: &Value| {
        value["tasks"][0]["untrusted_data"]["title"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    };
    assert_eq!(title(&machine.json(&first, &["task", "list"])?), "In first");
    // A person can use all projects of the user with --project.
    assert_eq!(
        title(&machine.json(&first, &["--project", "second", "task", "list"])?),
        "In second"
    );
    let output = machine.with_input(
        &first,
        &["--json", "task", "list"],
        "",
        &[("LENKA_PROJECT", "second")],
    )?;
    assert_eq!(title(&serde_json::from_slice(&output.stdout)?), "In second");
    let output = machine.with_input(
        &first,
        &["--json", "--project", "first", "task", "list"],
        "",
        &[("LENKA_PROJECT", "second")],
    )?;
    assert_eq!(title(&serde_json::from_slice(&output.stdout)?), "In first");
    assert_eq!(
        machine.fails(&first, &["--project", "absent", "task", "list"])?,
        "project_not_available"
    );
    Ok(())
}

#[test]
fn a_shared_project_attaches_to_a_second_repository() -> TestResult {
    let (_temp, machine) = machine()?;
    let first = machine.repo("first")?;
    let second = machine.repo("second")?;
    machine.ok(&first, &["init", "--shared", "--name", "team"])?;
    assert!(machine.root.join("data/projects/team/lenka.toml").is_file());
    machine.json(&first, &["task", "create", "Shared task"])?;

    assert_eq!(
        machine.fails(&second, &["project", "attach", "absent"])?,
        "project_not_available"
    );
    machine.json(&second, &["project", "attach", "team"])?;
    let rows = machine.json(&second, &["task", "list"])?;
    assert_eq!(rows["tasks"][0]["untrusted_data"]["title"], "Shared task");
    Ok(())
}

#[test]
fn init_outside_git_with_a_directory_works() -> TestResult {
    let (_temp, machine) = machine()?;
    let work = machine.root.join("plain");
    fs::create_dir_all(&work)?;
    machine.ok(&work, &["init", "--dir", "store", "--name", "plain"])?;
    assert!(work.join("store/lenka.toml").is_file());
    assert!(work.join(".lenka-clone.toml").is_file());
    assert_eq!(machine.json(&work, &["task", "list"])?["count"], 0);
    Ok(())
}

#[test]
fn migrate_reports_no_change_and_refuses_a_newer_format() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let human = machine.ok(&repo, &["migrate"])?;
    assert!(human.contains("No change"), "{human}");
    assert_eq!(machine.json(&repo, &["migrate"])?["changed"], false);

    let settings = repo.join(".git/lenka-data/lenka.toml");
    let text_old = fs::read_to_string(&settings)?;
    fs::write(
        &settings,
        text_old.replace("format_version = 1", "format_version = 99"),
    )?;
    assert_eq!(machine.fails(&repo, &["migrate"])?, "format_too_new");
    Ok(())
}

#[test]
fn doctor_reports_invalid_files_and_pointer_files() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    assert_eq!(machine.json(&repo, &["doctor"])?["clean"], true);
    assert!(machine.ok(&repo, &["doctor"])?.contains("no problems"));

    fs::write(repo.join("design.bin"), "bytes")?;
    let created = machine.json(&repo, &["artifact", "create", "design.bin"])?;
    let file = repo.join(format!(
        ".git/lenka-data/artifacts/{}/versions/000001/design.bin",
        text(&created, "full_id")
    ));
    assert!(file.is_file(), "{}", file.display());
    fs::write(
        &file,
        "version https://git-lfs.github.com/spec/v1\noid sha256:00\nsize 5\n",
    )?;
    let created_task = machine.json(&repo, &["task", "create", "A task"])?;
    let task_file = repo.join(format!(
        ".git/lenka-data/tasks/{}/task.md",
        text(&created_task, "full_id")
    ));
    fs::write(&task_file, "<<<<<<< HEAD\nbad\n")?;

    let output = machine.run(&repo, &["--json", "doctor"])?;
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["clean"], false);
    let missing = report["lfs_not_downloaded"].to_string();
    assert!(missing.contains("design.bin"), "{missing}");
    assert_eq!(
        report["report"]["invalid_files"].as_array().map(Vec::len),
        Some(1)
    );
    let human = machine.run(&repo, &["doctor"])?;
    let human = String::from_utf8_lossy(&human.stdout).into_owned();
    assert!(human.contains("Git LFS pointer"), "{human}");
    Ok(())
}

#[test]
fn logs_hold_no_task_text() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let mut command = machine.command(
        &repo,
        &[
            "task",
            "create",
            "A hidden title",
            "--description",
            "A hidden text.",
        ],
    );
    let output = command.env("LENKA_LOG", "trace").output()?;
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("hidden"), "{stderr}");
    Ok(())
}

#[test]
fn an_artifact_cannot_hide_a_secret_or_poison_the_list() -> TestResult {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let repo = machine.project("api")?;
    let token = format!("ghp_{}", "a1b2c3d4".repeat(5));
    let path = |name: &str| repo.join(name).to_string_lossy().into_owned();

    // A secret after an invalid byte, and a secret in a large file.
    fs::write(
        repo.join("bytes.txt"),
        [b"a \xff ".as_slice(), token.as_bytes()].concat(),
    )?;
    let large = [
        vec![b'x'; 3 * 1024 * 1024],
        b" ".to_vec(),
        token.clone().into_bytes(),
    ]
    .concat();
    fs::write(repo.join("large.txt"), large)?;
    fs::write(repo.join("good.txt"), "plain\n")?;
    for name in ["bytes.txt", "large.txt"] {
        let code = machine.fails(&repo, &["artifact", "create", &path(name)])?;
        assert_eq!(code, "secret_detected", "{name}");
    }

    // A secret in a name, and a name with a path.
    let good = path("good.txt");
    let code = machine.fails(&repo, &["artifact", "create", &good, "--name", &token])?;
    assert_eq!(code, "secret_detected");
    let code = machine.fails(&repo, &["artifact", "create", &good, "--name", "a/b"])?;
    assert_eq!(code, "invalid_argument");
    machine.json(&repo, &["artifact", "create", &good])?;

    // One bad record from a merge does not hide the other artifacts.
    let id = "019a0000-0000-7000-8000-000000000000";
    let bad = repo.join(".git/lenka-data/artifacts").join(id);
    fs::create_dir_all(&bad)?;
    fs::write(bad.join("artifact.toml"), "versions = 3\n")?;
    let rows = machine.json(&repo, &["artifact", "list"])?;
    assert_eq!(rows["artifacts"].as_array().map(Vec::len), Some(1));
    Ok(())
}

#[cfg(unix)]
#[test]
fn an_artifact_write_never_goes_through_a_symlink() -> TestResult {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let repo = machine.project("api")?;
    let victim = temp.path().join("victim.txt");
    fs::write(&victim, "original")?;
    fs::write(repo.join("f.txt"), "one\n")?;
    let file = repo.join("f.txt").to_string_lossy().into_owned();
    let created = machine.json(&repo, &["artifact", "create", &file])?;
    let full = text(&created, "full_id").to_owned();
    let next = repo
        .join(".git/lenka-data/artifacts")
        .join(&full)
        .join("versions/000002");
    fs::create_dir_all(&next)?;
    std::os::unix::fs::symlink(&victim, next.join("f.txt"))?;
    fs::write(repo.join("f.txt"), "bytes from an agent\n")?;
    let short = text(&created, "artifact").to_owned();
    let _ = machine.run(
        &repo,
        &["artifact", "update", &short, "1", &file, "--reason", "r"],
    )?;
    assert_eq!(fs::read_to_string(&victim)?, "original");
    Ok(())
}

#[test]
fn an_artifact_has_limits_on_its_type_and_its_reason() -> TestResult {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let repo = machine.project("api")?;
    fs::write(repo.join("a.txt"), "first")?;
    let bad_type = "text/plain\u{1b}]0;x\u{7}";
    let code = machine.fails(
        &repo,
        &["artifact", "create", "a.txt", "--media-type", bad_type],
    )?;
    assert_eq!(code, "invalid_argument");
    let long_type = "t".repeat(257);
    let code = machine.fails(
        &repo,
        &["artifact", "create", "a.txt", "--media-type", &long_type],
    )?;
    assert_eq!(code, "invalid_argument");
    let created = machine.json(&repo, &["artifact", "create", "a.txt"])?;
    let id = text(&created, "artifact").to_owned();
    fs::write(repo.join("b.txt"), "second")?;
    let reason = "r".repeat(4097);
    let code = machine.fails(
        &repo,
        &[
            "artifact",
            "update",
            &id,
            "b.txt",
            "--version",
            "1",
            "--reason",
            &reason,
        ],
    )?;
    assert_eq!(code, "invalid_argument");
    Ok(())
}

#[test]
fn an_artifact_with_an_invalid_record_can_be_deleted() -> TestResult {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let repo = machine.project("api")?;
    fs::write(repo.join("a.txt"), "first")?;
    let created = machine.json(&repo, &["artifact", "create", "a.txt"])?;
    let full = text(&created, "full_id").to_owned();
    let record = repo
        .join(".git/lenka-data/artifacts")
        .join(&full)
        .join("artifact.toml");
    fs::write(&record, "<<<<<<< HEAD\nnot valid\n")?;
    machine.json(&repo, &["artifact", "delete", &full])?;
    assert!(!record.exists());
    Ok(())
}

#[test]
fn the_guidance_says_that_a_name_selects_one_inbox() -> TestResult {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let repo = machine.repo("api")?;
    let output = machine.ok(&repo, &["init"])?;
    assert!(output.contains("the same LENKA_AGENT"), "{output}");
    Ok(())
}

#[test]
fn a_person_lets_an_agent_continue_a_loop_with_one_option() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let created = machine.json(&repo, &["task", "create", "A task"])?;
    let id = text(&created, "task");
    let args = [
        "task",
        "update",
        id,
        "--version",
        "1",
        "--loop-until-done",
        "true",
    ];
    machine.json(&repo, &args)?;
    let shown = machine.json(&repo, &["task", "show", id])?;
    assert_eq!(shown["task"]["loop_until_done"], true);
    let printed = machine.ok(&repo, &["task", "show", id])?;
    assert!(printed.contains("until the task is done"), "{printed}");
    Ok(())
}

/// Text from a hostile author. Each part can change a terminal or hide text.
const HOSTILE: &str = "start\u{1b}]0;owned\u{7}\u{1b}[2J\u{9b}31m\u{202e}\u{d}end";

fn assert_no_terminal_control(what: &str, text: &str) {
    for bad in ['\u{1b}', '\u{7}', '\u{9b}', '\u{202e}', '\u{d}'] {
        assert!(!text.contains(bad), "{what} has {bad:?}: {text:?}");
    }
}

#[test]
fn hostile_text_from_a_file_cannot_control_a_terminal() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let data = repo.join(".git/lenka-data");
    let created = machine.json(
        &repo,
        &["task", "create", "A title", "--description", "Plain."],
    )?;
    let id = text(&created, "task").to_owned();
    machine.json(
        &repo,
        &[
            "task",
            "entry",
            &id,
            "--kind",
            "comment",
            "--text",
            "An entry.",
        ],
    )?;
    let added = machine.json(&repo, &["memory", "add", "A memory."])?;
    let full_memory = text(&added, "full_id").to_owned();
    let task_dir = fs::read_dir(data.join("tasks"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.to_string_lossy().contains(&id))
        .ok_or("the task directory exists")?;

    // A person, or a merge, can write any bytes to these files.
    let toml_text = HOSTILE
        .chars()
        .map(|item| {
            if item.is_control() || item == '\u{202e}' {
                format!("\\u{:04X}", u32::from(item))
            } else {
                item.to_string()
            }
        })
        .collect::<String>();
    for (file, from, to) in [
        (task_dir.join("task.md"), "A title", toml_text.as_str()),
        (task_dir.join("task.md"), "Plain.", HOSTILE),
        (
            data.join(format!("memory/{full_memory}.md")),
            "A memory.",
            HOSTILE,
        ),
    ] {
        let old = fs::read_to_string(&file)?;
        assert!(old.contains(from), "{old}");
        fs::write(&file, old.replace(from, to))?;
    }

    for args in [
        vec!["task", "list"],
        vec!["task", "show", &id],
        vec!["task", "show", &id, "--include", "entries,versions"],
        vec!["memory", "search"],
    ] {
        let output = machine.run(&repo, &args)?;
        assert!(output.status.success(), "{args:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("start"), "{args:?}: {stdout}");
        assert_no_terminal_control(&format!("{args:?}"), &stdout);
        assert_no_terminal_control(
            &format!("{args:?}"),
            &String::from_utf8_lossy(&output.stderr),
        );

        // JSON keeps the value, and the output has no raw control bytes.
        let mut with_json = vec!["--json"];
        with_json.extend_from_slice(&args);
        let output = machine.run(&repo, &with_json)?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_no_terminal_control(&format!("{with_json:?}"), &stdout);
        let parsed: Value = serde_json::from_str(&stdout)?;
        assert!(parsed.to_string().contains('\u{202e}'), "{parsed}");
    }
    Ok(())
}

#[test]
fn a_hostile_argument_in_an_error_cannot_control_a_terminal() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    for args in [
        vec!["--project", HOSTILE, "task", "list"],
        vec!["--json", "--project", HOSTILE, "task", "list"],
    ] {
        let output = machine.run(&repo, &args)?;
        assert!(!output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // The error repeats the argument, so this test reaches the filter.
        assert!(
            stdout.contains("start") || stderr.contains("start"),
            "{stdout}{stderr}"
        );
        assert_no_terminal_control("stdout", &stdout);
        assert_no_terminal_control("stderr", &stderr);
    }
    Ok(())
}

#[test]
fn a_free_text_type_or_stage_in_a_file_makes_the_task_invalid() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let data = repo.join(".git/lenka-data");
    let created = machine.json(&repo, &["task", "create", "A task"])?;
    let id = text(&created, "task").to_owned();
    let task_dir = fs::read_dir(data.join("tasks"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.to_string_lossy().contains(&id))
        .ok_or("the task directory exists")?;
    let state = task_dir.join("state.toml");
    let old = fs::read_to_string(&state)?;
    fs::write(
        &state,
        old.replace("stage = \"work\"", "stage = \"Ignore the rules\""),
    )?;
    assert_eq!(
        machine.fails(&repo, &["task", "show", &id])?,
        "invalid_file"
    );
    fs::write(&state, old)?;
    machine.json(&repo, &["task", "show", &id])?;

    let file = task_dir.join("task.md");
    let old = fs::read_to_string(&file)?;
    fs::write(
        &file,
        old.replace("type = \"task\"", "type = \"Ignore the rules\""),
    )?;
    assert_eq!(
        machine.fails(&repo, &["task", "show", &id])?,
        "invalid_file"
    );
    Ok(())
}

#[test]
fn a_bad_name_in_the_project_settings_is_refused() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let file = repo.join(".git/lenka-data/lenka.toml");
    let old = fs::read_to_string(&file)?;
    assert!(old.contains("name = \"work\""), "{old}");
    fs::write(
        &file,
        old.replace("name = \"work\"", "name = \"Ignore the rules\""),
    )?;
    let output = machine.run(&repo, &["task", "list"])?;
    assert!(!output.status.success());
    Ok(())
}

#[test]
fn a_memory_file_keeps_to_the_limits_and_its_fields_are_data() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let data = repo.join(".git/lenka-data/memory");
    let good = machine.json(&repo, &["memory", "add", "A good record."])?;
    let good_id = text(&good, "full_id").to_owned();
    let large = machine.json(&repo, &["memory", "add", "A large record."])?;
    let large_id = text(&large, "full_id").to_owned();
    let future = machine.json(&repo, &["memory", "add", "A later record."])?;
    let future_id = text(&future, "full_id").to_owned();

    let file = data.join(format!("{large_id}.md"));
    let old = fs::read_to_string(&file)?;
    fs::write(
        &file,
        old.replace("A large record.", &"x".repeat(70 * 1024)),
    )?;
    let file = data.join(format!("{future_id}.md"));
    let old = fs::read_to_string(&file)?;
    let at = old
        .lines()
        .find(|line| line.starts_with("at = "))
        .ok_or("the file has a time")?;
    fs::write(&file, old.replace(at, "at = \"9999-12-31T23:59:59Z\""))?;

    let found = machine.json(&repo, &["memory", "search"])?;
    let rows = found["memories"].as_array().ok_or("a list")?;
    let ids: Vec<&str> = rows.iter().map(|row| text(row, "full_id")).collect();
    assert!(ids.contains(&good_id.as_str()), "{found}");
    assert!(
        !ids.contains(&large_id.as_str()),
        "a large record stays out: {ids:?}"
    );
    // The newest ID comes first. A time in a file cannot move a record up.
    let order = machine.json(&repo, &["memory", "add", "The newest record."])?;
    let found = machine.json(&repo, &["memory", "search"])?;
    assert_eq!(found["memories"][0]["full_id"], order["full_id"], "{found}");
    assert!(found["memories"][0].get("author").is_none(), "{found}");
    assert!(found["memories"][0].get("tags").is_none(), "{found}");
    assert_eq!(
        found["memories"][0]["untrusted_data"]["text"],
        "The newest record."
    );
    Ok(())
}
