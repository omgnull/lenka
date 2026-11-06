// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The stored forms of tasks, states, entries, claims, and versions.

use std::fmt;

use lenka_plugin_api::{
    Actor, AgentKey, ArtifactId, Day, DocStat, EntryId, LenkaError, ProjectId, Result, TaskId,
    Timestamp,
};
use serde::{Deserialize, Serialize};

/// The fixed statuses of a task. "Blocked" is calculated, so lenka does not store it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The task waits for a decision to do it.
    Backlog,
    /// The task can start.
    Todo,
    /// The work started.
    InProgress,
    /// The work stopped until a person or a claim names the task.
    Paused,
    /// The work is complete.
    Done,
    /// The work will not happen.
    Cancelled,
}

impl Status {
    /// All statuses in board order.
    pub const ALL: [Self; 6] = [
        Self::Backlog,
        Self::Todo,
        Self::InProgress,
        Self::Paused,
        Self::Done,
        Self::Cancelled,
    ];

    /// Tells if the task is complete in any way.
    #[must_use]
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }

    /// Returns the stored name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Backlog => "backlog",
            Self::Todo => "todo",
            Self::InProgress => "in_progress",
            Self::Paused => "paused",
            Self::Done => "done",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parses the stored name.
    ///
    /// # Errors
    ///
    /// Returns [`lenka_plugin_api::ErrorCode::InvalidArgument`] for another name.
    pub fn parse(text: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == text)
            .ok_or_else(|| {
                LenkaError::invalid_argument(
                    "A status is backlog, todo, in_progress, paused, done, or cancelled.",
                )
            })
    }
}

/// The priority of a task.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    /// Low.
    Low,
    /// Normal.
    #[default]
    Normal,
    /// High.
    High,
    /// Urgent.
    Urgent,
}

impl Priority {
    /// All priorities from low to high.
    pub const ALL: [Self; 4] = [Self::Low, Self::Normal, Self::High, Self::Urgent];

    /// Returns the stored name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
            Self::Urgent => "urgent",
        }
    }
}

/// A relation target: a task in this project, or a soft link to another project.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Link {
    /// A task in the same project.
    Local(TaskId),
    /// A task in another project.
    Remote(ProjectId, TaskId),
}

impl Link {
    /// Parses a stored link: `<task-id>` or `<project-id>/<task-id>`.
    ///
    /// # Errors
    ///
    /// Returns an error if an ID is not valid.
    pub fn parse(text: &str) -> Result<Self> {
        match text.split_once('/') {
            Some((project, task)) => Ok(Self::Remote(
                ProjectId::parse(project)?,
                TaskId::parse(task)?,
            )),
            None => Ok(Self::Local(TaskId::parse(text)?)),
        }
    }

    /// Returns the task of a local link.
    #[must_use]
    pub fn local(self) -> Option<TaskId> {
        match self {
            Self::Local(task) => Some(task),
            Self::Remote(..) => None,
        }
    }
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(task) => write!(f, "{task}"),
            Self::Remote(project, task) => write!(f, "{project}/{task}"),
        }
    }
}

impl fmt::Debug for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Link({self})")
    }
}

impl TryFrom<String> for Link {
    type Error = LenkaError;

    fn try_from(text: String) -> Result<Self> {
        Self::parse(&text)
    }
}

impl From<Link> for String {
    fn from(link: Link) -> Self {
        link.to_string()
    }
}

/// The TOML front matter of `task.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskFront {
    /// The task ID.
    pub id: TaskId,
    /// The task type.
    #[serde(rename = "type")]
    pub kind: String,
    /// The title.
    pub title: String,
    /// The parent task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<TaskId>,
    /// The tasks that must complete first.
    #[serde(default)]
    pub blocked_by: Vec<Link>,
    /// The tasks with a link and no order.
    #[serde(default)]
    pub relates: Vec<Link>,
    /// The linked artifacts.
    #[serde(default)]
    pub artifacts: Vec<ArtifactId>,
    /// The priority.
    #[serde(default)]
    pub priority: Priority,
    /// The labels.
    #[serde(default)]
    pub labels: Vec<String>,
    /// The author label. Nothing verifies it.
    pub created_by: String,
    /// The creation time.
    pub created_at: Timestamp,
    /// The planned first day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<Day>,
    /// The planned last day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<Day>,
    /// Tells if a coordinator agent holds the plan of this task tree.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub coordinated: bool,
}

