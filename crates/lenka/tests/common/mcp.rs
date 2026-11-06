// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! A real `rmcp` client for tests. It talks to the built binary over stdio.

use std::path::Path;
use std::process::Stdio;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation,
    ProtocolVersion,
};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::TokioChildProcess;
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use serde_json::Value;

use crate::common::{Machine, TestResult};

#[derive(Clone)]
pub struct TestClient;

impl ClientHandler for TestClient {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("test-client", "1.0.0"),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Era {
    Initialize,
    Discover,
}

pub fn child(machine: &Machine, dir: &Path, agent: Option<&str>) -> TestResult<TokioChildProcess> {
    let mut command = machine.command(dir, &["mcp"]);
    if let Some(name) = agent {
        command.env("LENKA_AGENT", name);
    }
    let command = tokio::process::Command::from(command);
    let (process, _stderr) = TokioChildProcess::builder(command)
        .stderr(Stdio::null())
        .spawn()?;
    Ok(process)
}

pub async fn connect(
    machine: &Machine,
    dir: &Path,
    agent: Option<&str>,
    era: Era,
) -> TestResult<RunningService<RoleClient, TestClient>> {
    let transport = child(machine, dir, agent)?;
    let lifecycle = match era {
        Era::Initialize => ClientLifecycleMode::Initialize,
        Era::Discover => ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    };
    Ok(TestClient
        .serve_with_lifecycle(transport, lifecycle)
        .await?)
}

pub fn params(name: &'static str, args: &Value) -> CallToolRequestParams {
    let mut params = CallToolRequestParams::new(name);
    params.arguments = args.as_object().cloned();
    params
}

pub fn result_text(result: &CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|item| item.as_text())
        .map(|item| item.text.clone())
        .unwrap_or_default()
}

pub async fn call_ok(
    client: &RunningService<RoleClient, TestClient>,
    name: &'static str,
    args: Value,
) -> TestResult<Value> {
    let result = client.call_tool(params(name, &args)).await?;
    assert_eq!(
        result.is_error,
        Some(false),
        "{name}: {}",
        result_text(&result)
    );
    Ok(serde_json::from_str(&result_text(&result))?)
}

pub async fn call_error(
    client: &RunningService<RoleClient, TestClient>,
    name: &'static str,
    args: Value,
) -> TestResult<Value> {
    let result = client.call_tool(params(name, &args)).await?;
    assert_eq!(
        result.is_error,
        Some(true),
        "{name}: {}",
        result_text(&result)
    );
    Ok(serde_json::from_str(&result_text(&result))?)
}
