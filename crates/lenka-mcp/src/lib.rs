// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! MCP over stdio. One service answers the revisions `2026-07-28` and `2025-11-25`.
//!
//! The standard output carries only protocol data, and logs go to the standard error. No project
//! loads until a tool call needs it, so the first answer stays fast.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use lenka_core::init::{CallerSetup, caller_for};
use lenka_core::{Host, Plugin, PluginInfo, Registry};
use lenka_plugin_api::{Actor, Caller, ErrorCode, LenkaError, Result, ToolDef};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities,
    ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

/// The protocol revisions of this server.
const VERSIONS: &[ProtocolVersion] =
    &[ProtocolVersion::V_2025_11_25, ProtocolVersion::V_2026_07_28];

/// The largest size of a client name in an author label.
const MAX_NAME_CHARS: usize = 64;

/// The MCP plugin. It adds no tools. It gives the admin view its state.
#[derive(Debug, Clone, Copy, Default)]
pub struct McpPlugin;

/// Returns the MCP plugin.
#[must_use]
pub fn plugin() -> McpPlugin {
    McpPlugin
}

impl Plugin for McpPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "mcp",
            description: "MCP server over stdio",
            sensitive: false,
        }
    }

    fn register(&self, _registry: &mut Registry) {}
}

/// The process facts for a server.
#[derive(Debug, Clone)]
pub struct ServerSetup {
    /// The directory of the process.
    pub cwd: PathBuf,
    /// The project from `LENKA_PROJECT`.
    pub project: Option<String>,
    /// The agent name from `LENKA_AGENT`.
    pub agent: Option<String>,
}

impl ServerSetup {
    /// Reads the facts from the environment of the process.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Io`] if the current directory is not available.
    pub fn from_env() -> Result<Self> {
        let cwd = std::env::current_dir()
            .map_err(|err| LenkaError::io("Read the current directory", &err))?;
        let text = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        Ok(Self {
            cwd,
            project: text("LENKA_PROJECT"),
            agent: text("LENKA_AGENT"),
        })
    }
}

/// The MCP service of a host.
#[derive(Debug, Clone)]
pub struct McpServer {
    host: Arc<Host>,
    setup: Arc<ServerSetup>,
}

impl McpServer {
    /// Creates a service.
    #[must_use]
    pub fn new(host: Arc<Host>, setup: ServerSetup) -> Self {
        Self {
            host,
            setup: Arc::new(setup),
        }
    }

    fn caller(&self, client: Option<&str>) -> Result<Caller> {
        caller_for(
            self.host.paths(),
            &self.setup.cwd,
            CallerSetup {
                actor: Actor::Agent,
                label: agent_label(self.setup.agent.as_deref(), client),
                project: self.setup.project.clone(),
                agent: self.setup.agent.clone(),
            },
        )
    }
}

/// Forms the author label from the agent name, then the client name, then a fixed name.
fn agent_label(agent: Option<&str>, client: Option<&str>) -> String {
    let raw = agent.or(client).unwrap_or("mcp");
    let clean: String = raw
        .chars()
        .filter(|c| !c.is_control() && !lenka_core::input::is_direction_control(*c))
        .take(MAX_NAME_CHARS)
        .collect();
    format!("agent:{}", if clean.is_empty() { "mcp" } else { &clean })
}

/// Forms the MCP definition of a tool.
fn tool_of(def: ToolDef) -> Tool {
    let schema: JsonObject = match def.input_schema {
        Value::Object(map) => map,
        _ => JsonObject::new(),
    };
    Tool::new(def.name, def.description, Arc::new(schema))
        .with_annotations(ToolAnnotations::new().read_only(def.read_only))
}

