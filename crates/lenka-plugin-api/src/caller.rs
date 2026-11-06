// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The kind of actor that does an operation or a stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    /// An AI agent, through MCP.
    Agent,
    /// A person, through the CLI or the desktop app.
    Human,
    /// Either kind. This value is valid only on a stage.
    Any,
}

impl Actor {
    /// Tells if this caller can do a stage with the given actor.
    #[must_use]
    pub fn can_do(self, stage_actor: Self) -> bool {
        stage_actor == Self::Any || self == stage_actor
    }
}

/// The identity of an inbox: a hash of the worktree path and the optional agent name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AgentKey(pub String);

/// The projects that a caller can use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectScope {
    /// Only the projects in this list. An agent has this scope.
    Listed(Vec<String>),
    /// All projects of this user. The desktop app has this scope.
    All,
}

/// Who makes a call, and from where.
#[derive(Debug, Clone)]
pub struct Caller {
    /// The kind of actor.
    pub actor: Actor,
    /// The author label, such as `agent:claude-code`. Nothing verifies it.
    pub label: String,
    /// The worktree or directory of the process.
    pub worktree: PathBuf,
    /// The inbox identity.
    pub agent_key: AgentKey,
    /// The projects that the caller can use.
    pub scope: ProjectScope,
    /// The project to use when a call names none.
    pub default_project: Option<String>,
}
