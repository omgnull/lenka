// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Tests of MCP with a real `rmcp` client and the built binary, in both protocol eras.

#[expect(dead_code, reason = "this test binary uses only a part of the helpers")]
mod common;
#[path = "common/mcp.rs"]
mod mcp_client;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Stdio;

use common::{Machine, TestResult, text};
use mcp_client::{Era, call_error, call_ok, connect, params};
use rmcp::ServiceError;
use rmcp::model::ProtocolVersion;
use serde_json::{Value, json};

/// The limit of section 8.2 of the design, as an approximate count.
const TOKEN_LIMIT: usize = 3000;

fn machine() -> TestResult<(tempfile::TempDir, Machine)> {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    Ok((temp, machine))
}

/// The same work runs in each era: tools, the read-only hint, a task flow, and errors.
async fn work_in_era(era: Era, expected: ProtocolVersion) -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let client = connect(&machine, &repo, None, era).await?;
    let info = client.peer_info().map(|info| info.protocol_version.clone());
    assert_eq!(info, Some(expected));

    let tools = client.list_all_tools().await?;
    let find = |name: &str| tools.iter().find(|tool| tool.name == name);
    let hint = |name: &str| {
        find(name)
            .and_then(|tool| tool.annotations.clone())
            .and_then(|item| item.read_only_hint)
    };
    assert_eq!(hint("task.list"), Some(true));
    assert_eq!(hint("task.create"), Some(false));
    assert_eq!(hint("memory.search"), Some(true));
    assert!(find("artifact.update").is_some());
    assert!(find("memory.delete").is_some());
    assert!(find("project.list").is_some());

    let created = call_ok(
        &client,
        "task.create",
        json!({"title": "From MCP", "description": "The text."}),
    )
    .await?;
    let id = text(&created, "task").to_owned();
    let rows = call_ok(&client, "task.list", json!({"ready": true})).await?;
    assert_eq!(rows["tasks"][0]["untrusted_data"]["title"], "From MCP");
    let shown = call_ok(&client, "task.show", json!({"task": id})).await?;
    assert_eq!(shown["untrusted_data"]["author"], "agent:test-client");
    let claim = call_ok(&client, "task.claim", json!({})).await?;
    assert_eq!(text(&claim, "task"), id);
    let done = call_ok(
        &client,
        "task.finish",
        json!({"task": id, "outcome": "done", "evidence": "ok"}),
    )
    .await?;
    assert_eq!(done["status"], "done");

    // A tool error is a result, with the code and the message as JSON.
    let error = call_error(&client, "task.show", json!({"task": "ffffffff"})).await?;
    assert_eq!(error["code"], "not_found");
    assert!(!text(&error, "message").is_empty());
    let secret = format!("ghp_{}", "a1b2c3d4".repeat(5));
    let error = call_error(
        &client,
        "task.create",
        json!({"title": "x", "description": secret}),
    )
    .await?;
    assert_eq!(error["code"], "secret_detected");
    assert!(!error.to_string().contains(&secret));

    // An unknown tool is a JSON-RPC error.
    let unknown = client.call_tool(params("no.such.tool", &json!({}))).await;
    assert!(
        matches!(unknown, Err(ServiceError::McpError(_))),
        "{unknown:?}"
    );

    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn the_initialize_era_works() -> TestResult {
    work_in_era(Era::Initialize, ProtocolVersion::V_2025_11_25).await
}

#[tokio::test]
async fn the_discover_era_works() -> TestResult {
    work_in_era(Era::Discover, ProtocolVersion::V_2026_07_28).await
}

