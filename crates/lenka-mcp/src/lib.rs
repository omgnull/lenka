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
        .filter(|c| !c.is_control())
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

/// Forms a tool result. A tool error is a result, so that the model can correct the call.
fn result_of(outcome: Result<Value>) -> CallToolResult {
    match outcome {
        Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
        Err(error) => {
            let text = json!({"code": error.code().as_str(), "message": error.message()});
            CallToolResult::error(vec![ContentBlock::text(text.to_string())])
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
            .serve(rmcp::transport::stdio())
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

#[cfg(test)]
mod tests {
    use super::*;

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
