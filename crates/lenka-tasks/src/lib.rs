// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The task plugin: tasks, plans, entries, stages, ready work, claims, notices, and soft links.

mod index;
mod links;
pub mod markdown;
pub mod model;
mod service;
mod tools;

pub use links::{LinkState, LinkView};
pub use model::{
    ArtifactRecord, ArtifactVersion, Claim, Entry, EntryKind, Link, Priority, State, Status,
    TaskFront, TaskMeta, VersionMeta,
};
pub use service::{
    ArtifactRouting, ArtifactView, BlockerView, BoardRow, CallNotes, CheckItem, ClaimRef,
    DoctorReport, Finish, ListFilter, NewEntry, NewTask, RelatedView, TaskDetail, TaskPatch,
    TaskRow, TaskService, artifact_view, is_safe_file_name, read_artifact, service_of,
};
pub use tools::{TasksPlugin, detail_json, plugin, task_ref, with_notes};
