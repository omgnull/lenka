// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The acceptance scenario of the design, with one real MCP server process for each agent.

#[expect(dead_code, reason = "this test binary uses only a part of the helpers")]
mod common;
#[path = "common/mcp.rs"]
mod mcp_client;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Machine, TestResult, text};
use mcp_client::{Era, TestClient, call_error, call_ok, connect, params, result_text};
use rmcp::service::{RoleClient, RunningService};
use serde_json::{Value, json};

type Client = RunningService<RoleClient, TestClient>;

struct Agent {
    client: Client,
    worktree: PathBuf,
    task: String,
    claim: String,
}

impl Agent {
    /// Starts one server process, and creates and claims a task that links to the artifact.
    async fn start(
        machine: &Machine,
        worktree: &Path,
        name: &str,
        title: &str,
        artifact: &str,
    ) -> TestResult<Self> {
        let client = connect(machine, worktree, Some(name), Era::Discover).await?;
        let created = call_ok(
            &client,
            "task.create",
            json!({"title": title, "acceptance": "- [x] It works"}),
        )
        .await?;
        let task = text(&created, "task").to_owned();
        call_ok(
            &client,
            "task.update",
            json!({"task": task, "version": 1, "artifacts_add": [artifact]}),
        )
        .await?;
        let claimed = call_ok(&client, "task.claim", json!({"task": task})).await?;
        let claim = text(&claimed, "claim").to_owned();
        assert!(!claim.is_empty(), "{claimed}");
        Ok(Self {
            client,
            worktree: worktree.to_path_buf(),
            task,
            claim,
        })
    }

    async fn finish_error(&self) -> TestResult<Value> {
        call_error(
            &self.client,
            "task.finish",
            json!({
                "task": self.task, "outcome": "done", "claim": self.claim,
                "evidence": "The tests pass.",
            }),
        )
        .await
    }

    async fn finish(&self) -> TestResult<Value> {
        call_ok(
            &self.client,
            "task.finish",
            json!({
                "task": self.task, "outcome": "done", "claim": self.claim,
                "evidence": "The tests pass.",
            }),
        )
        .await
    }
}

/// Creates the contract artifact at version 3.
async fn contract_artifact(machine: &Machine, worktree: &Path) -> TestResult<String> {
    let client = connect(machine, worktree, Some("setup"), Era::Initialize).await?;
    let file = worktree.join("contract.json");
    fs::write(&file, "{\"contract\": 1}\n")?;
    let created = call_ok(
        &client,
        "artifact.create",
        json!({"path": file, "name": "contract.json", "media_type": "application/json"}),
    )
    .await?;
    let id = text(&created, "artifact").to_owned();
    for version in 1_u64..=2 {
        fs::write(&file, format!("{{\"contract\": {}}}\n", version + 1))?;
        let updated = call_ok(
            &client,
            "artifact.update",
            json!({
                "artifact": id, "version": version, "path": file,
                "reason": "Prepare the contract.",
            }),
        )
        .await?;
        assert_eq!(updated["version"], version + 1);
    }
    client.cancel().await?;
    Ok(id)
}

/// Steps 1 to 4 of the scenario in section 12.1 of the design.
async fn contract_scenario(machine: &Machine, places: [(&Path, &str); 3]) -> TestResult {
    let [(place_1, name_1), (place_2, name_2), (place_3, name_3)] = places;
    let artifact = contract_artifact(machine, place_1).await?;
    let agent_1 = Agent::start(machine, place_1, name_1, "Contract", &artifact).await?;
    let agent_2 = Agent::start(machine, place_2, name_2, "Service 1", &artifact).await?;
    let agent_3 = Agent::start(machine, place_3, name_3, "Service 2", &artifact).await?;

    // Step 1: agent 1 changes the contract from version 3 to version 4.
    let file = agent_1.worktree.join("contract.json");
    fs::write(&file, "{\"contract\": 4}\n")?;
    let updated = call_ok(
        &agent_1.client,
        "artifact.update",
        json!({
            "artifact": artifact, "version": 3, "path": file,
            "reason": "The field names change.", "claim": agent_1.claim,
        }),
    )
    .await?;
    assert_eq!(updated["version"], 4);

    // Steps 2 and 3: the next tool call of agents 2 and 3 shows the notice.
    for agent in [&agent_2, &agent_3] {
        let result = call_ok(&agent.client, "task.list", json!({"limit": 1})).await?;
        let unread = result["unread_changes"]
            .as_array()
            .ok_or("no unread_changes field")?;
        assert_eq!(unread.len(), 1, "{result}");
        assert_eq!(unread[0]["kind"], "artifact");
        assert_eq!(unread[0]["version"], 4);
        assert_eq!(
            unread[0]["untrusted_data"]["reason"],
            "The field names change."
        );
    }

    // The author of the change gets no notice.
    let own = call_ok(&agent_1.client, "task.list", json!({"limit": 1})).await?;
    assert!(own.get("unread_changes").is_none(), "{own}");

    // Step 4: an agent that still works on version 3 cannot finish.
    assert_eq!(agent_2.finish_error().await?["code"], "unread_changes");
    assert_eq!(agent_3.finish_error().await?["code"], "unread_changes");

    // After a read of version 4, the agent can finish. The other agent stays blocked.
    let shown = call_ok(
        &agent_2.client,
        "task.show",
        json!({"task": agent_2.task, "include": ["artifacts"]}),
    )
    .await?;
    let links = serde_json::to_string(&shown)?;
    assert!(links.contains("\"version\":4"), "{links}");
    assert_eq!(agent_2.finish().await?["status"], "done");
    assert_eq!(agent_3.finish_error().await?["code"], "unread_changes");

    call_ok(
        &agent_3.client,
        "task.show",
        json!({"task": agent_3.task, "include": ["artifacts"]}),
    )
    .await?;
    assert_eq!(agent_3.finish().await?["status"], "done");
    assert_eq!(agent_1.finish().await?["status"], "done");

    for agent in [agent_1, agent_2, agent_3] {
        agent.client.cancel().await?;
    }
    Ok(())
}