#[tokio::test]
async fn the_agent_name_is_the_label_and_an_agent_has_no_person_rights() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let second = machine.project("second")?;
    let client = connect(&machine, &repo, Some("builder"), Era::Discover).await?;

    let created = call_ok(&client, "task.create", json!({"title": "A task"})).await?;
    let shown = call_ok(
        &client,
        "task.show",
        json!({"task": text(&created, "task")}),
    )
    .await?;
    assert_eq!(shown["untrusted_data"]["author"], "agent:builder");

    // An agent can use only the projects of its clone.
    let projects = call_ok(&client, "project.list", json!({})).await?;
    assert_eq!(projects["projects"].as_array().map(Vec::len), Some(1));
    let error = call_error(&client, "task.list", json!({"project": "second"})).await?;
    assert_eq!(error["code"], "project_not_available");
    assert!(second.is_dir());

    // The list of tools has no form of a secret override.
    let tools = client.list_all_tools().await?;
    let text = serde_json::to_string(&tools)?;
    assert!(!text.contains("allow_secret"), "{text}");
    assert!(!text.contains("override"), "{text}");
    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn no_project_loads_until_a_tool_call_needs_it() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let settings = repo.join(".git/lenka-data/lenka.toml");
    fs::write(&settings, "this is not TOML = = =\n")?;

    let client = connect(&machine, &repo, None, Era::Initialize).await?;
    assert!(!client.list_all_tools().await?.is_empty());
    let error = call_error(&client, "task.list", json!({})).await?;
    assert_eq!(error["code"], "invalid_file");
    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn a_process_outside_a_clone_has_no_projects() -> TestResult {
    let (_temp, machine) = machine()?;
    machine.project("api")?;
    let outside = machine.root.join("outside");
    fs::create_dir_all(&outside)?;
    let client = connect(&machine, &outside, None, Era::Initialize).await?;
    let error = call_error(&client, "task.list", json!({})).await?;
    assert_eq!(error["code"], "project_not_available");
    assert!(text(&error, "message").contains("lenka init"), "{error}");
    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn the_tool_definitions_stay_within_the_token_limit() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let client = connect(&machine, &repo, None, Era::Discover).await?;
    let tools = client.list_all_tools().await?;
    let tokens = serde_json::to_string(&tools)?.len() / 4;
    println!(
        "tool definitions: {} tools, about {tokens} tokens",
        tools.len()
    );
    assert!(
        tokens <= TOKEN_LIMIT,
        "the tool definitions use about {tokens} tokens"
    );
    client.cancel().await?;
    Ok(())
}

/// Sends raw JSON-RPC lines, and checks that each output line is protocol data.
fn raw_session(machine: &Machine, dir: &Path, lines: &[Value]) -> TestResult<Vec<Value>> {
    let mut command = machine.command(dir, &["mcp"]);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let mut answers = Vec::new();
    {
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            panic!("the pipes exist");
        };
        let mut reader = BufReader::new(stdout);
        for line in lines {
            writeln!(stdin, "{line}")?;
            stdin.flush()?;
            if line.get("id").is_some() {
                let mut answer = String::new();
                reader.read_line(&mut answer)?;
                answers.push(serde_json::from_str(&answer)?);
            }
        }
    }
    let output = child.wait_with_output()?;
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    Ok(answers)
}

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "raw", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {},
    })
}

