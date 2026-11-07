// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The requests that views send to the app. Views draw, and the app runs the requests after the
//! frame, so that drawing code never writes data.

use lenka_core::SecretPolicy;
use lenka_plugin_api::{ArtifactId, TaskId};
use lenka_tasks::{EntryKind, Status, TaskPatch};

use crate::dialogs::Dialog;

/// A task change that can meet a claim of another holder.
#[derive(Debug, Clone)]
pub(crate) enum TaskOp {
    SetStatus {
        id: TaskId,
        status: Status,
    },
    Finish {
        id: TaskId,
        evidence: String,
        unchecked_reason: Option<String>,
    },
    CompleteStage {
        id: TaskId,
        evidence: String,
    },
    ReturnStage {
        id: TaskId,
        stage: String,
        reason: String,
    },
    Pause {
        id: TaskId,
    },
    /// A change of a field. The flag tells if the change saves the text drafts of the detail panel.
    Update {
        id: TaskId,
        patch: Box<TaskPatch>,
        from_draft: bool,
    },
}

/// The main views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum View {
    Board,
    Tree,
    Roadmap,
    Gantt,
    Drawing,
    Settings,
}

/// A request from a view.
#[derive(Debug)]
pub(crate) enum Action {
    Select(TaskId),
    CloseDetail,
    /// Opens another project at the end of the frame, because the views of a frame use the rows
    /// of one project.
    OpenProject(String),
    ReloadSettings,
    /// A drop on a board column, or a choice in the status field.
    MoveTo {
        id: TaskId,
        status: Status,
    },
    Task(TaskOp),
    Open(Dialog),
    OpenFinish(TaskId),
    OpenCompleteStage(TaskId),
    OpenReturnStage(TaskId),
    OpenNewTask {
        parent: Option<TaskId>,
    },
    SaveDraft,
    ReloadDetail,
    DiscardDraft,
    SaveWithSecret,
    AddEntry {
        id: TaskId,
        kind: EntryKind,
        text: String,
        policy: SecretPolicy,
    },
    AddFile {
        id: TaskId,
        path: String,
    },
    OpenDrawing {
        artifact: ArtifactId,
    },
    ShowVersion {
        id: TaskId,
        version: u64,
    },
    SaveSettings,
    RunDoctor,
}