/// Forms the JSON of an error. The message of an invalid file can repeat text of this file, so
/// this text goes in `untrusted_data`.
fn error_json(error: &LenkaError) -> Value {
    if error.code() == ErrorCode::InvalidFile {
        return json!({
            "code": error.code().as_str(),
            "message": "A data file is not valid. A person must repair it. Run `lenka doctor`.",
            "untrusted_data": {"details": error.message()},
        });
    }
    json!({"code": error.code().as_str(), "message": error.message()})
}

/// Forms a tool result. A tool error is a result, so that the model can correct the call.
fn result_of(outcome: Result<Value>) -> CallToolResult {
    match outcome {
        Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
        Err(error) => {
            CallToolResult::error(vec![ContentBlock::text(error_json(&error).to_string())])
        }
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info = Implementation::new("lenka", env!("CARGO_PKG_VERSION"));
        info
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(VERSIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            self.host.tools().into_iter().map(tool_of).collect(),
        ))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.host
            .tools()
            .into_iter()
            .find(|def| def.name == name)
            .map(tool_of)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, ErrorData> {
        let name = request.name.into_owned();
        if !self.host.tools().iter().any(|def| def.name == name) {
            return Err(ErrorData::invalid_params(
                format!("The tool {name} does not exist."),
                None,
            ));
        }
        let args = Value::Object(request.arguments.unwrap_or_default());
        let client = context.client_info().map(|info| info.name);
        let server = self.clone();
        // A tool call does file I/O, and it can run `git`.
        let outcome = tokio::task::spawn_blocking(move || {
            let caller = server.caller(client.as_deref())?;
            server.host.call_tool(&caller, &name, args)
        })
        .await
        .map_err(|error| {
            tracing::error!(error = %error, "a tool call stopped");
            ErrorData::internal_error("The tool call stopped in the server.", None)
        })?;
        Ok(result_of(outcome).into())
    }
}

/// Serves MCP on the standard input and the standard output until the client closes them.
///
/// # Errors
///
/// Returns [`ErrorCode::Io`] if the runtime cannot start or the session fails to start.
pub fn serve_stdio(host: Arc<Host>, setup: ServerSetup) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| LenkaError::io("Start the runtime", &err))?;
    let server = McpServer::new(host, setup);
    let outcome = runtime.block_on(async move {
        let running = server
            .serve((session_input(), tokio::io::stdout()))
            .await
            .map_err(|error| {
                LenkaError::new(
                    ErrorCode::Io,
                    format!("The MCP session did not start: {error}"),
                )
            })?;
        running.waiting().await.map_err(|error| {
            LenkaError::new(ErrorCode::Io, format!("The MCP session stopped: {error}"))
        })?;
        Ok(())
    });
    // A blocking read of the standard input must not hold the process open.
    runtime.shutdown_background();
    outcome
}

/// Gives the standard input to `rmcp`, with no notification before the session starts.
///
/// Until the first request other than `ping` or `server/discover`, `rmcp` stops the server at any
/// notification. A client can send one in this time, and no such notification has a meaning there.
fn session_input() -> tokio::io::DuplexStream {
    let (mut writer, reader) = tokio::io::duplex(64 * 1024);
    tokio::spawn(async move {
        let mut input = tokio::io::BufReader::new(tokio::io::stdin());
        copy_session_input(&mut input, &mut writer).await;
    });
    reader
}

async fn copy_session_input<R, W>(input: &mut R, writer: &mut W)
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
    let mut line = Vec::new();
    loop {
        line.clear();
        match input.read_until(b'\n', &mut line).await {
            Ok(count) if count > 0 => {}
            _ => return,
        }
        let kind = early_line_kind(&line);
        if kind != EarlyLine::Notification && writer.write_all(&line).await.is_err() {
            return;
        }
        if kind == EarlyLine::SessionStart {
            break;
        }
    }
    let _ = tokio::io::copy_buf(input, writer).await;
}

/// What a line of the client means before the session starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EarlyLine {
    Notification,
    SessionStart,
    /// `ping`, `server/discover`, or text that `rmcp` must read and answer.
    Other,
}