#[test]
fn the_wire_shows_both_revisions_and_nothing_else() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;

    let answers = raw_session(
        &machine,
        &repo,
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {"_meta": modern_meta()}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": modern_meta()}}),
        ],
    )?;
    let versions = answers[0]["result"]["supportedVersions"].clone();
    assert_eq!(versions, json!(["2025-11-25", "2026-07-28"]));
    assert!(answers[0]["result"]["capabilities"]["tools"].is_object());
    assert_eq!(answers[1]["result"]["resultType"], "complete");
    assert!(answers[1]["result"]["tools"].is_array());

    let initialize = |version: &str| {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": {"name": "raw", "version": "1"},
            },
        })
    };
    let answers = raw_session(&machine, &repo, &[initialize("2025-11-25")])?;
    assert_eq!(answers[0]["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(answers[0]["result"]["serverInfo"]["name"], "lenka");
    // An older client gets the oldest revision of this server, and it can decide to leave.
    let answers = raw_session(&machine, &repo, &[initialize("2024-11-05")])?;
    assert_eq!(answers[0]["result"]["protocolVersion"], "2025-11-25");
    Ok(())
}

#[tokio::test]
async fn task_text_stays_in_untrusted_data_fields() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let client = connect(&machine, &repo, None, Era::Discover).await?;
    let marker = "MARKER text from an author";
    let blocker = call_ok(&client, "task.create", json!({"title": marker})).await?;
    let waiter = call_ok(
        &client,
        "task.create",
        json!({
            "title": marker, "labels": [marker],
            "blocked_by": [text(&blocker, "task")],
        }),
    )
    .await?;
    let id = text(&waiter, "task");
    let update = json!({"task": id, "version": 1, "description": "x", "reason": marker});
    call_ok(&client, "task.update", update).await?;

    // Removes all `untrusted_data` fields. The rest must not hold the text.
    fn trusted(value: &Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.iter()
                    .filter(|(key, _)| *key != "untrusted_data")
                    .map(|(key, item)| (key.clone(), trusted(item)))
                    .collect(),
            ),
            Value::Array(rows) => Value::Array(rows.iter().map(trusted).collect()),
            other => other.clone(),
        }
    }
    let results = [
        call_ok(&client, "task.list", json!({})).await?,
        call_ok(&client, "task.related", json!({"task": id})).await?,
        call_ok(
            &client,
            "task.show",
            json!({"task": id, "include": ["versions", "entries", "plan"]}),
        )
        .await?,
    ];
    for result in results {
        let full = serde_json::to_string(&result)?;
        assert!(full.contains(marker), "the test must see the text: {full}");
        let rest = serde_json::to_string(&trusted(&result))?;
        assert!(!rest.contains(marker), "{rest}");
    }
    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn a_person_in_the_directory_of_an_agent_is_another_identity() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let id = text(&task, "task").to_owned();
    let client = connect(&machine, &repo, None, Era::Initialize).await?;
    let claim = call_ok(&client, "task.claim", json!({"task": id})).await?;
    let handle = text(&claim, "claim").to_owned();

    // A person needs the override, because the agent holds the task.
    let update = [
        "task",
        "update",
        &id,
        "--version",
        "1",
        "--description",
        "Do not ship this.",
        "--reason",
        "The requirement changed.",
    ];
    let refused = machine.fails(&repo, &update)?;
    assert!(refused.contains("claim_lost"), "{refused}");
    let mut with_override = update.to_vec();
    with_override.push("--override");
    machine.json(&repo, &with_override)?;

    // The change reaches the agent, and a read by the person does not close the notice.
    machine.json(&repo, &["task", "show", &id])?;
    let finish = json!({"task": id, "claim": handle, "outcome": "done", "evidence": "It works."});
    let error = call_error(&client, "task.finish", finish).await?;
    assert_eq!(error["code"], "unread_changes");
    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn a_hand_edit_is_a_version_before_an_automatic_commit() -> TestResult {
    let (_temp, machine) = machine()?;
    fs::write(
        machine.root.join("config/plugins.toml"),
        "enabled = [\"git\"]\n",
    )?;
    let repo = machine.project("api")?;
    machine.ok(&repo, &["git", "enable"])?;
    let create = |title: &str| -> TestResult<String> {
        let args = ["task", "create", title, "--description", "Old text."];
        Ok(text(&machine.json(&repo, &args)?, "task").to_owned())
    };
    let edited = create("Edited")?;
    let done = create("Done")?;

    // A running server has the current stamps, so only a new check finds the edit.
    let client = connect(&machine, &repo, None, Era::Initialize).await?;
    call_ok(&client, "task.list", json!({})).await?;
    let data = repo.join(".git/lenka-data");
    let full = fs::read_dir(data.join("tasks"))?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| name.contains(&edited))
        .ok_or("the task directory exists")?;
    let file = data.join("tasks").join(&full).join("task.md");
    let old = fs::read_to_string(&file)?;
    fs::write(&file, old.replace("Old text.", "New text and more."))?;

    let args = json!({"task": done});
    let claim = call_ok(&client, "task.claim", args).await?;
    let args = json!({
        "task": done,
        "claim": text(&claim, "claim"),
        "outcome": "done",
        "evidence": "It works.",
    });
    call_ok(&client, "task.finish", args).await?;
    client.cancel().await?;

    let tree = machine.git(&data, &["ls-tree", "-r", "--name-only", "HEAD"])?;
    assert!(
        tree.contains(&format!("tasks/{full}/versions/000002/meta.toml")),
        "{tree}"
    );
    Ok(())
}

