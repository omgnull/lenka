// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

use serde::Serialize;

/// A shorter name for results with a [`LenkaError`].
pub type Result<T, E = LenkaError> = std::result::Result<T, E>;

/// The stable code of an error. Agents and scripts can match on this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The agent read an old version.
    StaleVersion,
    /// The agent has not read a change.
    UnreadChanges,
    /// The claim expired, or another agent holds it.
    ClaimLost,
    /// The task has active blockers.
    Blocked,
    /// The relation creates a cycle.
    Cycle,
    /// The text matches a secret pattern.
    SecretDetected,
    /// A file is invalid, has conflict markers, or names a missing stage.
    InvalidFile,
    /// The data format is newer than this process.
    FormatTooNew,
    /// A short ID matches more than one task.
    AmbiguousId,
    /// A policy does not allow the link.
    LinkDenied,
    /// The clone does not list the project.
    ProjectNotAvailable,
    /// A delete rule stops the operation.
    DeleteRefused,
    /// The item does not exist.
    NotFound,
    /// A call parameter is not valid.
    InvalidArgument,
    /// This actor cannot do this operation.
    ActorDenied,
    /// A material change has no reason.
    ReasonRequired,
    /// The task reached the limit of agent changes, and a person must act.
    LoopLimit,
    /// An input or output operation failed.
    Io,
}

impl ErrorCode {
    /// Returns the code as the text that tool results show.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StaleVersion => "stale_version",
            Self::UnreadChanges => "unread_changes",
            Self::ClaimLost => "claim_lost",
            Self::Blocked => "blocked",
            Self::Cycle => "cycle",
            Self::SecretDetected => "secret_detected",
            Self::InvalidFile => "invalid_file",
            Self::FormatTooNew => "format_too_new",
            Self::AmbiguousId => "ambiguous_id",
            Self::LinkDenied => "link_denied",
            Self::ProjectNotAvailable => "project_not_available",
            Self::DeleteRefused => "delete_refused",
            Self::NotFound => "not_found",
            Self::InvalidArgument => "invalid_argument",
            Self::ActorDenied => "actor_denied",
            Self::ReasonRequired => "reason_required",
            Self::LoopLimit => "loop_limit",
            Self::Io => "io",
        }
    }
}

/// An error with a stable code and a message that states the next action.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{}: {message}", code.as_str())]
pub struct LenkaError {
    code: ErrorCode,
    message: String,
}

impl LenkaError {
    /// Creates an error.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Creates an [`ErrorCode::Io`] error with the operation that failed.
    #[must_use]
    pub fn io(what: &str, err: &std::io::Error) -> Self {
        Self::new(ErrorCode::Io, format!("{what}: {err}"))
    }

    /// Creates an [`ErrorCode::InvalidArgument`] error.
    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    /// Creates an [`ErrorCode::NotFound`] error.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    /// Creates an [`ErrorCode::InvalidFile`] error.
    pub fn invalid_file(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidFile, message)
    }

    /// Returns the stable code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// Returns the message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}