fn early_line_kind(line: &[u8]) -> EarlyLine {
    let Ok(Value::Object(message)) = serde_json::from_slice::<Value>(line) else {
        return EarlyLine::Other;
    };
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return EarlyLine::Other;
    };
    if !message.contains_key("id") {
        EarlyLine::Notification
    } else if matches!(method, "ping" | "server/discover") {
        EarlyLine::Other
    } else {
        EarlyLine::SessionStart
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifications_pass_after_the_session_starts() -> Result<()> {
        let text = concat!(
            "{\"method\":\"notifications/a\"}\n",
            "{\"id\":1,\"method\":\"initialize\"}\n",
            "{\"method\":\"notifications/b\"}\n",
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|err| LenkaError::io("Start the runtime", &err))?;
        let mut output = Vec::new();
        runtime.block_on(copy_session_input(&mut text.as_bytes(), &mut output));
        let output = String::from_utf8_lossy(&output);
        assert!(!output.contains("notifications/a"), "{output}");
        assert!(output.contains("initialize"), "{output}");
        assert!(output.contains("notifications/b"), "{output}");
        Ok(())
    }

    #[test]
    fn only_a_real_request_starts_the_session() {
        let kind = |text: &str| early_line_kind(text.as_bytes());
        assert_eq!(
            kind(r#"{"jsonrpc":"2.0","method":"notifications/cancelled"}"#),
            EarlyLine::Notification
        );
        assert_eq!(kind(r#"{"id":1,"method":"ping"}"#), EarlyLine::Other);
        assert_eq!(
            kind(r#"{"id":1,"method":"server/discover"}"#),
            EarlyLine::Other
        );
        assert_eq!(
            kind(r#"{"id":1,"method":"initialize"}"#),
            EarlyLine::SessionStart
        );
        assert_eq!(
            kind(r#"{"id":1,"method":"tools/list"}"#),
            EarlyLine::SessionStart
        );
        assert_eq!(kind("not json"), EarlyLine::Other);
        assert_eq!(kind(r#"{"id":1,"result":{}}"#), EarlyLine::Other);
    }

    #[test]
    fn a_label_has_no_bidirectional_control() {
        assert_eq!(agent_label(Some("root\u{202e}x\n"), None), "agent:rootx");
        assert_eq!(agent_label(None, Some("\u{2066}")), "agent:mcp");
    }

    #[test]
    fn the_label_prefers_the_agent_name() {
        assert_eq!(agent_label(Some("a"), Some("b")), "agent:a");
        assert_eq!(agent_label(None, Some("b")), "agent:b");
        assert_eq!(agent_label(None, None), "agent:mcp");
        assert_eq!(agent_label(None, Some("\n\t")), "agent:mcp");
        assert_eq!(agent_label(None, Some("a\nb")), "agent:ab");
        assert_eq!(
            agent_label(Some(&"x".repeat(99)), None).len(),
            6 + MAX_NAME_CHARS
        );
    }

    #[test]
    fn a_tool_error_is_a_result_with_the_code() {
        let result = result_of(Err(LenkaError::new(
            ErrorCode::ClaimLost,
            "Claim the task again.",
        )));
        assert_eq!(result.is_error, Some(true));
        let text = result.content[0]
            .as_text()
            .map(|item| item.text.clone())
            .unwrap_or_default();
        assert_eq!(
            serde_json::from_str::<Value>(&text).ok(),
            Some(json!({"code": "claim_lost", "message": "Claim the task again."}))
        );

        let result = result_of(Ok(json!({"task": "a"})));
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn a_tool_definition_carries_the_read_only_hint() {
        let tool = tool_of(ToolDef {
            name: "task.list",
            description: "List.",
            input_schema: json!({"type": "object", "properties": {}}),
            read_only: true,
        });
        assert_eq!(
            tool.annotations.and_then(|item| item.read_only_hint),
            Some(true)
        );
    }
}