#[tokio::test]
async fn an_agent_sees_the_loop_stop_and_cannot_claim_the_task() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let created = machine.json(&repo, &["task", "create", "A task"])?;
    let id = text(&created, "task").to_owned();
    let client = connect(&machine, &repo, None, Era::Initialize).await?;
    let change = |version: u64| json!({"task": id, "version": version, "description": format!("Text {version}"), "reason": "A change."});
    for version in 1..=5 {
        call_ok(&client, "task.update", change(version)).await?;
    }
    let error = call_error(&client, "task.update", change(6)).await?;
    assert_eq!(error["code"], "loop_limit");
    let error = call_error(&client, "task.claim", json!({"task": id})).await?;
    assert_eq!(error["code"], "loop_limit");
    let shown = call_ok(&client, "task.show", json!({"task": id})).await?;
    assert_eq!(shown["task"]["loop_stopped"], true);

    // The person acts, and the same running server sees it.
    machine.json(&repo, &["task", "status", &id, "todo"])?;
    call_ok(&client, "task.claim", json!({"task": id})).await?;
    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn an_artifact_of_a_marked_task_has_no_loop_limit() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let file = repo.join("design.txt");
    fs::write(&file, "text 0")?;
    let path = file.to_string_lossy().into_owned();
    let client = connect(&machine, &repo, None, Era::Initialize).await?;
    let created = call_ok(&client, "artifact.create", json!({"path": path})).await?;
    let artifact = text(&created, "artifact").to_owned();
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let id = text(&task, "task").to_owned();
    let linked = json!({"task": id, "version": 1, "artifacts_add": [artifact]});
    let linked = call_ok(&client, "task.update", linked).await?;

    let update = |version: u64| -> TestResult<Value> {
        fs::write(&file, format!("text {version}"))?;
        Ok(json!({
            "artifact": artifact, "version": version, "path": path, "reason": "A change.",
        }))
    };
    for version in 1..=5 {
        call_ok(&client, "artifact.update", update(version)?).await?;
    }
    let error = call_error(&client, "artifact.update", update(6)?).await?;
    assert_eq!(error["code"], "loop_limit");

    let version = linked["version"].to_string();
    let mark = [
        "task",
        "update",
        &id,
        "--version",
        &version,
        "--loop-until-done",
        "true",
    ];
    machine.json(&repo, &mark)?;
    for version in 6..=8 {
        call_ok(&client, "artifact.update", update(version)?).await?;
    }
    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn text_from_files_stays_in_untrusted_data() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let file = repo.join("design.txt");
    fs::write(&file, "text")?;
    let path = file.to_string_lossy().into_owned();
    let client = connect(&machine, &repo, Some("worker"), Era::Initialize).await?;

    // A media type, and a name of an artifact.
    let args = json!({"path": path, "media_type": "text/plain"});
    let created = call_ok(&client, "artifact.create", args).await?;
    assert!(created.get("media_type").is_none(), "{created}");
    assert_eq!(created["untrusted_data"]["media_type"], "text/plain");

    // A notice in the replies of memory tools has the form of the task tools.
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let id = text(&task, "task").to_owned();
    call_ok(&client, "task.claim", json!({"task": id})).await?;
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
    let found = call_ok(&client, "memory.search", json!({})).await?;
    let row = &found["unread_changes"][0];
    assert_eq!(row["untrusted_data"]["reason"], "A reason.", "{found}");
    assert!(row.get("reason_untrusted").is_none(), "{found}");
    assert!(row.get("author").is_none(), "{found}");
    assert!(row.get("changed").is_none(), "{found}");
    assert_eq!(
        row["untrusted_data"]["changed"][0], "description",
        "{found}"
    );

    let shown = call_ok(&client, "task.show", json!({"task": id})).await?;
    let _ = shown;
    client.cancel().await?;

    // A holder name is data, so an agent does not get it in the text of an error.
    let client = connect(&machine, &repo, Some("other"), Era::Initialize).await?;
    let error = call_error(&client, "task.claim", json!({"task": id})).await?;
    assert_eq!(error["code"], "claim_lost");
    assert!(!text(&error, "message").contains("worker"), "{error}");
    client.cancel().await?;
    Ok(())
}