#[tokio::test]
async fn the_contract_scenario_works_in_one_repository_with_worktrees() -> TestResult {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let repo = machine.project("repo")?;
    let first = temp.path().join("worktree-1");
    let second = temp.path().join("worktree-2");
    for (place, branch) in [(&first, "one"), (&second, "two")] {
        let path = place.to_str().ok_or("path")?;
        machine.git(&repo, &["worktree", "add", "-q", "-b", branch, path])?;
    }
    contract_scenario(
        &machine,
        [
            (&repo, "contract"),
            (&first, "service-1"),
            (&second, "service-2"),
        ],
    )
    .await
}

#[tokio::test]
async fn the_contract_scenario_works_in_two_repositories_with_a_shared_project() -> TestResult {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let first = machine.repo("first")?;
    let second = machine.repo("second")?;
    machine.ok(&first, &["init", "--shared", "--name", "team"])?;
    machine.ok(&second, &["project", "attach", "team"])?;
    contract_scenario(
        &machine,
        [
            (&first, "contract"),
            (&second, "service-1"),
            (&second, "service-2"),
        ],
    )
    .await
}

#[tokio::test]
async fn exactly_one_process_wins_a_claim() -> TestResult {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let repo = machine.project("repo")?;
    let mut clients = Vec::new();
    for number in 0..8 {
        let name = format!("agent-{number}");
        clients.push(connect(&machine, &repo, Some(&name), Era::Discover).await?);
    }
    let created = call_ok(&clients[0], "task.create", json!({"title": "One task"})).await?;
    let task = text(&created, "task").to_owned();

    let mut calls = Vec::new();
    for client in clients {
        let args = json!({"task": task});
        calls.push(tokio::spawn(async move {
            let result = client.call_tool(params("task.claim", &args)).await;
            let answer = result.map(|item| (item.is_error, result_text(&item)));
            client.cancel().await.ok();
            answer
        }));
    }
    let mut winners = 0;
    for call in calls {
        let (is_error, body) = call.await??;
        if is_error == Some(true) {
            let value: Value = serde_json::from_str(&body)?;
            assert_eq!(value["code"], "claim_lost", "{body}");
        } else {
            winners += 1;
        }
    }
    assert_eq!(winners, 1);
    Ok(())
}

#[tokio::test]
async fn a_second_process_gets_stale_version_for_an_old_write() -> TestResult {
    let temp = tempfile::tempdir()?;
    let machine = Machine::new(temp.path())?;
    let repo = machine.project("repo")?;
    let first = connect(&machine, &repo, Some("first"), Era::Discover).await?;
    let second = connect(&machine, &repo, Some("second"), Era::Initialize).await?;
    let created = call_ok(&first, "task.create", json!({"title": "A task"})).await?;
    let task = text(&created, "task");
    call_ok(
        &second,
        "task.update",
        json!({"task": task, "version": 1, "title": "From the second agent"}),
    )
    .await?;
    let error = call_error(
        &first,
        "task.update",
        json!({"task": task, "version": 1, "title": "From the first agent"}),
    )
    .await?;
    assert_eq!(error["code"], "stale_version");
    let shown = call_ok(&first, "task.show", json!({"task": task})).await?;
    assert_eq!(shown["untrusted_data"]["title"], "From the second agent");
    first.cancel().await?;
    second.cancel().await?;
    Ok(())
}