/// The contents of `state.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The content version.
    pub version: u64,
    /// The status.
    pub status: Status,
    /// The current stage.
    pub stage: String,
    /// Tells if the task completed its `unblocks` stage.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unblocked: bool,
    /// Tells if the loop limit stopped this task. An agent cannot claim it until a person acts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loop_stopped: bool,
    /// Tells if a person lets an agent continue the loop until the task is done.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loop_until_done: bool,
}

/// The kind of an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A decision.
    Decision,
    /// A comment.
    Comment,
    /// A proposal to a coordinator.
    Proposal,
    /// Evidence for a stage.
    Evidence,
}

impl EntryKind {
    /// Returns the stored name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Comment => "comment",
            Self::Proposal => "proposal",
            Self::Evidence => "evidence",
        }
    }
}

/// The front matter of an entry file, with the text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The entry ID.
    pub id: EntryId,
    /// The kind.
    pub kind: EntryKind,
    /// The author label.
    pub author: String,
    /// The time.
    pub at: Timestamp,
    /// The stage of evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// An earlier decision that this entry replaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaces: Option<EntryId>,
    /// The text. It is the file body, not a front matter field.
    #[serde(skip)]
    pub text: String,
}

/// The contents of `meta.toml` in a version directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionMeta {
    /// The version.
    pub version: u64,
    /// The author label.
    pub author: String,
    /// The time.
    pub at: Timestamp,
    /// The reason for a material change.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// The changed parts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<String>,
    /// Tells if this version is a material change.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub material: bool,
}

/// A claim on one stage of a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// The task.
    pub task: TaskId,
    /// The stage.
    pub stage: String,
    /// The random handle. It is state, not authentication.
    pub handle: String,
    /// The holder label.
    pub holder: String,
    /// The kind of holder.
    pub actor: Actor,
    /// The worktree of the holder.
    pub worktree: String,
    /// The inbox of the holder.
    pub agent_key: AgentKey,
    /// The time of the claim.
    pub claimed_at: Timestamp,
    /// The end of the claim.
    pub expires_at: Timestamp,
}

/// The index record of a task. It holds no description, plan, or entry text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskMeta {
    /// The front matter.
    pub front: TaskFront,
    /// The state.
    pub state: State,
    /// Tells if the task has a plan.
    pub has_plan: bool,
    /// The file stamps of `task.md`, `plan.md`, and `state.toml`.
    pub stamps: [Option<DocStat>; 3],
}

impl TaskMeta {
    /// Returns the task ID.
    #[must_use]
    pub fn id(&self) -> TaskId {
        self.front.id
    }
}

/// One version of an artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVersion {
    /// The version.
    pub version: u64,
    /// The file name in the version directory.
    pub file: String,
    /// The size in bytes.
    pub size: u64,
    /// The SHA-256 hash.
    pub sha256: String,
    /// The author label.
    pub author: String,
    /// The time.
    pub at: Timestamp,
    /// The reason for the change.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// The contents of `artifact.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    /// The artifact ID.
    pub id: ArtifactId,
    /// The display name.
    pub name: String,
    /// The media type.
    pub media_type: String,
    /// The current version.
    pub version: u64,
    /// The author label of the first version.
    pub created_by: String,
    /// The time of the first version.
    pub created_at: Timestamp,
    /// The kept versions, oldest first. The last item is the current version.
    pub versions: Vec<ArtifactVersion>,
}

impl ArtifactRecord {
    /// Returns the current version.
    ///
    /// # Panics
    ///
    /// Panics if the record has no versions. The parser and the writers keep at least one.
    #[must_use]
    pub fn current(&self) -> &ArtifactVersion {
        self.versions
            .last()
            .unwrap_or_else(|| panic!("the artifact {} has no versions", self.id))
    }
}