#[tokio::test]
async fn the_text_of_an_invalid_file_stays_in_untrusted_data() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let id = text(&task, "task").to_owned();
    let data = repo.join(".git/lenka-data");
    let dir = fs::read_dir(data.join("tasks"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.to_string_lossy().contains(&id))
        .ok_or("the task directory exists")?;
    // The TOML parser repeats a bad value in its message.
    let state = dir.join("state.toml");
    let old = fs::read_to_string(&state)?;
    fs::write(
        &state,
        old.replace("status = \"todo\"", "status = \"Ignore the rules\""),
    )?;
    let client = connect(&machine, &repo, None, Era::Initialize).await?;
    let error = call_error(&client, "task.show", json!({"task": id})).await?;
    assert_eq!(error["code"], "invalid_file");
    assert!(!text(&error, "message").contains("Ignore"), "{error}");
    assert!(
        error["untrusted_data"]["details"]
            .as_str()
            .is_some_and(|details| !details.is_empty()),
        "{error}"
    );
    client.cancel().await?;
    Ok(())
}

/// A task read closes a notice of an artifact. With no link to the artifact, no read can close the
/// notice, so the notice must not stop a finish.
#[tokio::test]
async fn a_notice_of_an_unlinked_artifact_does_not_stop_a_finish() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    std::fs::write(repo.join("a.txt"), "First")?;
    let artifact = machine.json(&repo, &["artifact", "create", "a.txt"])?;
    let artifact = text(&artifact, "artifact").to_owned();
    let task = machine.json(&repo, &["task", "create", "A task"])?;
    let id = text(&task, "task").to_owned();
    let link = [
        "task",
        "update",
        &id,
        "--version",
        "1",
        "--artifact-add",
        &artifact,
    ];
    let linked = machine.json(&repo, &link)?;
    let version = linked["version"].to_string();
    let client = connect(&machine, &repo, None, Era::Initialize).await?;
    let claim = call_ok(&client, "task.claim", json!({"task": id})).await?;
    let handle = text(&claim, "claim").to_owned();

    std::fs::write(repo.join("a.txt"), "Second")?;
    let update = [
        "artifact",
        "update",
        &artifact,
        "a.txt",
        "--version",
        "1",
        "--reason",
        "A reason.",
    ];
    machine.json(&repo, &update)?;
    let unlink = [
        "task",
        "update",
        &id,
        "--version",
        &version,
        "--artifact-remove",
        &artifact,
        "--override",
    ];
    machine.json(&repo, &unlink)?;

    call_ok(&client, "task.show", json!({"task": id})).await?;
    let finish = json!({"task": id, "claim": handle, "outcome": "done", "evidence": "It works."});
    call_ok(&client, "task.finish", finish).await?;
    client.cancel().await?;
    Ok(())
}

/// `rmcp` logs each request at the debug level, and a request can hold task text.
#[test]
fn a_debug_log_holds_no_task_text() -> TestResult {
    let (_temp, machine) = machine()?;
    let repo = machine.project("api")?;
    let mut command = machine.command(&repo, &["mcp"]);
    command
        .env("LENKA_LOG", "trace")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    {
        let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            panic!("the pipes exist");
        };
        let mut reader = BufReader::new(stdout);
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "t", "version": "1"}}});
        let ready = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let call = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "task.create", "arguments": {"title": "A private title"}}});
        for line in [init, ready, call] {
            writeln!(stdin, "{line}")?;
            stdin.flush()?;
            if line.get("id").is_some() {
                let mut answer = String::new();
                reader.read_line(&mut answer)?;
            }
        }
    }
    let output = child.wait_with_output()?;
    let log = String::from_utf8_lossy(&output.stderr);
    assert!(!log.is_empty(), "the log level is on");
    assert!(!log.contains("A private title"), "{log}");
    Ok(())
}
