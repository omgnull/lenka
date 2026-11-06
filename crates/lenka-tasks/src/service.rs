// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The task operations. The tools, the CLI, and the desktop app all call this service.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lenka_core::input;
use lenka_core::notices::{self, FEED_PATH};
use lenka_core::{
    Commit, FeedEvent, FeedKind, Locked, Notice, Op, Project, ProjectEvent, SecretPolicy, Stage,
    Subject, TextCheck,
};
use lenka_plugin_api::{
    Actor, ArtifactId, Caller, Day, DocPath, EntryId, ErrorCode, LenkaError, NoticeId, Result,
    TaskId, Timestamp,
};
use serde::Serialize;

use crate::index::{Index, LoadedTask, load_task, open_index};
use crate::links::{LinkReader, LinkState, LinkView, check_new_link};
use crate::markdown::{
    ACCEPTANCE, Body, Criterion, DESCRIPTION, MAX_TEXT_BYTES, REQUIREMENTS, criteria_text_differs,
    join_front, new_body, split_front,
};
use crate::model::{
    ArtifactRecord, Claim, Entry, EntryKind, Link, Priority, State, Status, TaskFront, TaskMeta,
    VersionMeta,
};

const MS_PER_MINUTE: i64 = 60_000;
const MS_PER_HOUR: i64 = 60 * MS_PER_MINUTE;
const MAX_TITLE_BYTES: usize = 500;

/// The largest count of labels in one call.
const MAX_LABELS_IN_CALL: usize = 100;

/// The largest count of links of one kind in one call.
const MAX_LINKS_IN_CALL: usize = 1000;

/// The largest count of criterion marks in one call. A longer list only costs time under the lock.
const MAX_CHECK_ITEMS: usize = 10_000;

/// The largest count of notices that the text of an error names.
const MAX_NAMES_IN_ERROR: usize = 20;
const EXTERNAL_AUTHOR: &str = "external";

/// Returns the task service of a project, and creates it on first use.
///
/// # Errors
///
/// Returns an error if the index cannot load.
pub fn service_of(project: &Arc<Project>) -> Result<Arc<TaskService>> {
    project.extension(TaskService::open)
}

/// How a call relates to a claim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ClaimRef {
    /// The call names no handle. The worktree identity of the caller decides.
    #[default]
    Implicit,
    /// The call names a claim handle.
    Handle(String),
    /// A person overrides any claim. An agent cannot do this.
    Override,
}

/// The fields of a new task.
#[derive(Debug, Clone, Default)]
pub struct NewTask {
    /// The type. An empty value selects `task`.
    pub kind: String,
    /// The title.
    pub title: String,
    /// The description.
    pub description: String,
    /// The requirements.
    pub requirements: String,
    /// The acceptance criteria.
    pub acceptance: String,
    /// The plan.
    pub plan: Option<String>,
    /// The parent task.
    pub parent: Option<TaskId>,
    /// The blockers.
    pub blocked_by: Vec<Link>,
    /// The related tasks.
    pub relates: Vec<Link>,
    /// The linked artifacts.
    pub artifacts: Vec<ArtifactId>,
    /// The priority.
    pub priority: Priority,
    /// The labels.
    pub labels: Vec<String>,
    /// The first status: `backlog` or `todo`. The default is `todo`.
    pub status: Option<Status>,
    /// The planned first day.
    pub start: Option<Day>,
    /// The planned last day.
    pub due: Option<Day>,
    /// The policy of the secret check. Only a person can override it.
    pub secret_policy: SecretPolicy,
}

/// A change of one acceptance criterion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckItem {
    /// The position of the criterion, from 0.
    pub index: usize,
    /// The new mark.
    pub done: bool,
}

/// A change to a task. An empty field changes nothing.
#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    /// The version that the caller read.
    pub expected_version: u64,
    /// The reason. A material change requires it.
    pub reason: Option<String>,
    /// The relation to a claim.
    pub claim: ClaimRef,
    /// A new title.
    pub title: Option<String>,
    /// A new description.
    pub description: Option<String>,
    /// New requirements.
    pub requirements: Option<String>,
    /// New acceptance criteria.
    pub acceptance: Option<String>,
    /// A new plan. An empty text removes the plan.
    pub plan: Option<String>,
    /// A new priority.
    pub priority: Option<Priority>,
    /// Labels to add.
    pub labels_add: Vec<String>,
    /// Labels to remove.
    pub labels_remove: Vec<String>,
    /// A new parent. `Some(None)` removes the parent.
    pub parent: Option<Option<TaskId>>,
    /// Blockers to add.
    pub blocked_by_add: Vec<Link>,
    /// Blockers to remove.
    pub blocked_by_remove: Vec<Link>,
    /// Related tasks to add.
    pub relates_add: Vec<Link>,
    /// Related tasks to remove.
    pub relates_remove: Vec<Link>,
    /// Artifacts to link.
    pub artifacts_add: Vec<ArtifactId>,
    /// Artifacts to unlink. This does not delete the artifact.
    pub artifacts_remove: Vec<ArtifactId>,
    /// Acceptance criteria to mark.
    pub check: Vec<CheckItem>,
    /// A new first day. `Some(None)` removes it.
    pub start: Option<Option<Day>>,
    /// A new last day. `Some(None)` removes it.
    pub due: Option<Option<Day>>,
    /// A new type. Only a person can change it.
    pub kind: Option<String>,
    /// A new coordinator setting. Only a person can change it.
    pub coordinated: Option<bool>,
    /// Lets an agent continue the loop until the task is done. Only a person can set this.
    pub loop_until_done: Option<bool>,
    /// The policy of the secret check. Only a person can override it.
    pub secret_policy: SecretPolicy,
}

/// A new entry.
#[derive(Debug, Clone)]
pub struct NewEntry {
    /// The kind.
    pub kind: EntryKind,
    /// The text.
    pub text: String,
    /// An earlier entry that this entry replaces.
    pub replaces: Option<EntryId>,
    /// The policy of the secret check. Only a person can override it.
    pub secret_policy: SecretPolicy,
}

/// The way to end work on a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    /// The work is complete.
    Done {
        /// The evidence, such as a command and its result.
        evidence: String,
        /// The reason to finish with an unchecked criterion.
        unchecked_reason: Option<String>,
    },
    /// End the claim, and make the task ready again.
    Released,
}

/// A filter for task lists.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    /// Only these statuses.
    pub statuses: Vec<Status>,
    /// Only this type.
    pub kind: Option<String>,
    /// Only tasks with this label.
    pub label: Option<String>,
    /// Only children of this task.
    pub parent: Option<TaskId>,
    /// Only tasks with this text in the title, the text, the plan, or a decision.
    pub text: Option<String>,
    /// Only tasks that are ready for the caller.
    pub ready: bool,
    /// Only tasks with active blockers.
    pub blocked: bool,
    /// The maximum count. Zero selects 50.
    pub limit: usize,
}

/// A row of a task list. It holds only the ID, type, status, stage, and title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskRow {
    /// The short ID.
    pub id: String,
    /// The type.
    #[serde(rename = "type")]
    pub kind: String,
    /// The status.
    pub status: Status,
    /// The stage.
    pub stage: String,
    /// The title.
    pub title: String,
}

impl TaskRow {
    fn of(meta: &TaskMeta) -> Self {
        Self {
            id: meta.id().short(),
            kind: meta.front.kind.clone(),
            status: meta.state.status,
            stage: meta.state.stage.clone(),
            title: meta.front.title.clone(),
        }
    }
}

/// A task with all the parts that a call asked for.
#[derive(Debug, Clone)]
pub struct TaskDetail {
    /// The index record.
    pub meta: TaskMeta,
    /// The description.
    pub description: String,
    /// The requirements.
    pub requirements: String,
    /// The acceptance criteria text.
    pub acceptance: String,
    /// The parsed acceptance criteria.
    pub criteria: Vec<Criterion>,
    /// The unknown sections.
    pub other_sections: Vec<(String, String)>,
    /// The plan, if the call asked for it.
    pub plan: Option<String>,
    /// The entries, if the call asked for them.
    pub entries: Vec<Entry>,
    /// The versions, if the call asked for them.
    pub versions: Vec<VersionMeta>,
    /// The artifacts, if the call asked for them.
    pub artifacts: Vec<ArtifactView>,
    /// The active claim.
    pub claim: Option<Claim>,
    /// The active blockers, also from parents.
    pub blockers: Vec<BlockerView>,
    /// The views of the links to other projects.
    pub soft_links: Vec<LinkView>,
    /// Warnings about this task.
    pub warnings: Vec<String>,
}

/// An active blocker of a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockerView {
    /// The short ID, or the full link of another project.
    pub id: String,
    /// The title, if lenka can show it.
    pub title: Option<String>,
    /// The status, if lenka can show it.
    pub status: Option<Status>,
    /// The ancestor that has this blocker. It is empty for the task itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via_parent: Option<String>,
}

/// The details of a linked artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactView {
    /// The short ID.
    pub id: String,
    /// The full ID.
    pub full_id: String,
    /// The file name.
    pub name: String,
    /// The media type.
    pub media_type: String,
    /// The current version.
    pub version: u64,
    /// The size in bytes.
    pub size: u64,
    /// The SHA-256 hash.
    pub sha256: String,
    /// The local path, if the store has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Tells if the bytes are here. A clone without Git LFS holds only a pointer file.
    pub downloaded: bool,
}

/// The neighbors of a task, for a search of related tasks and blockers.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RelatedView {
    /// The active blockers, also through other tasks.
    pub blockers: Vec<TaskRow>,
    /// The open tasks that wait for this task.
    pub blocks: Vec<TaskRow>,
    /// The parent.
    pub parent: Option<TaskRow>,
    /// The children.
    pub children: Vec<TaskRow>,
    /// The tasks with a `relates` link.
    pub relates: Vec<TaskRow>,
    /// The tasks with a similar title or labels, best first.
    pub similar: Vec<TaskRow>,
    /// The links to other projects.
    pub soft_links: Vec<LinkView>,
}

/// The findings of a project check.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DoctorReport {
    /// Tasks with invalid files, with the problem.
    pub invalid_files: Vec<String>,
    /// Directories with a name that is not a valid ID.
    pub bad_names: Vec<String>,
    /// Relation cycles.
    pub cycles: Vec<Vec<String>>,
    /// Links to other projects in the state `broken`.
    pub broken_links: Vec<String>,
    /// Links to other projects in the state `unavailable`.
    pub unavailable_links: Vec<String>,
    /// Tasks with a type or a stage that the settings do not have.
    pub missing_stages: Vec<String>,
    /// The reason that an unfinished write cannot complete. All writes stop until it can.
    pub unfinished_write: Option<String>,
}

impl DoctorReport {
    /// Tells if the check found nothing.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.invalid_files.is_empty()
            && self.bad_names.is_empty()
            && self.cycles.is_empty()
            && self.broken_links.is_empty()
            && self.unavailable_links.is_empty()
            && self.missing_stages.is_empty()
            && self.unfinished_write.is_none()
    }
}

/// The result of a stage change or a finish. The data is stored, also if a plugin failed.
#[derive(Debug, Clone)]
pub struct StateChange {
    /// The new task state.
    pub state: State,
    /// The failures of plugins after the write, for example a failed Git commit or push. The text
    /// can hold words from a remote, so a tool result must label it as untrusted data.
    pub warnings: Vec<String>,
}

/// A row for the desktop board: the full index record, with calculated values.
#[derive(Debug, Clone)]
pub struct BoardRow {
    /// The index record.
    pub meta: TaskMeta,
    /// Tells if the task has active blockers.
    pub blocked: bool,
    /// The holder of an active claim.
    pub claimed_by: Option<String>,
    /// The count of children.
    pub child_count: usize,
    /// The count of closed children.
    pub closed_children: usize,
    /// Tells if the files of this task are invalid.
    pub invalid: bool,
}

/// The change notices and warnings to add to a tool result.
#[derive(Debug, Clone, Default)]
pub struct CallNotes {
    /// The open change notices of the caller.
    pub unread: Vec<Notice>,
    /// The changes of soft link targets since the last call.
    pub cross_project: Vec<LinkView>,
}

/// The links from tasks to artifacts, during an artifact write.
#[derive(Debug, Clone, Copy)]
pub struct ArtifactRouting<'a> {
    index: &'a Index,
    now: Timestamp,
}

impl ArtifactRouting<'_> {
    /// Tells if a task links to the artifact.
    #[must_use]
    pub fn is_linked(&self, artifact: ArtifactId) -> bool {
        self.index
            .artifact_users
            .get(&artifact)
            .is_some_and(|users| !users.is_empty())
    }

    /// Creates the inbox operations for the claim holders that a change reaches.
    ///
    /// # Errors
    ///
    /// Returns an error if a notice cannot be formatted.
    pub fn notice_ops(
        &self,
        artifact: ArtifactId,
        version: u64,
        caller: &Caller,
        changed: &[String],
        reason: &str,
    ) -> Result<Vec<Op>> {
        let Some(users) = self.index.artifact_users.get(&artifact) else {
            return Ok(Vec::new());
        };
        let mut ops = Vec::new();
        for task in users {
            if let Some(claim) = self.index.active_claim(*task, self.now)
                && claim.agent_key != caller.agent_key
            {
                let notice = Notice {
                    id: NoticeId::generate(),
                    subject: Subject::Artifact(artifact.to_string()),
                    version,
                    changed: changed.to_vec(),
                    author: caller.label.clone(),
                    reason: input::clip(reason, input::MAX_REASON_BYTES).to_owned(),
                    at: self.now,
                    claim: claim.handle.clone(),
                    claimed_task: task.to_string(),
                };
                ops.push(Op::Append {
                    path: notices::inbox_path(&claim.agent_key)?.as_str().to_owned(),
                    line: notices::format_line(&notice)?,
                });
            }
        }
        Ok(ops)
    }
}

/// The versions of the tasks at one time. A sync compares them with the merged versions.
#[derive(Debug, Clone, Default)]
pub struct TaskVersions(HashMap<TaskId, u64>);

/// The task operations of one project.
#[derive(Debug)]
pub struct TaskService {
    project: Arc<Project>,
    index: Mutex<Index>,
    seen_links: Mutex<HashMap<Link, (u64, LinkState)>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn require_reason(reason: Option<&str>, what: &str) -> Result<String> {
    match reason.map(str::trim) {
        Some(text) if !text.is_empty() => Ok(text.to_owned()),
        _ => Err(LenkaError::new(
            ErrorCode::ReasonRequired,
            format!(
                "{what} is a material change, so it requires a reason. Add the `reason` parameter."
            ),
        )),
    }
}

fn check_field(what: &str, text: &str) -> Result<()> {
    check_text_size(what, text)?;
    input::check_text(what, text)?;
    crate::markdown::check_field_text(what, text)
}

fn check_plan(text: &str) -> Result<()> {
    check_text_size("The plan", text)?;
    input::check_text("The plan", text)?;
    crate::markdown::check_line_text("The plan", text)
}

/// Checks a reason. It goes into a TOML file and into notices.
fn check_reason(what: &str, text: &str) -> Result<()> {
    input::check_reason(what, text)?;
    crate::markdown::check_line_text(what, text)
}

fn check_evidence(text: &str) -> Result<()> {
    check_text_size("The evidence", text)?;
    input::check_text("The evidence", text)
}

fn check_list_size(what: &str, count: usize, limit: usize) -> Result<()> {
    if count > limit {
        return Err(LenkaError::invalid_argument(format!(
            "{what} has {count} items, and the limit is {limit}."
        )));
    }
    Ok(())
}

fn check_labels(labels: &[String]) -> Result<()> {
    check_list_size("The label list", labels.len(), MAX_LABELS_IN_CALL)?;
    labels
        .iter()
        .try_for_each(|label| input::check_short("A label", label.trim()))
}

/// Checks the sizes in a change before it takes any lock.
fn check_patch_input(patch: &TaskPatch) -> Result<()> {
    if let Some(reason) = &patch.reason {
        check_reason("The reason", reason)?;
    }
    check_labels(&patch.labels_add)?;
    check_list_size("The list of marks", patch.check.len(), MAX_CHECK_ITEMS)?;
    for (what, count) in [
        ("The list of labels to remove", patch.labels_remove.len()),
        ("The list of blockers to add", patch.blocked_by_add.len()),
        (
            "The list of blockers to remove",
            patch.blocked_by_remove.len(),
        ),
        ("The list of related tasks to add", patch.relates_add.len()),
        (
            "The list of related tasks to remove",
            patch.relates_remove.len(),
        ),
        ("The list of artifacts to link", patch.artifacts_add.len()),
        (
            "The list of artifacts to unlink",
            patch.artifacts_remove.len(),
        ),
    ] {
        check_list_size(what, count, MAX_LINKS_IN_CALL)?;
    }
    Ok(())
}

fn check_text_size(what: &str, text: &str) -> Result<()> {
    if text.len() > MAX_TEXT_BYTES {
        return Err(LenkaError::invalid_argument(format!(
            "{what} is larger than the limit of {MAX_TEXT_BYTES} bytes. Put long content in an artifact."
        )));
    }
    Ok(())
}

fn policy_for(caller: &Caller, policy: SecretPolicy) -> Result<SecretPolicy> {
    if policy == SecretPolicy::PersonOverride && caller.actor != Actor::Human {
        return Err(LenkaError::new(
            ErrorCode::ActorDenied,
            "Only a person can override the secret check.",
        ));
    }
    Ok(policy)
}

fn toml_text<T: Serialize>(value: &T) -> Result<String> {
    toml::to_string(value).map_err(|err| LenkaError::invalid_file(format!("TOML: {err}")))
}

fn put(path: String, text: String) -> Op {
    Op::Put { path, text }
}

fn feed_op(event: &FeedEvent) -> Result<Op> {
    Ok(Op::Append {
        path: FEED_PATH.to_owned(),
        line: notices::format_line(event)?,
    })
}

fn claim_path(id: TaskId) -> String {
    format!(".runtime/claims/{id}.toml")
}

fn loops_path(key: &str) -> String {
    format!(".runtime/loops/{key}.json")
}

fn root_of(id: TaskId) -> String {
    format!("tasks/{id}")
}

fn tokens(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.len() >= 3)
        .map(str::to_lowercase)
        .collect()
}

impl TaskService {
    /// Opens the service, loads the index, and records external edits that it finds.
    ///
    /// # Errors
    ///
    /// Returns an error if the data cannot be read.
    pub fn open(project: &Arc<Project>) -> Result<Self> {
        let (index, changed) = open_index(project)?;
        let service = Self {
            project: Arc::clone(project),
            index: Mutex::new(index),
            seen_links: Mutex::new(HashMap::new()),
        };
        service.record_external_edits(&changed)?;
        Ok(service)
    }

    /// Returns the project.
    #[must_use]
    pub fn project(&self) -> &Arc<Project> {
        &self.project
    }

    fn now(&self) -> Timestamp {
        self.project.clock().now()
    }

    fn stages_of(&self, kind: &str) -> Result<Vec<Stage>> {
        let config = self.project.config();
        config
            .types
            .get(kind)
            .map(|def| def.stages.clone())
            .ok_or_else(|| {
                LenkaError::invalid_file(format!(
                    "The type {kind} is not in lenka.toml. An admin must add the type or change the task."
                ))
            })
    }

    fn stage_position(&self, meta: &TaskMeta) -> Result<(Vec<Stage>, usize)> {
        let stages = self.stages_of(&meta.front.kind)?;
        let position = stages
            .iter()
            .position(|stage| stage.name == meta.state.stage)
            .ok_or_else(|| {
                LenkaError::invalid_file(format!(
                    "The task {} is on the stage {}, which its type does not have. An admin must move it.",
                    meta.id().short(),
                    meta.state.stage
                ))
            })?;
        Ok((stages, position))
    }

    /// Sends an event to the hooks. A hook can commit all data, so the hand edits become versions
    /// first.
    fn emit_after_edits(&self, event: &ProjectEvent) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.project.has_event_hooks()
            && let Err(error) = self.record_all_external_edits()
        {
            warnings.push(error.to_string());
        }
        warnings.extend(self.project.emit(event));
        warnings
    }

    /// Takes the lock, brings the index up to date, runs the work, and applies the result.
    fn with_write<T>(
        &self,
        work: impl FnOnce(&mut Locked<'_>, &mut Index, Timestamp) -> Result<T>,
    ) -> Result<T> {
        self.project.reload_config_if_changed();
        let mut index = lock(&self.index);
        let now = self.now();
        let driver = self.project.driver();
        let mut pending_error = None;
        let value = self.project.write_locked(|locked| {
            index.refresh(driver)?;
            let result = work(locked, &mut index, now);
            // A failed call can also have written data, as the loop limit does.
            index.refresh(driver)?;
            match result {
                Ok(value) => Ok(Some(value)),
                Err(error) => {
                    pending_error = Some(error);
                    Ok(None)
                }
            }
        })?;
        match (value, pending_error) {
            (Some(value), _) => Ok(value),
            (None, Some(error)) => Err(error),
            (None, None) => Err(LenkaError::new(ErrorCode::Io, "A write gave no result.")),
        }
    }

    /// Brings the index up to date, and tells if something changed.
    ///
    /// # Errors
    ///
    /// Returns an error if the feed cannot be read.
    pub fn refresh(&self) -> Result<bool> {
        let mut index = lock(&self.index);
        let before = index.feed_offset;
        index.refresh(self.project.driver())?;
        Ok(index.feed_offset != before)
    }

    fn read_index(&self) -> Result<MutexGuard<'_, Index>> {
        self.project.reload_config_if_changed();
        let mut index = lock(&self.index);
        index.refresh(self.project.driver())?;
        Ok(index)
    }

    /// Finds a task by its full ID or its short ID.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] or [`ErrorCode::AmbiguousId`].
    pub fn resolve(&self, text: &str) -> Result<TaskId> {
        let index = self.read_index()?;
        resolve_in(&index, text)
    }

    /// Finds a relation target: a task ID, a short ID, or `<project-id>/<task-id>`.
    ///
    /// # Errors
    ///
    /// Returns an error if the ID is not valid or not known.
    pub fn resolve_link(&self, text: &str) -> Result<Link> {
        if text.contains('/') {
            return Link::parse(text);
        }
        Ok(Link::Local(self.resolve(text)?))
    }

    // ---- Create and change ----

    /// Stores a new task.
    ///
    /// # Errors
    ///
    /// Returns an error if a field, a relation, or a text check fails.
    pub fn create(&self, caller: &Caller, new: NewTask) -> Result<TaskMeta> {
        let policy = policy_for(caller, new.secret_policy)?;
        let kind = if new.kind.is_empty() {
            "task".to_owned()
        } else {
            new.kind.clone()
        };
        let stages = self.stages_of(&kind)?;
        let title = clean_title(&new.title)?;
        check_labels(&new.labels)?;
        for (what, count) in [
            ("The list of blockers", new.blocked_by.len()),
            ("The list of related tasks", new.relates.len()),
            ("The list of artifacts", new.artifacts.len()),
        ] {
            check_list_size(what, count, MAX_LINKS_IN_CALL)?;
        }
        for (what, text) in [
            ("The description", &new.description),
            ("The requirements", &new.requirements),
            ("The acceptance criteria", &new.acceptance),
        ] {
            check_field(what, text)?;
        }
        if let Some(plan) = &new.plan {
            check_plan(plan)?;
        }
        check_dates(new.start, new.due)?;
        let status = new.status.unwrap_or(Status::Todo);
        if !matches!(status, Status::Backlog | Status::Todo) {
            return Err(LenkaError::invalid_argument(
                "A new task has the status backlog or todo.",
            ));
        }
        let id = TaskId::generate();
        self.with_write(|locked, index, now| {
            if let Some(parent) = new.parent
                && !index.tasks.contains_key(&parent)
            {
                return Err(LenkaError::not_found("The parent task does not exist."));
            }
            if let Some(parent) = new.parent {
                check_parent_open(index, parent)?;
            }
            self.check_links(index, id, &new.blocked_by, &new.relates)?;
            // A new task has no children, and only its ancestors wait for it. A blocker that waits
            // for the parent therefore makes a group of tasks wait for itself.
            if let Some(parent) = new.parent
                && new.blocked_by.iter().any(|link| {
                    link.local()
                        .is_some_and(|target| index.waits_for(target, parent))
                })
            {
                return Err(cycle_error());
            }
            for artifact in &new.artifacts {
                self.check_artifact_exists(*artifact)?;
            }
            let front = TaskFront {
                id,
                kind,
                title,
                parent: new.parent,
                blocked_by: new.blocked_by.clone(),
                relates: new.relates.clone(),
                artifacts: new.artifacts.clone(),
                priority: new.priority,
                labels: clean_labels(&new.labels),
                created_by: caller.label.clone(),
                created_at: now,
                start: new.start,
                due: new.due,
                coordinated: false,
            };
            let body = new_body(&new.description, &new.requirements, &new.acceptance);
            let task_text = join_front(&front, &body)?;
            check_text_size("The task", &task_text)?;
            let plan = new.plan.as_ref().filter(|plan| !plan.trim().is_empty());
            let state = State {
                version: 1,
                status,
                stage: stages[0].name.clone(),
                unblocked: false,
                loop_stopped: false,
                loop_until_done: false,
            };
            let meta = VersionMeta {
                version: 1,
                author: caller.label.clone(),
                at: now,
                reason: String::new(),
                changed: Vec::new(),
                material: false,
            };
            let mut ops = version_ops(id, 1, &task_text, plan.map(String::as_str), &meta)?;
            ops.push(put(
                format!("{}/state.toml", root_of(id)),
                toml_text(&state)?,
            ));
            ops.push(feed_op(&task_event(id, 1, FeedKind::Task, &meta))?);
            let mut checks = vec![
                TextCheck::new("title", front.title.clone()),
                TextCheck::new("description", new.description.clone()),
                TextCheck::new("requirements", new.requirements.clone()),
                TextCheck::new("acceptance criteria", new.acceptance.clone()),
                TextCheck::new("labels", front.labels.join("\n")),
            ];
            if let Some(plan) = plan {
                checks.push(TextCheck::new("plan", plan.clone()));
            }
            locked.commit(&Commit {
                checks,
                secret_policy: policy,
                ops,
            })?;
            index.refresh(self.project.driver())?;
            index
                .tasks
                .get(&id)
                .cloned()
                .ok_or_else(|| LenkaError::new(ErrorCode::Io, "A new task did not load."))
        })
    }

    fn check_artifact_exists(&self, id: ArtifactId) -> Result<()> {
        if read_artifact(self.project.as_ref(), id)?.is_none() {
            return Err(LenkaError::not_found(format!(
                "The artifact {} does not exist in this project.",
                id.short()
            )));
        }
        Ok(())
    }

    fn check_links(
        &self,
        index: &Index,
        id: TaskId,
        blocked_by: &[Link],
        relates: &[Link],
    ) -> Result<()> {
        for link in blocked_by.iter().chain(relates) {
            match *link {
                Link::Local(target) => {
                    if target == id {
                        return Err(LenkaError::new(
                            ErrorCode::Cycle,
                            "A task cannot link to itself.",
                        ));
                    }
                    if !index.tasks.contains_key(&target) {
                        return Err(LenkaError::not_found(format!(
                            "The linked task {} does not exist.",
                            target.short()
                        )));
                    }
                }
                Link::Remote(project, task) => {
                    if project == self.project.id() {
                        return Err(LenkaError::invalid_argument(
                            "A link inside one project uses only the task ID.",
                        ));
                    }
                    check_new_link(&self.project, project, task)?;
                }
            }
        }
        Ok(())
    }

    /// Changes a task.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::StaleVersion`] for an old version, [`ErrorCode::ClaimLost`] if another
    /// holder has the claim, and other codes for a field that is not valid.
    pub fn update(&self, caller: &Caller, id: TaskId, patch: TaskPatch) -> Result<TaskMeta> {
        check_patch_input(&patch)?;
        let policy = policy_for(caller, patch.secret_policy)?;
        if caller.actor != Actor::Human
            && (patch.kind.is_some()
                || patch.coordinated.is_some()
                || patch.loop_until_done.is_some())
        {
            return Err(LenkaError::new(
                ErrorCode::ActorDenied,
                "Only a person can change the type, the coordinator setting, or the loop mark of a task.",
            ));
        }
        self.with_write(|locked, index, now| {
            self.ingest_if_changed(locked, index, id, now)?;
            check_not_invalid(index, id)?;
            let meta = index.tasks.get(&id).cloned().ok_or_else(|| task_missing(id))?;
            check_not_closed(&meta)?;
            check_loop_not_stopped(&meta, caller)?;
            locked.check_version("The task", meta.state.version, patch.expected_version)?;
            check_write_claim(index, id, caller, &patch.claim, now)?;

            let loaded = load_task(self.project.driver(), id)?;
            let (mut front, old_body_text, old_plan) = (loaded.meta.front, loaded.body, loaded.plan);
            let old_body = Body::parse(&old_body_text);
            let mut body = old_body.clone();
            let mut plan = old_plan.clone();

            if let Some(title) = &patch.title {
                front.title = clean_title(title)?;
            }
            for (field, heading, what) in [
                (&patch.description, DESCRIPTION, "The description"),
                (&patch.requirements, REQUIREMENTS, "The requirements"),
                (&patch.acceptance, ACCEPTANCE, "The acceptance criteria"),
            ] {
                if let Some(text) = field {
                    check_field(what, text)?;
                    body.set_section(heading, text);
                }
            }
            let marks: BTreeMap<usize, bool> = patch
                .check
                .iter()
                .map(|item| (item.index, item.done))
                .collect();
            if let Some(index) = body.mark_criteria(&marks) {
                return Err(LenkaError::invalid_argument(format!(
                    "The task has no acceptance criterion at position {index}. Positions start at 0."
                )));
            }
            if let Some(text) = &patch.plan {
                check_plan(text)?;
                plan = (!text.trim().is_empty()).then(|| text.clone());
            }
            self.apply_relations(index, &mut front, &patch)?;
            apply_fields(&mut front, &patch)?;

            let mut new_state = meta.state.clone();
            if let Some(kind) = &patch.kind {
                let stages = self.stages_of(kind)?;
                let found = stages.iter().position(|stage| stage.name == new_state.stage);
                let position = found.unwrap_or(0);
                new_state.stage = stages[position].name.clone();
                if found.is_none() || *kind != meta.front.kind {
                    new_state.unblocked = stages[..position].iter().any(|stage| stage.unblocks);
                }
                front.kind.clone_from(kind);
            }
            let loop_mark = patch.loop_until_done == Some(true);
            if let Some(mark) = patch.loop_until_done {
                new_state.loop_until_done = mark;
            }
            if loop_mark {
                new_state.loop_stopped = false;
                if meta.state.loop_stopped && new_state.status == Status::Paused {
                    new_state.status = self.status_after_release(&meta);
                }
            }

            let mut changed = changed_parts(&old_body, &body, old_plan.as_deref(), plan.as_deref());
            if front.kind != meta.front.kind {
                changed.push("type".to_owned());
            }
            let body_text = if body == old_body { old_body_text.clone() } else { body.to_text() };
            let same_front = front == meta.front;
            if changed.is_empty()
                && same_front
                && new_state == meta.state
                && body == old_body
                && plan == old_plan
            {
                return Ok(meta);
            }
            let material = !changed.is_empty();
            let reason = if material {
                require_reason(patch.reason.as_deref(), "This change")?
            } else {
                patch.reason.clone().unwrap_or_default()
            };
            if material
                && plan != old_plan
                && let Some(coordinator) = coordinator_of(index, id)
            {
                check_coordinator(index, coordinator, caller, &patch.claim, now)?;
            }
            let counts_for_loop =
                material && caller.actor == Actor::Agent && !meta.state.loop_until_done;
            if counts_for_loop {
                let limit = self.project.config().limits.loop_limit;
                let recent = read_loop_counts(self.project.driver(), &id.to_string(), now)?;
                if recent.len() >= limit as usize {
                    let mut stopped = meta.state.clone();
                    stopped.loop_stopped = true;
                    let ops = self.pause_ops(index, id, stopped, caller, now)?;
                    locked.commit(&Commit {
                        ops,
                        ..Commit::default()
                    })?;
                    return Err(LenkaError::new(
                        ErrorCode::LoopLimit,
                        format!(
                            "This task reached the limit of {limit} material agent changes in one hour, so lenka paused it. A person must act."
                        ),
                    ));
                }
            }

            let version = meta.state.version + 1;
            new_state.version = version;
            let task_text = join_front(&front, &body_text)?;
            check_text_size("The task", &task_text)?;
            let version_meta = VersionMeta {
                version,
                author: caller.label.clone(),
                at: now,
                reason: reason.clone(),
                changed: changed.clone(),
                material,
            };
            let mut ops = version_ops(id, version, &task_text, plan.as_deref(), &version_meta)?;
            if loop_mark {
                ops.push(Op::Remove {
                    path: loops_path(&id.to_string()),
                });
            }
            if let Some(active) = index.claims.get(&id)
                && active.stage != new_state.stage
            {
                let mut moved = active.clone();
                moved.stage.clone_from(&new_state.stage);
                ops.push(put(claim_path(id), toml_text(&moved)?));
                ops.push(feed_op(&simple_event(FeedKind::Claim, id, version, caller, now))?);
            }
            if plan.is_none() && old_plan.is_some() {
                ops.push(Op::Remove {
                    path: format!("{}/plan.md", root_of(id)),
                });
            }
            ops.push(put(format!("{}/state.toml", root_of(id)), toml_text(&new_state)?));
            ops.push(feed_op(&task_event(id, version, FeedKind::Task, &version_meta))?);
            if material {
                ops.extend(notice_ops(
                    index,
                    id,
                    Subject::Task(id.to_string()),
                    caller,
                    &version_meta,
                    now,
                )?);
                if counts_for_loop {
                    ops.push(loop_count_op(self.project.driver(), &id.to_string(), now)?);
                }
            }
            let mut checks = vec![
                TextCheck::new("title", front.title.clone()),
                TextCheck::new("labels", front.labels.join("\n")),
            ];
            for (name, text) in [
                ("description", patch.description.as_ref()),
                ("requirements", patch.requirements.as_ref()),
                ("acceptance criteria", patch.acceptance.as_ref()),
                ("plan", patch.plan.as_ref()),
                ("reason", patch.reason.as_ref()),
            ] {
                if let Some(text) = text {
                    checks.push(TextCheck::new(name, text.clone()));
                }
            }
            locked.commit(&Commit {
                checks,
                secret_policy: policy,
                ops,
            })?;
            index.refresh(self.project.driver())?;
            index.tasks.get(&id).cloned().ok_or_else(|| task_missing(id))
        })
    }

    fn apply_relations(
        &self,
        index: &mut Index,
        front: &mut TaskFront,
        patch: &TaskPatch,
    ) -> Result<()> {
        let id = front.id;
        if let Some(parent) = patch.parent {
            if let Some(target) = parent {
                if !index.tasks.contains_key(&target) {
                    return Err(LenkaError::not_found("The parent task does not exist."));
                }
                check_parent_open(index, target)?;
                if index.parent_makes_cycle(id, target) {
                    return Err(cycle_error());
                }
            }
            front.parent = parent;
        }
        self.check_links(index, id, &patch.blocked_by_add, &patch.relates_add)?;
        for link in &patch.blocked_by_add {
            if let Link::Local(target) = link {
                // A child waits for all blockers of its parent, and a parent waits for its open
                // children. The second check covers a new parent in the same patch.
                let new_parent = patch.parent.flatten();
                if index.blocker_makes_cycle(id, *target)
                    || new_parent.is_some_and(|parent| index.waits_for(*target, parent))
                {
                    return Err(cycle_error());
                }
            }
        }
        update_list(
            &mut front.blocked_by,
            &patch.blocked_by_add,
            &patch.blocked_by_remove,
        );
        update_list(
            &mut front.relates,
            &patch.relates_add,
            &patch.relates_remove,
        );
        for artifact in &patch.artifacts_add {
            self.check_artifact_exists(*artifact)?;
        }
        update_list(
            &mut front.artifacts,
            &patch.artifacts_add,
            &patch.artifacts_remove,
        );
        Ok(())
    }

    fn pause_ops(
        &self,
        index: &Index,
        id: TaskId,
        mut state: State,
        caller: &Caller,
        now: Timestamp,
    ) -> Result<Vec<Op>> {
        state.status = Status::Paused;
        let mut ops = vec![put(
            format!("{}/state.toml", root_of(id)),
            toml_text(&state)?,
        )];
        if index.claims.contains_key(&id) {
            ops.push(Op::Remove {
                path: claim_path(id),
            });
            ops.push(feed_op(&simple_event(
                FeedKind::Claim,
                id,
                state.version,
                caller,
                now,
            ))?);
        }
        ops.push(feed_op(&simple_event(
            FeedKind::State,
            id,
            state.version,
            caller,
            now,
        ))?);
        Ok(ops)
    }

    /// Records a material agent change of an artifact, and checks the loop limit.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::LoopLimit`] if the limit is reached.
    pub fn check_artifact_loop(&self, caller: &Caller, artifact: ArtifactId) -> Result<Option<Op>> {
        if caller.actor != Actor::Agent {
            return Ok(None);
        }
        {
            let index = self.read_index()?;
            let marked = index.artifact_users.get(&artifact).is_some_and(|users| {
                users.iter().any(|task| {
                    index
                        .tasks
                        .get(task)
                        .is_some_and(|meta| meta.state.loop_until_done)
                })
            });
            if marked {
                return Ok(None);
            }
        }
        let key = &artifact.to_string();
        let limit = self.project.config().limits.loop_limit;
        let now = self.now();
        let recent = read_loop_counts(self.project.driver(), key, now)?;
        if recent.len() >= limit as usize {
            return Err(LenkaError::new(
                ErrorCode::LoopLimit,
                format!(
                    "This artifact reached the limit of {limit} material agent changes in one hour. Wait, or ask a person to make the change."
                ),
            ));
        }
        loop_count_op(self.project.driver(), key, now).map(Some)
    }

    // ---- Read ----

    /// Reads a task, and closes the notices that this read answers.
    ///
    /// `parts` can hold `plan`, `entries`, `artifacts`, and `versions`.
    ///
    /// # Errors
    ///
    /// Returns an error if the task does not exist or its files are invalid.
    pub fn show(&self, caller: &Caller, id: TaskId, parts: &[&str]) -> Result<TaskDetail> {
        let detail = self.detail(id, parts)?;
        if let Some(reason) = lock(&self.index).invalid.get(&id) {
            return Err(LenkaError::invalid_file(reason.clone()));
        }
        let mut read = vec![(Subject::Task(id.to_string()), detail.meta.state.version)];
        for artifact in &detail.artifacts {
            read.push((
                Subject::Artifact(artifact.full_id.clone()),
                artifact.version,
            ));
        }
        self.close_notices(caller, &read)?;
        Ok(detail)
    }

    /// Reads a task with no effect on notices. The desktop app uses this for its preview.
    ///
    /// # Errors
    ///
    /// Returns an error if the task does not exist.
    pub fn detail(&self, id: TaskId, parts: &[&str]) -> Result<TaskDetail> {
        self.ingest_external_on_read(id)?;
        let now = self.now();
        let driver = self.project.driver();
        let index = self.read_index()?;
        let Some(meta) = index.tasks.get(&id).cloned() else {
            return Err(match index.invalid.get(&id) {
                Some(reason) => LenkaError::invalid_file(reason.clone()),
                None => task_missing(id),
            });
        };
        let mut warnings = Vec::new();
        if let Some(reason) = index.invalid.get(&id) {
            warnings.push(format!("The files of this task are invalid: {reason}"));
        }
        let loaded = match load_task(driver, id) {
            Ok(loaded) => loaded,
            Err(error) if error.code() == ErrorCode::InvalidFile => read_last_valid(driver, &meta)
                .unwrap_or(LoadedTask {
                    meta: meta.clone(),
                    body: String::new(),
                    plan: None,
                }),
            Err(error) => return Err(error),
        };
        let body = Body::parse(&loaded.body);
        let mut reader = LinkReader::default();
        let blockers = active_blockers(&index, &self.project, id, &mut reader);
        let soft_links: Vec<LinkView> = meta
            .front
            .blocked_by
            .iter()
            .chain(&meta.front.relates)
            .filter(|link| matches!(link, Link::Remote(..)))
            .map(|link| reader.view(&self.project, *link))
            .collect();
        for view in &soft_links {
            match view.state {
                LinkState::Unavailable => warnings.push(format!(
                    "The link {} is unavailable. A blocker on it still blocks.",
                    view.link
                )),
                LinkState::Broken => warnings.push(format!(
                    "The link {} is broken. It no longer blocks.",
                    view.link
                )),
                LinkState::Ok => {}
            }
        }
        let claim = index.active_claim(id, now).cloned();
        let mut detail = TaskDetail {
            description: body.section(DESCRIPTION).unwrap_or("").to_owned(),
            requirements: body.section(REQUIREMENTS).unwrap_or("").to_owned(),
            acceptance: body.section(ACCEPTANCE).unwrap_or("").to_owned(),
            criteria: body.criteria(),
            other_sections: body.other_sections(),
            plan: None,
            entries: Vec::new(),
            versions: Vec::new(),
            artifacts: Vec::new(),
            claim,
            blockers,
            soft_links,
            warnings,
            meta,
        };
        drop(index);
        if parts.contains(&"plan") {
            detail.plan = loaded.plan;
        }
        if parts.contains(&"entries") {
            detail.entries = read_entries(driver, id)?;
        }
        if parts.contains(&"versions") {
            detail.versions = read_versions(driver, id)?;
        }
        if parts.contains(&"artifacts") {
            for artifact in &detail.meta.front.artifacts {
                // One bad artifact file must not stop the read of the task.
                match read_artifact(self.project.as_ref(), *artifact) {
                    Ok(Some(record)) => detail
                        .artifacts
                        .push(artifact_view(self.project.as_ref(), &record)),
                    Ok(None) => {}
                    Err(error) => detail.warnings.push(error.message().to_owned()),
                }
            }
        }
        Ok(detail)
    }

    /// Reads the text of an old version: `task.md` and `plan.md`.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] if the version does not exist.
    pub fn read_version(&self, id: TaskId, version: u64) -> Result<(String, Option<String>)> {
        let driver = self.project.driver();
        let root = format!("{}/versions/{version:06}", root_of(id));
        let text = read_text(driver, &format!("{root}/task.md"))?
            .ok_or_else(|| LenkaError::not_found(format!("The task has no version {version}.")))?;
        Ok((text, read_text(driver, &format!("{root}/plan.md"))?))
    }

    /// Filters tasks. The result holds only the ID, type, status, stage, and title.
    ///
    /// # Errors
    ///
    /// Returns an error if the data cannot be read.
    pub fn list(&self, caller: &Caller, filter: &ListFilter) -> Result<Vec<TaskRow>> {
        let now = self.now();
        let index = self.read_index()?;
        let limit = if filter.limit == 0 { 50 } else { filter.limit };
        let mut reader = LinkReader::default();
        let mut matches: Vec<&TaskMeta> = Vec::new();
        let text = filter.text.as_deref().map(str::to_lowercase);
        let base = |meta: &&TaskMeta| {
            (filter.statuses.is_empty() || filter.statuses.contains(&meta.state.status))
                && filter
                    .kind
                    .as_ref()
                    .is_none_or(|kind| *kind == meta.front.kind)
                && filter
                    .label
                    .as_ref()
                    .is_none_or(|label| meta.front.labels.contains(label))
                && filter
                    .parent
                    .is_none_or(|parent| meta.front.parent == Some(parent))
        };
        let candidates: Vec<&TaskMeta> = index
            .order
            .iter()
            .filter_map(|id| index.tasks.get(id))
            .filter(base)
            .collect();
        let candidates = match &text {
            Some(text) => search_tasks(self.project.driver(), candidates, text),
            None => candidates,
        };
        for meta in candidates {
            if filter.ready && !self.is_ready(&index, meta, caller.actor, now, &mut reader) {
                continue;
            }
            if filter.blocked && !is_blocked(&index, &self.project, meta.id(), &mut reader) {
                continue;
            }
            matches.push(meta);
        }
        if filter.ready {
            matches.sort_by_key(|meta| (std::cmp::Reverse(meta.front.priority), meta.id()));
        }
        Ok(matches.into_iter().take(limit).map(TaskRow::of).collect())
    }

    fn is_ready(
        &self,
        index: &Index,
        meta: &TaskMeta,
        actor: Actor,
        now: Timestamp,
        reader: &mut LinkReader,
    ) -> bool {
        let id = meta.id();
        let status_ok = match meta.state.status {
            Status::Todo => true,
            Status::InProgress => index.active_claim(id, now).is_none(),
            _ => false,
        };
        if !status_ok || index.invalid.contains_key(&id) {
            return false;
        }
        let Ok((stages, position)) = self.stage_position(meta) else {
            return false;
        };
        actor.can_do(stages[position].actor)
            && !has_open_children(index, id)
            && !is_blocked(index, &self.project, id, reader)
    }

    /// Returns all tasks with calculated values, for the desktop app.
    ///
    /// # Errors
    ///
    /// Returns an error if the data cannot be read.
    pub fn board_rows(&self) -> Result<Vec<BoardRow>> {
        let now = self.now();
        let index = self.read_index()?;
        let mut reader = LinkReader::default();
        Ok(index
            .order
            .iter()
            .filter_map(|id| index.tasks.get(id))
            .map(|meta| {
                let id = meta.id();
                let children = index.children.get(&id);
                BoardRow {
                    meta: meta.clone(),
                    blocked: is_blocked(&index, &self.project, id, &mut reader),
                    claimed_by: index
                        .active_claim(id, now)
                        .map(|claim| claim.holder.clone()),
                    child_count: children.map_or(0, BTreeSet::len),
                    closed_children: children.map_or(0, |items| {
                        items
                            .iter()
                            .filter(|child| {
                                index
                                    .tasks
                                    .get(child)
                                    .is_some_and(|meta| meta.state.status.is_closed())
                            })
                            .count()
                    }),
                    invalid: index.invalid.contains_key(&id),
                }
            })
            .collect())
    }

    /// Returns related tasks and blockers.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] if the task does not exist.
    pub fn related(&self, id: TaskId) -> Result<RelatedView> {
        let index = self.read_index()?;
        let meta = index.tasks.get(&id).ok_or_else(|| task_missing(id))?;
        let row = |task: &TaskId| index.tasks.get(task).map(TaskRow::of);
        let mut reader = LinkReader::default();
        let mut view = RelatedView {
            parent: meta.front.parent.as_ref().and_then(row),
            ..RelatedView::default()
        };

        // A walk through active blockers, also of parents, which also finds indirect blockers.
        let mut seen = HashSet::new();
        let mut stack = vec![id];
        while let Some(current) = stack.pop() {
            if !seen.insert(current) {
                continue;
            }
            let Some(item) = index.tasks.get(&current) else {
                continue;
            };
            stack.extend(item.front.parent);
            for link in &item.front.blocked_by {
                if let Link::Local(target) = link
                    && local_blocks(&index, *target)
                {
                    stack.push(*target);
                    if let Some(found) = row(target)
                        && !view.blockers.contains(&found)
                    {
                        view.blockers.push(found);
                    }
                }
            }
        }
        if let Some(items) = index.blocks.get(&id) {
            view.blocks = items
                .iter()
                .filter_map(|task| index.tasks.get(task))
                .filter(|item| !item.state.status.is_closed())
                .map(TaskRow::of)
                .collect();
        }
        if let Some(items) = index.children.get(&id) {
            view.children = items.iter().filter_map(row).collect();
        }
        let mut relates: BTreeSet<TaskId> = meta
            .front
            .relates
            .iter()
            .filter_map(|link| link.local())
            .collect();
        if let Some(items) = index.related_from.get(&id) {
            relates.extend(items);
        }
        view.relates = relates.iter().filter_map(row).collect();
        view.soft_links = meta
            .front
            .blocked_by
            .iter()
            .chain(&meta.front.relates)
            .filter(|link| matches!(link, Link::Remote(..)))
            .map(|link| reader.view(&self.project, *link))
            .collect();

        let mut words = tokens(&meta.front.title);
        if let Ok(loaded) = load_task(self.project.driver(), id) {
            words.extend(tokens(&loaded.body));
        }
        let neighbors = index.neighbors(id);
        let mut scored: Vec<(usize, TaskId)> = index
            .tasks
            .values()
            .filter(|item| item.id() != id && !neighbors.contains(&item.id()))
            .filter_map(|item| {
                let title_score = tokens(&item.front.title)
                    .iter()
                    .filter(|word| words.contains(*word))
                    .count();
                let label_score = item
                    .front
                    .labels
                    .iter()
                    .filter(|label| meta.front.labels.contains(*label))
                    .count();
                let score = title_score * 2 + label_score * 3;
                (score > 0).then_some((score, item.id()))
            })
            .collect();
        scored.sort_by_key(|(score, task)| (std::cmp::Reverse(*score), *task));
        view.similar = scored
            .iter()
            .take(10)
            .filter_map(|(_, task)| row(task))
            .collect();
        Ok(view)
    }

    // ---- Claims ----

    /// Takes a task, or the next ready task. A repeat call with the handle prolongs the claim.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ClaimLost`], [`ErrorCode::Blocked`], [`ErrorCode::ActorDenied`], or
    /// [`ErrorCode::NotFound`] if no task is ready.
    pub fn claim(
        &self,
        caller: &Caller,
        task: Option<TaskId>,
        handle: Option<&str>,
        ttl_minutes: Option<u32>,
    ) -> Result<Claim> {
        let default_minutes = self.project.config().limits.claim_timeout_minutes;
        let minutes = i64::from(ttl_minutes.unwrap_or(default_minutes).clamp(1, 24 * 60));
        self.with_write(|locked, index, now| {
            let expires_at = now.plus_ms(minutes * MS_PER_MINUTE);
            if let Some(handle) = handle {
                let found = index
                    .claims
                    .values()
                    .find(|claim| claim.handle == handle && claim.expires_at > now)
                    .cloned()
                    .filter(|claim| task.is_none_or(|id| id == claim.task))
                    .ok_or_else(claim_lost)?;
                return self.write_prolong(locked, index, found, caller, expires_at, now);
            }
            let mut reader = LinkReader::default();
            let id = match task {
                Some(id) => {
                    self.ingest_if_changed(locked, index, id, now)?;
                    self.check_can_claim(index, id, caller, now, &mut reader)?;
                    if let Some(existing) = index.active_claim(id, now).cloned() {
                        return self
                            .write_prolong(locked, index, existing, caller, expires_at, now);
                    }
                    id
                }
                None => {
                    let id = self
                        .next_ready(index, caller.actor, now, &mut reader)
                        .ok_or_else(|| LenkaError::not_found("No task is ready for this actor."))?;
                    self.ingest_if_changed(locked, index, id, now)?;
                    self.check_can_claim(index, id, caller, now, &mut reader)?;
                    id
                }
            };
            let meta = index
                .tasks
                .get(&id)
                .cloned()
                .ok_or_else(|| task_missing(id))?;
            let claim = Claim {
                task: id,
                stage: meta.state.stage.clone(),
                handle: uuid::Uuid::new_v4().simple().to_string(),
                holder: caller.label.clone(),
                actor: caller.actor,
                worktree: caller.worktree.display().to_string(),
                agent_key: caller.agent_key.clone(),
                claimed_at: now,
                expires_at,
            };
            let mut ops = vec![put(claim_path(id), toml_text(&claim)?)];
            ops.push(feed_op(&simple_event(
                FeedKind::Claim,
                id,
                meta.state.version,
                caller,
                now,
            ))?);
            let mut state = meta.state.clone();
            state.status = Status::InProgress;
            // A claim by a person is the action that the loop limit waits for.
            state.loop_stopped = state.loop_stopped && caller.actor != Actor::Human;
            if meta.state.loop_stopped && !state.loop_stopped {
                ops.push(Op::Remove {
                    path: loops_path(&id.to_string()),
                });
            }
            if state != meta.state {
                ops.push(put(
                    format!("{}/state.toml", root_of(id)),
                    toml_text(&state)?,
                ));
                ops.push(feed_op(&simple_event(
                    FeedKind::State,
                    id,
                    state.version,
                    caller,
                    now,
                ))?);
            }
            locked.commit(&Commit {
                ops,
                ..Commit::default()
            })?;
            Ok(claim)
        })
    }

    fn write_prolong(
        &self,
        locked: &mut Locked<'_>,
        index: &Index,
        mut claim: Claim,
        caller: &Caller,
        expires_at: Timestamp,
        now: Timestamp,
    ) -> Result<Claim> {
        claim.expires_at = expires_at;
        let version = index
            .tasks
            .get(&claim.task)
            .map_or(0, |meta| meta.state.version);
        locked.commit(&Commit {
            ops: vec![
                put(claim_path(claim.task), toml_text(&claim)?),
                feed_op(&simple_event(
                    FeedKind::Claim,
                    claim.task,
                    version,
                    caller,
                    now,
                ))?,
            ],
            ..Commit::default()
        })?;
        Ok(claim)
    }

    /// Returns the status of a task with no holder that is not paused.
    fn status_after_release(&self, meta: &TaskMeta) -> Status {
        match self.stage_position(meta) {
            Ok((_, position)) if position > 0 => Status::InProgress,
            _ => Status::Todo,
        }
    }

    fn check_can_claim(
        &self,
        index: &Index,
        id: TaskId,
        caller: &Caller,
        now: Timestamp,
        reader: &mut LinkReader,
    ) -> Result<()> {
        check_not_invalid(index, id)?;
        let meta = index.tasks.get(&id).ok_or_else(|| task_missing(id))?;
        if meta.state.status.is_closed() {
            return Err(LenkaError::invalid_argument(format!(
                "The task {} is {}, so nobody can claim it.",
                id.short(),
                meta.state.status.as_str()
            )));
        }
        check_loop_not_stopped(meta, caller)?;
        if let Some(existing) = index.active_claim(id, now)
            && existing.agent_key != caller.agent_key
        {
            return Err(LenkaError::new(
                ErrorCode::ClaimLost,
                format!(
                    "{} holds this task until {}. Select another task.",
                    existing.holder, existing.expires_at
                ),
            ));
        }
        let (stages, position) = self.stage_position(meta)?;
        let stage = &stages[position];
        if !caller.actor.can_do(stage.actor) {
            return Err(LenkaError::new(
                ErrorCode::ActorDenied,
                format!(
                    "The stage {} needs the actor {:?}. A person completes a human stage with the CLI or the desktop app.",
                    stage.name, stage.actor
                ),
            ));
        }
        // A coordinator holds the claim of a parent task while workers do its children.
        if !meta.front.coordinated && has_open_children(index, id) {
            return Err(LenkaError::new(
                ErrorCode::Blocked,
                "The task has open children. Complete or cancel them first.",
            ));
        }
        let blockers = active_blockers(index, &self.project, id, reader);
        if !blockers.is_empty() {
            let names: Vec<&str> = blockers.iter().map(|item| item.id.as_str()).collect();
            return Err(LenkaError::new(
                ErrorCode::Blocked,
                format!("The task has active blockers: {}.", names.join(", ")),
            ));
        }
        Ok(())
    }

    fn next_ready(
        &self,
        index: &Index,
        actor: Actor,
        now: Timestamp,
        reader: &mut LinkReader,
    ) -> Option<TaskId> {
        index
            .order
            .iter()
            .filter_map(|id| index.tasks.get(id))
            .filter(|meta| self.is_ready(index, meta, actor, now, reader))
            .min_by_key(|meta| (std::cmp::Reverse(meta.front.priority), meta.id()))
            .map(TaskMeta::id)
    }

    /// Finishes the current stage with evidence, or returns to an earlier stage with a reason.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ClaimLost`], [`ErrorCode::UnreadChanges`], [`ErrorCode::ActorDenied`],
    /// or [`ErrorCode::InvalidArgument`] for the last stage.
    pub fn advance(
        &self,
        caller: &Caller,
        id: TaskId,
        claim: &ClaimRef,
        evidence: &str,
        back_to: Option<&str>,
    ) -> Result<StateChange> {
        check_evidence(evidence)?;
        let state = self.with_write(|locked, index, now| {
            self.ingest_if_changed(locked, index, id, now)?;
            check_not_invalid(index, id)?;
            let meta = index
                .tasks
                .get(&id)
                .cloned()
                .ok_or_else(|| task_missing(id))?;
            check_not_closed(&meta)?;
            let (stages, position) = self.stage_position(&meta)?;
            let active = check_stage_claim(index, id, caller, claim, now)?;
            if !caller.actor.can_do(stages[position].actor) && caller.actor != Actor::Human {
                return Err(LenkaError::new(
                    ErrorCode::ActorDenied,
                    format!("The stage {} needs a person.", stages[position].name),
                ));
            }
            if let Some(active) = &active {
                check_unread(index, self.project.driver(), active)?;
            }
            if evidence.trim().is_empty() {
                return Err(LenkaError::invalid_argument(match back_to {
                    Some(_) => "A return to an earlier stage requires a reason in `evidence`.",
                    None => {
                        "A stage completion requires evidence, such as a command and its result."
                    }
                }));
            }
            let (next, unblocked) = match back_to {
                Some(name) => {
                    let target = stages
                        .iter()
                        .position(|stage| stage.name == name)
                        .ok_or_else(|| {
                            LenkaError::invalid_argument(format!("The type has no stage {name}."))
                        })?;
                    if target >= position {
                        return Err(LenkaError::invalid_argument(
                            "`back_to` must name an earlier stage.",
                        ));
                    }
                    let unblocked = stages[..target].iter().any(|stage| stage.unblocks);
                    (target, unblocked)
                }
                None => {
                    if position + 1 >= stages.len() {
                        return Err(LenkaError::invalid_argument(
                            "This is the last stage. Call task.finish to complete the task.",
                        ));
                    }
                    (
                        position + 1,
                        meta.state.unblocked || stages[position].unblocks,
                    )
                }
            };
            let mut state = meta.state.clone();
            state.stage = stages[next].name.clone();
            state.unblocked = unblocked;
            state.status = Status::InProgress;
            let entry = Entry {
                id: EntryId::generate(),
                kind: EntryKind::Evidence,
                author: caller.label.clone(),
                at: now,
                stage: Some(stages[position].name.clone()),
                replaces: None,
                text: evidence.to_owned(),
            };
            let mut ops = vec![entry_op(id, &entry)?];
            ops.push(put(
                format!("{}/state.toml", root_of(id)),
                toml_text(&state)?,
            ));
            ops.push(Op::Remove {
                path: claim_path(id),
            });
            ops.push(feed_op(&simple_event(
                FeedKind::Claim,
                id,
                state.version,
                caller,
                now,
            ))?);
            ops.push(feed_op(&simple_event(
                FeedKind::State,
                id,
                state.version,
                caller,
                now,
            ))?);
            locked.commit(&Commit {
                checks: vec![TextCheck::new("evidence", evidence)],
                ops,
                ..Commit::default()
            })?;
            Ok(state)
        })?;
        let warnings = self.emit_after_edits(&ProjectEvent::StageChanged {
            task: id.to_string(),
        });
        Ok(StateChange { state, warnings })
    }

    /// Finishes a task with evidence, or releases it.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ClaimLost`], [`ErrorCode::UnreadChanges`], [`ErrorCode::ActorDenied`],
    /// or [`ErrorCode::InvalidArgument`] for missing evidence or an unchecked criterion.
    pub fn finish(
        &self,
        caller: &Caller,
        id: TaskId,
        claim: &ClaimRef,
        finish: &Finish,
    ) -> Result<StateChange> {
        if let Finish::Done {
            evidence,
            unchecked_reason,
        } = finish
        {
            check_evidence(evidence)?;
            if let Some(reason) = unchecked_reason {
                check_reason("The reason", reason)?;
            }
        }
        let state = self.with_write(|locked, index, now| {
            self.ingest_if_changed(locked, index, id, now)?;
            check_not_invalid(index, id)?;
            let meta = index.tasks.get(&id).cloned().ok_or_else(|| task_missing(id))?;
            check_not_closed(&meta)?;
            let (stages, position) = self.stage_position(&meta)?;
            let active = check_stage_claim(index, id, caller, claim, now)?;
            let mut state = meta.state.clone();
            let mut ops = Vec::new();
            let mut checks = Vec::new();
            match finish {
                Finish::Released => {
                    if active.is_none() && !matches!(claim, ClaimRef::Override) {
                        return Err(claim_lost());
                    }
                    state.status = self.status_after_release(&meta);
                }
                Finish::Done { evidence, unchecked_reason } => {
                    if let Some(active) = &active {
                        check_unread(index, self.project.driver(), active)?;
                    }
                    if !caller.actor.can_do(stages[position].actor) && caller.actor != Actor::Human {
                        return Err(LenkaError::new(
                            ErrorCode::ActorDenied,
                            format!("The stage {} needs a person.", stages[position].name),
                        ));
                    }
                    if caller.actor != Actor::Human && position + 1 < stages.len() {
                        return Err(LenkaError::invalid_argument(format!(
                            "The task is on the stage {}, and later stages remain. Call task.advance first.",
                            stages[position].name
                        )));
                    }
                    if evidence.trim().is_empty() {
                        return Err(LenkaError::invalid_argument(
                            "`done` requires evidence, such as a command and its result.",
                        ));
                    }
                    if has_open_children(index, id) {
                        return Err(LenkaError::new(
                            ErrorCode::Blocked,
                            "The task has open children. Complete or cancel them first.",
                        ));
                    }
                    let loaded = load_task(self.project.driver(), id)?;
                    let unchecked = Body::parse(&loaded.body)
                        .criteria()
                        .iter()
                        .filter(|item| !item.done)
                        .count();
                    let reason = unchecked_reason.as_deref().map(str::trim).unwrap_or("");
                    if unchecked > 0 && reason.is_empty() {
                        return Err(LenkaError::invalid_argument(format!(
                            "{unchecked} acceptance criteria are not checked. Check them with task.update, or give `unchecked_reason`."
                        )));
                    }
                    let mut text = evidence.clone();
                    if unchecked > 0 {
                        text.push_str("\n\nUnchecked criteria: ");
                        text.push_str(reason);
                    }
                    checks.push(TextCheck::new("evidence", text.clone()));
                    ops.push(entry_op(
                        id,
                        &Entry {
                            id: EntryId::generate(),
                            kind: EntryKind::Evidence,
                            author: caller.label.clone(),
                            at: now,
                            stage: Some(stages[position].name.clone()),
                            replaces: None,
                            text,
                        },
                    )?);
                    state.status = Status::Done;
                    state.unblocked = true;
                    state.loop_stopped = false;
                    state.loop_until_done = false;
                }
            }
            ops.push(put(format!("{}/state.toml", root_of(id)), toml_text(&state)?));
            if index.claims.contains_key(&id) {
                ops.push(Op::Remove { path: claim_path(id) });
                ops.push(feed_op(&simple_event(FeedKind::Claim, id, state.version, caller, now))?);
            }
            ops.push(feed_op(&simple_event(FeedKind::State, id, state.version, caller, now))?);
            locked.commit(&Commit {
                checks,
                ops,
                ..Commit::default()
            })?;
            Ok(state)
        })?;
        let warnings = if state.status == Status::Done {
            self.emit_after_edits(&ProjectEvent::TaskFinished {
                task: id.to_string(),
            })
        } else {
            Vec::new()
        };
        Ok(StateChange { state, warnings })
    }

    /// Ends the claim, and stops automatic pickup.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ClaimLost`] if another holder has the claim.
    pub fn pause(&self, caller: &Caller, id: TaskId, claim: &ClaimRef) -> Result<State> {
        self.with_write(|locked, index, now| {
            self.ingest_if_changed(locked, index, id, now)?;
            check_not_invalid(index, id)?;
            let meta = index
                .tasks
                .get(&id)
                .cloned()
                .ok_or_else(|| task_missing(id))?;
            check_write_claim(index, id, caller, claim, now)?;
            check_not_closed(&meta)?;
            let ops = self.pause_ops(index, id, meta.state.clone(), caller, now)?;
            locked.commit(&Commit {
                ops,
                ..Commit::default()
            })?;
            let mut state = meta.state;
            state.status = Status::Paused;
            Ok(state)
        })
    }

    /// Sets a status directly. Only a person can do this. It also ends the loop limit pause.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ActorDenied`] for an agent, and [`ErrorCode::ClaimLost`] if another
    /// holder has the claim and the call has no override.
    pub fn set_status(
        &self,
        caller: &Caller,
        id: TaskId,
        status: Status,
        claim: &ClaimRef,
    ) -> Result<State> {
        if caller.actor != Actor::Human {
            return Err(LenkaError::new(
                ErrorCode::ActorDenied,
                "Only a person can set a status directly.",
            ));
        }
        if status == Status::Done {
            return Err(LenkaError::invalid_argument(
                "Use task.finish to complete a task, because `done` requires evidence.",
            ));
        }
        self.with_write(|locked, index, now| {
            self.ingest_if_changed(locked, index, id, now)?;
            check_not_invalid(index, id)?;
            let meta = index
                .tasks
                .get(&id)
                .cloned()
                .ok_or_else(|| task_missing(id))?;
            check_write_claim(index, id, caller, claim, now)?;
            if status.is_closed() && has_open_children(index, id) {
                return Err(LenkaError::new(
                    ErrorCode::Blocked,
                    "The task has open children. Complete or cancel them first.",
                ));
            }
            if !status.is_closed()
                && let Some(parent) = meta.front.parent
            {
                check_parent_open(index, parent)?;
            }
            let mut state = meta.state.clone();
            state.status = status;
            state.loop_stopped = false;
            if status.is_closed() {
                state.loop_until_done = false;
            }
            if meta.state.status.is_closed() {
                // A reopened task blocks its dependents again.
                state.unblocked = false;
            }
            let mut ops = vec![put(
                format!("{}/state.toml", root_of(id)),
                toml_text(&state)?,
            )];
            ops.push(Op::Remove {
                path: loops_path(&id.to_string()),
            });
            if index.claims.contains_key(&id) {
                ops.push(Op::Remove {
                    path: claim_path(id),
                });
                ops.push(feed_op(&simple_event(
                    FeedKind::Claim,
                    id,
                    state.version,
                    caller,
                    now,
                ))?);
            }
            ops.push(feed_op(&simple_event(
                FeedKind::State,
                id,
                state.version,
                caller,
                now,
            ))?);
            locked.commit(&Commit {
                ops,
                ..Commit::default()
            })?;
            Ok(state)
        })
    }

    /// Adds an entry. Any caller can add an entry while another holder has the claim.
    ///
    /// # Errors
    ///
    /// Returns an error if the text fails a check.
    pub fn add_entry(&self, caller: &Caller, id: TaskId, new: NewEntry) -> Result<Entry> {
        let policy = policy_for(caller, new.secret_policy)?;
        if new.text.trim().is_empty() {
            return Err(LenkaError::invalid_argument("An entry requires text."));
        }
        check_text_size("The entry", &new.text)?;
        input::check_text("The entry", &new.text)?;
        self.with_write(|locked, index, now| {
            let meta = index
                .tasks
                .get(&id)
                .cloned()
                .ok_or_else(|| task_missing(id))?;
            let entry = Entry {
                id: EntryId::generate(),
                kind: new.kind,
                author: caller.label.clone(),
                at: now,
                stage: None,
                replaces: new.replaces,
                text: new.text.clone(),
            };
            let mut ops = vec![entry_op(id, &entry)?];
            ops.push(feed_op(&simple_event(
                FeedKind::Entry,
                id,
                meta.state.version,
                caller,
                now,
            ))?);
            if new.kind == EntryKind::Proposal
                && let Some(coordinator) = coordinator_of(index, id)
                && let Some(claim) = index.active_claim(coordinator, now)
                && claim.agent_key != caller.agent_key
            {
                let notice = Notice {
                    id: NoticeId::generate(),
                    subject: Subject::Task(id.to_string()),
                    version: meta.state.version,
                    changed: vec!["proposal".to_owned()],
                    author: caller.label.clone(),
                    reason: "A worker recorded a proposal.".to_owned(),
                    at: now,
                    claim: claim.handle.clone(),
                    claimed_task: claim.task.to_string(),
                };
                ops.push(Op::Append {
                    path: notices::inbox_path(&claim.agent_key)?.as_str().to_owned(),
                    line: notices::format_line(&notice)?,
                });
            }
            locked.commit(&Commit {
                checks: vec![TextCheck::new("entry", new.text.clone())],
                secret_policy: policy,
                ops,
            })?;
            Ok(entry)
        })
    }

    /// Deletes a task.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::DeleteRefused`] if a delete rule stops the operation.
    pub fn delete(&self, caller: &Caller, id: TaskId) -> Result<()> {
        if caller.actor == Actor::Agent && !self.project.config().limits.agent_delete {
            return Err(LenkaError::new(
                ErrorCode::DeleteRefused,
                "A project setting denies the delete tools to agents.",
            ));
        }
        self.with_write(|locked, index, now| {
            if !index.tasks.contains_key(&id) && !index.invalid.contains_key(&id) {
                return Err(task_missing(id));
            }
            if index
                .children
                .get(&id)
                .is_some_and(|items| !items.is_empty())
            {
                return Err(LenkaError::new(
                    ErrorCode::DeleteRefused,
                    "The task has children. Delete or move them first.",
                ));
            }
            if index.blocks.get(&id).is_some_and(|items| !items.is_empty())
                || index
                    .related_from
                    .get(&id)
                    .is_some_and(|items| !items.is_empty())
            {
                return Err(LenkaError::new(
                    ErrorCode::DeleteRefused,
                    "A task in this project depends on this task. Remove that link first.",
                ));
            }
            check_write_claim(index, id, caller, &ClaimRef::Implicit, now)?;
            let ops = vec![
                Op::RemoveTree { path: root_of(id) },
                Op::Remove {
                    path: claim_path(id),
                },
                Op::Remove {
                    path: loops_path(&id.to_string()),
                },
                feed_op(&simple_event(FeedKind::TaskDeleted, id, 0, caller, now))?,
            ];
            locked.commit(&Commit {
                ops,
                ..Commit::default()
            })?;
            Ok(())
        })
    }

    // ---- Notices ----

    /// Returns the open notices of a caller and the changes of soft link targets.
    ///
    /// # Errors
    ///
    /// Returns an error if the inbox cannot be read.
    pub fn call_notes(&self, caller: &Caller) -> Result<CallNotes> {
        let now = self.now();
        let mut unread = notices::read_inbox(self.project.driver(), &caller.agent_key)?;
        let index = self.read_index()?;
        unread.retain(|notice| notice_subject_exists(&index, self.project.driver(), notice));
        let mut links: BTreeSet<Link> = BTreeSet::new();
        for claim in index.claims.values() {
            if claim.agent_key == caller.agent_key
                && claim.expires_at > now
                && let Some(meta) = index.tasks.get(&claim.task)
            {
                links.extend(
                    meta.front
                        .blocked_by
                        .iter()
                        .chain(&meta.front.relates)
                        .filter(|link| matches!(link, Link::Remote(..))),
                );
            }
        }
        drop(index);
        let mut cross_project = Vec::new();
        if !links.is_empty() {
            let mut reader = LinkReader::default();
            let mut seen = lock(&self.seen_links);
            for link in links {
                let view = reader.view(&self.project, link);
                let current = (view.version, view.state);
                match seen.insert(link, current) {
                    Some(previous) if previous != current => cross_project.push(view),
                    _ => {}
                }
            }
        }
        Ok(CallNotes {
            unread,
            cross_project,
        })
    }

    fn close_notices(&self, caller: &Caller, read: &[(Subject, u64)]) -> Result<()> {
        let driver = self.project.driver();
        let closes = |notice: &Notice| {
            read.iter()
                .any(|(subject, version)| notice.subject == *subject && notice.version <= *version)
        };
        let open = notices::read_inbox(driver, &caller.agent_key)?;
        if !open.iter().any(closes) {
            return Ok(());
        }
        self.project.write_locked(|locked| {
            let open = notices::read_inbox(driver, &caller.agent_key)?;
            let keep: Vec<Notice> = open.into_iter().filter(|notice| !closes(notice)).collect();
            let path = notices::inbox_path(&caller.agent_key)?.as_str().to_owned();
            let op = if keep.is_empty() {
                Op::Remove { path }
            } else {
                put(path, notices::format_inbox(&keep)?)
            };
            locked.commit(&Commit {
                ops: vec![op],
                ..Commit::default()
            })
        })
    }

    /// Takes the index and the write lock in the correct order, for an artifact write.
    ///
    /// # Errors
    ///
    /// Returns the error of the work, or an error from the lock.
    pub fn with_artifact_write<T>(
        &self,
        work: impl FnOnce(&mut Locked<'_>, ArtifactRouting<'_>, Timestamp) -> Result<T>,
    ) -> Result<T> {
        self.with_write(|locked, index, now| work(locked, ArtifactRouting { index, now }, now))
    }

    /// Returns the tasks that link to an artifact.
    ///
    /// # Errors
    ///
    /// Returns an error if the index cannot be read.
    pub fn artifact_users(&self, artifact: ArtifactId) -> Result<Vec<TaskId>> {
        let index = self.read_index()?;
        Ok(index
            .artifact_users
            .get(&artifact)
            .map(|items| items.iter().copied().collect())
            .unwrap_or_default())
    }

    /// Checks the coordinator rule for a change to a shared artifact.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ActorDenied`] if a coordinator holds the task tree of a linked task.
    pub fn check_artifact_coordinator(
        &self,
        artifact: ArtifactId,
        caller: &Caller,
        claim: &ClaimRef,
    ) -> Result<()> {
        let now = self.now();
        let index = self.read_index()?;
        if let Some(users) = index.artifact_users.get(&artifact) {
            for task in users {
                if let Some(coordinator) = coordinator_of(&index, *task) {
                    check_coordinator(&index, coordinator, caller, claim, now)?;
                }
            }
        }
        Ok(())
    }

    // ---- External edits, repair, and checks ----

    fn record_external_edits(&self, changed: &[TaskId]) -> Result<()> {
        let driver = self.project.driver();
        let edited: Vec<TaskId> = changed
            .iter()
            .filter(|id| differs_from_version(driver, **id))
            .copied()
            .collect();
        if edited.is_empty() {
            return Ok(());
        }
        self.with_write(|locked, index, now| {
            for id in &edited {
                // A process can read during a commit of another process, so check again here.
                self.ingest_if_changed(locked, index, *id, now)?;
            }
            Ok(())
        })
    }

    /// Records each hand edit in the project as a new version. A Git commit needs this first, so
    /// that the pushed data needs no new version on another clone.
    ///
    /// # Errors
    ///
    /// Returns an error if the data cannot be read or written.
    pub fn record_all_external_edits(&self) -> Result<()> {
        let driver = self.project.driver();
        self.with_write(|locked, index, now| {
            index.scan(driver)?;
            for id in index.order.clone() {
                self.ingest_if_changed(locked, index, id, now)?;
            }
            Ok(())
        })
    }

    /// Returns the version of each task.
    ///
    /// # Errors
    ///
    /// Returns an error if the feed cannot be read.
    pub fn task_versions(&self) -> Result<TaskVersions> {
        let index = self.read_index()?;
        Ok(TaskVersions(
            index
                .tasks
                .iter()
                .map(|(id, meta)| (*id, meta.state.version))
                .collect(),
        ))
    }

    /// Creates notices for the material changes that a Git merge brought. Call this after the
    /// reindex, with the versions from before the merge.
    ///
    /// # Errors
    ///
    /// Returns an error if the data cannot be read or written.
    pub fn notify_merged_changes(&self, before: &TaskVersions) -> Result<()> {
        let driver = self.project.driver();
        self.with_write(|locked, index, now| {
            let mut ops = Vec::new();
            for (id, old) in &before.0 {
                let Some(meta) = index.tasks.get(id) else {
                    continue;
                };
                let newest = (old + 1..=meta.state.version)
                    .rev()
                    .filter_map(|version| read_version_meta(driver, *id, version))
                    .find(|version| version.material);
                if let Some(version) = newest {
                    ops.extend(notice_ops(
                        index,
                        *id,
                        Subject::Task(id.to_string()),
                        &external_caller(),
                        &version,
                        now,
                    )?);
                }
            }
            if ops.is_empty() {
                return Ok(());
            }
            locked.commit(&Commit {
                ops,
                ..Commit::default()
            })
        })
    }

    fn ingest_external_on_read(&self, id: TaskId) -> Result<()> {
        let needed = {
            let index = lock(&self.index);
            let Some(meta) = index.tasks.get(&id) else {
                return Ok(());
            };
            stamps_differ(self.project.driver(), meta)
        };
        if needed {
            self.with_write(|locked, index, now| self.ingest_if_changed(locked, index, id, now))?;
        }
        Ok(())
    }

    /// Brings one task up to date under the lock, before a write that depends on it. A feed line of
    /// another write can already have updated the stamps, so this compares the text of the files.
    fn ingest_if_changed(
        &self,
        locked: &mut Locked<'_>,
        index: &mut Index,
        id: TaskId,
        now: Timestamp,
    ) -> Result<()> {
        let driver = self.project.driver();
        let Some(meta) = index.tasks.get(&id) else {
            return Ok(());
        };
        if stamps_differ(driver, meta) {
            index.reload(driver, id);
        }
        self.restore_lost_version(locked, index, id, now)?;
        if differs_from_version(driver, id) {
            self.record_external(locked, index, id, now)?;
            index.refresh(driver)?;
        }
        Ok(())
    }

    /// Raises a version in `state.toml` that is lower than the newest stored version. An old backup
    /// of the file can cause this. Without this step, the next write replaces a stored version.
    fn restore_lost_version(
        &self,
        locked: &mut Locked<'_>,
        index: &mut Index,
        id: TaskId,
        now: Timestamp,
    ) -> Result<()> {
        let driver = self.project.driver();
        let Some(meta) = index.tasks.get(&id) else {
            return Ok(());
        };
        let newest = newest_version(driver, id, meta.state.version)?;
        if newest == meta.state.version {
            return Ok(());
        }
        let mut state = meta.state.clone();
        state.version = newest;
        locked.commit(&Commit {
            ops: vec![
                put(format!("{}/state.toml", root_of(id)), toml_text(&state)?),
                feed_op(&FeedEvent {
                    kind: FeedKind::State,
                    id: id.to_string(),
                    version: newest,
                    author: EXTERNAL_AUTHOR.to_owned(),
                    reason: String::new(),
                    changed: Vec::new(),
                    at: now,
                })?,
            ],
            ..Commit::default()
        })?;
        index.refresh(driver)
    }

    fn record_external(
        &self,
        locked: &mut Locked<'_>,
        index: &Index,
        id: TaskId,
        now: Timestamp,
    ) -> Result<()> {
        let driver = self.project.driver();
        let Ok(current) = load_task(driver, id) else {
            // An invalid file stays reported, and lenka keeps the last valid version.
            return Ok(());
        };
        let root = root_of(id);
        let old_version = current.meta.state.version;
        let old_dir = format!("{root}/versions/{old_version:06}");
        let Some(old_text) = read_text(driver, &format!("{old_dir}/task.md"))? else {
            return Ok(());
        };
        let old_plan = read_text(driver, &format!("{old_dir}/plan.md"))?;
        let old_body = split_front::<TaskFront>("task.md", &old_text)
            .map(|(_, body)| Body::parse(&body))
            .unwrap_or_default();
        let new_body = Body::parse(&current.body);
        let changed = changed_parts(
            &old_body,
            &new_body,
            old_plan.as_deref(),
            current.plan.as_deref(),
        );
        let version = old_version + 1;
        let meta = VersionMeta {
            version,
            author: EXTERNAL_AUTHOR.to_owned(),
            at: now,
            reason: "External edit of the file.".to_owned(),
            material: !changed.is_empty(),
            changed,
        };
        let text = read_text(driver, &format!("{root}/task.md"))?.unwrap_or_default();
        let mut state = current.meta.state.clone();
        state.version = version;
        let mut ops = version_ops(id, version, &text, current.plan.as_deref(), &meta)?;
        ops.push(put(format!("{root}/state.toml"), toml_text(&state)?));
        ops.push(feed_op(&task_event(id, version, FeedKind::Task, &meta))?);
        if meta.material {
            ops.extend(notice_ops(
                index,
                id,
                Subject::Task(id.to_string()),
                &external_caller(),
                &meta,
                now,
            )?);
        }
        locked.commit(&Commit {
            // A person already wrote this file, so the secret check has no effect on the disk here.
            ops,
            ..Commit::default()
        })
    }

    /// Rebuilds the index from the files, stores the snapshot, and tells other processes.
    ///
    /// # Errors
    ///
    /// Returns an error if the data cannot be read.
    pub fn reindex(&self, caller: &Caller) -> Result<usize> {
        self.project.reload_config()?;
        let driver = self.project.driver();
        let mut fresh = Index::default();
        let changed = fresh.scan(driver)?;
        fresh.reload_claims(driver)?;
        fresh.feed_offset = driver
            .stat(&DocPath::new(FEED_PATH)?)?
            .map_or(0, |stat| stat.len);
        let count = fresh.tasks.len();
        fresh.save_snapshot(&self.project)?;
        *lock(&self.index) = fresh;
        self.record_external_edits(&changed)?;
        let now = self.now();
        self.with_write(|locked, _, _| {
            locked.commit(&Commit {
                ops: vec![feed_op(&FeedEvent {
                    kind: FeedKind::Reindex,
                    id: String::new(),
                    version: 0,
                    author: caller.label.clone(),
                    reason: String::new(),
                    changed: Vec::new(),
                    at: now,
                })?],
                ..Commit::default()
            })
        })?;
        Ok(count)
    }

    /// Stores the index snapshot for the next process.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn save_snapshot(&self) -> Result<()> {
        let index = self.read_index()?;
        index.save_snapshot(&self.project)
    }

    /// Checks the project data.
    ///
    /// # Errors
    ///
    /// Returns an error if the data cannot be read.
    pub fn doctor(&self) -> Result<DoctorReport> {
        let mut fresh = Index::default();
        let driver = self.project.driver();
        fresh.scan(driver)?;
        let mut report = DoctorReport {
            invalid_files: fresh
                .invalid
                .iter()
                .map(|(id, reason)| format!("{id}: {reason}"))
                .collect(),
            bad_names: fresh.bad_names.clone(),
            cycles: fresh
                .find_cycles()
                .into_iter()
                .map(|cycle| cycle.iter().map(TaskId::short).collect())
                .collect(),
            unfinished_write: self
                .project
                .repair_if_needed()
                .err()
                .map(|err| err.to_string()),
            ..DoctorReport::default()
        };
        let mut reader = LinkReader::default();
        for meta in fresh.tasks.values() {
            if self.stage_position(meta).is_err() {
                report.missing_stages.push(format!(
                    "{}: type {}, stage {}",
                    meta.id().short(),
                    meta.front.kind,
                    meta.state.stage
                ));
            }
            for link in meta.front.blocked_by.iter().chain(&meta.front.relates) {
                if matches!(link, Link::Remote(..)) {
                    let view = reader.view(&self.project, *link);
                    let line = format!("{} -> {}", meta.id().short(), view.link);
                    match view.state {
                        LinkState::Broken => report.broken_links.push(line),
                        LinkState::Unavailable => report.unavailable_links.push(line),
                        LinkState::Ok => {}
                    }
                }
            }
        }
        report.invalid_files.sort();
        report.broken_links.sort();
        report.unavailable_links.sort();
        report.missing_stages.sort();
        Ok(report)
    }
}

fn resolve_in(index: &Index, text: &str) -> Result<TaskId> {
    if let Ok(id) = TaskId::parse(text) {
        if index.tasks.contains_key(&id) || index.invalid.contains_key(&id) {
            return Ok(id);
        }
        return Err(task_missing(id));
    }
    let valid_short = text.len() >= 4
        && text.len() < 36
        && text
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-');
    if !valid_short {
        return Err(LenkaError::invalid_argument(
            "A task ID must be a full ID or a short ID from a tool result.",
        ));
    }
    let mut found = index.order.iter().filter(|id| id.matches_short(text));
    let Some(first) = found.next() else {
        return Err(LenkaError::not_found(format!(
            "No task matches the ID {text}."
        )));
    };
    if found.next().is_some() {
        return Err(LenkaError::new(
            ErrorCode::AmbiguousId,
            format!("The short ID {text} matches more than one task. Use the full ID."),
        ));
    }
    Ok(*first)
}

fn task_missing(id: TaskId) -> LenkaError {
    LenkaError::not_found(format!("The task {} does not exist.", id.short()))
}

fn claim_lost() -> LenkaError {
    LenkaError::new(
        ErrorCode::ClaimLost,
        "The claim expired, or another holder has the task. Call task.claim, and read the task again.",
    )
}

fn cycle_error() -> LenkaError {
    LenkaError::new(
        ErrorCode::Cycle,
        "This relation creates a cycle. Remove another relation first.",
    )
}

/// Refuses a change of a closed task. Only [`TaskService::set_status`] opens a closed task.
fn check_not_closed(meta: &TaskMeta) -> Result<()> {
    if meta.state.status.is_closed() {
        return Err(LenkaError::invalid_argument(format!(
            "The task {} is {}, so it cannot change. A person can open it again with a status change.",
            meta.id().short(),
            meta.state.status.as_str()
        )));
    }
    Ok(())
}

/// Refuses an agent on a task that the loop limit stopped.
fn check_loop_not_stopped(meta: &TaskMeta, caller: &Caller) -> Result<()> {
    if caller.actor != Actor::Human && meta.state.loop_stopped {
        return Err(LenkaError::new(
            ErrorCode::LoopLimit,
            format!(
                "The loop limit stopped the task {}. A person must act: change its status, claim it, or let the agent continue until the task is done.",
                meta.id().short()
            ),
        ));
    }
    Ok(())
}

/// Refuses a closed parent. A closed parent has no open children.
fn check_parent_open(index: &Index, parent: TaskId) -> Result<()> {
    if let Some(meta) = index.tasks.get(&parent)
        && meta.state.status.is_closed()
    {
        return Err(LenkaError::invalid_argument(format!(
            "The parent task {} is {}. Open it first.",
            parent.short(),
            meta.state.status.as_str()
        )));
    }
    Ok(())
}

fn check_not_invalid(index: &Index, id: TaskId) -> Result<()> {
    match index.invalid.get(&id) {
        Some(reason) => Err(LenkaError::invalid_file(reason.clone())),
        None => Ok(()),
    }
}

fn clean_title(title: &str) -> Result<String> {
    let line = title.trim();
    if line.is_empty() {
        return Err(LenkaError::invalid_argument("A task requires a title."));
    }
    if line.len() > MAX_TITLE_BYTES || line.chars().any(char::is_control) {
        return Err(LenkaError::invalid_argument(format!(
            "A title is one line with at most {MAX_TITLE_BYTES} bytes and no control characters."
        )));
    }
    Ok(line.to_owned())
}

fn clean_labels(labels: &[String]) -> Vec<String> {
    let mut result: Vec<String> = labels
        .iter()
        .map(|label| label.trim().to_owned())
        .filter(|label| !label.is_empty())
        .collect();
    result.sort();
    result.dedup();
    result
}

fn check_dates(start: Option<Day>, due: Option<Day>) -> Result<()> {
    if let (Some(start), Some(due)) = (start, due)
        && start > due
    {
        return Err(LenkaError::invalid_argument(
            "The first day must not be after the last day.",
        ));
    }
    Ok(())
}

fn update_list<T: Copy + PartialEq>(list: &mut Vec<T>, add: &[T], remove: &[T]) {
    list.retain(|item| !remove.contains(item));
    for item in add {
        if !list.contains(item) {
            list.push(*item);
        }
    }
}

fn apply_fields(front: &mut TaskFront, patch: &TaskPatch) -> Result<()> {
    if let Some(priority) = patch.priority {
        front.priority = priority;
    }
    if !patch.labels_add.is_empty() || !patch.labels_remove.is_empty() {
        let mut labels = front.labels.clone();
        labels.retain(|label| !patch.labels_remove.contains(label));
        labels.extend(patch.labels_add.iter().cloned());
        front.labels = clean_labels(&labels);
    }
    if let Some(start) = patch.start {
        front.start = start;
    }
    if let Some(due) = patch.due {
        front.due = due;
    }
    check_dates(front.start, front.due)?;
    if let Some(coordinated) = patch.coordinated {
        front.coordinated = coordinated;
    }
    Ok(())
}

fn changed_parts(
    old: &Body,
    new: &Body,
    old_plan: Option<&str>,
    new_plan: Option<&str>,
) -> Vec<String> {
    let mut changed = Vec::new();
    if old.section(DESCRIPTION).map(str::trim) != new.section(DESCRIPTION).map(str::trim) {
        changed.push("description".to_owned());
    }
    if old.section(REQUIREMENTS).map(str::trim) != new.section(REQUIREMENTS).map(str::trim) {
        changed.push("requirements".to_owned());
    }
    if criteria_text_differs(old.section(ACCEPTANCE), new.section(ACCEPTANCE)) {
        changed.push("acceptance_criteria".to_owned());
    }
    if old_plan.map(str::trim) != new_plan.map(str::trim) {
        changed.push("plan".to_owned());
    }
    changed
}

fn version_ops(
    id: TaskId,
    version: u64,
    task_text: &str,
    plan: Option<&str>,
    meta: &VersionMeta,
) -> Result<Vec<Op>> {
    let root = root_of(id);
    let dir = format!("{root}/versions/{version:06}");
    let mut ops = vec![put(format!("{dir}/task.md"), task_text.to_owned())];
    if let Some(plan) = plan {
        ops.push(put(format!("{dir}/plan.md"), plan.to_owned()));
    }
    ops.push(put(format!("{dir}/meta.toml"), toml_text(meta)?));
    ops.push(put(format!("{root}/task.md"), task_text.to_owned()));
    if let Some(plan) = plan {
        ops.push(put(format!("{root}/plan.md"), plan.to_owned()));
    }
    Ok(ops)
}

fn task_event(id: TaskId, version: u64, kind: FeedKind, meta: &VersionMeta) -> FeedEvent {
    FeedEvent {
        kind,
        id: id.to_string(),
        version,
        author: meta.author.clone(),
        reason: meta.reason.clone(),
        changed: meta.changed.clone(),
        at: meta.at,
    }
}

fn simple_event(
    kind: FeedKind,
    id: TaskId,
    version: u64,
    caller: &Caller,
    now: Timestamp,
) -> FeedEvent {
    FeedEvent {
        kind,
        id: id.to_string(),
        version,
        author: caller.label.clone(),
        reason: String::new(),
        changed: Vec::new(),
        at: now,
    }
}

fn entry_op(id: TaskId, entry: &Entry) -> Result<Op> {
    let text = join_front(entry, &entry.text)?;
    Ok(put(
        format!("{}/entries/{}.md", root_of(id), entry.id),
        text,
    ))
}

fn notice_ops(
    index: &Index,
    id: TaskId,
    subject: Subject,
    caller: &Caller,
    meta: &VersionMeta,
    now: Timestamp,
) -> Result<Vec<Op>> {
    let mut targets = index.neighbors(id);
    targets.insert(id);
    let mut ops = Vec::new();
    for task in targets {
        if let Some(claim) = index.active_claim(task, now)
            && claim.agent_key != caller.agent_key
        {
            let notice = Notice {
                id: NoticeId::generate(),
                subject: subject.clone(),
                version: meta.version,
                changed: meta.changed.clone(),
                author: meta.author.clone(),
                reason: input::clip(&meta.reason, input::MAX_REASON_BYTES).to_owned(),
                at: now,
                claim: claim.handle.clone(),
                claimed_task: task.to_string(),
            };
            ops.push(Op::Append {
                path: notices::inbox_path(&claim.agent_key)?.as_str().to_owned(),
                line: notices::format_line(&notice)?,
            });
        }
    }
    Ok(ops)
}

/// Tells if the subject of a notice still exists. A read of a deleted subject cannot close a notice,
/// so such a notice must not stop any work.
fn notice_subject_exists(
    index: &Index,
    driver: &dyn lenka_plugin_api::StorageDriver,
    notice: &Notice,
) -> bool {
    match &notice.subject {
        Subject::Task(id) => TaskId::parse(id).is_ok_and(|id| index.tasks.contains_key(&id)),
        Subject::Artifact(id) => DocPath::new(format!("artifacts/{id}/artifact.toml"))
            .ok()
            .and_then(|path| driver.stat(&path).ok().flatten())
            .is_some(),
    }
}

fn check_unread(
    index: &Index,
    driver: &dyn lenka_plugin_api::StorageDriver,
    claim: &Claim,
) -> Result<()> {
    let open = notices::read_inbox(driver, &claim.agent_key)?;
    let task = claim.task.to_string();
    let unread: Vec<&Notice> = open
        .iter()
        // A notice stays valid for a new claim on the same task. A claim can end by timeout.
        .filter(|notice| {
            notice.claimed_task == task && notice_subject_exists(index, driver, notice)
        })
        .collect();
    if unread.is_empty() {
        return Ok(());
    }
    let names: Vec<String> = unread
        .iter()
        .take(MAX_NAMES_IN_ERROR)
        .map(|notice| match &notice.subject {
            Subject::Task(id) => format!("task {id} version {}", notice.version),
            Subject::Artifact(id) => format!("artifact {id} version {}", notice.version),
        })
        .collect();
    Err(LenkaError::new(
        ErrorCode::UnreadChanges,
        format!(
            "This agent has not read these changes: {}. Read them with task.show. Use `include` with `artifacts` for an artifact.",
            names.join(", ")
        ),
    ))
}

fn check_write_claim(
    index: &Index,
    id: TaskId,
    caller: &Caller,
    claim: &ClaimRef,
    now: Timestamp,
) -> Result<()> {
    let active = index.active_claim(id, now);
    match claim {
        ClaimRef::Override => {
            if caller.actor == Actor::Human {
                Ok(())
            } else {
                Err(LenkaError::new(
                    ErrorCode::ActorDenied,
                    "Only a person can override a claim.",
                ))
            }
        }
        ClaimRef::Handle(handle) => match active {
            Some(found) if found.handle == *handle => Ok(()),
            _ => Err(claim_lost()),
        },
        ClaimRef::Implicit => match active {
            Some(found) if found.agent_key != caller.agent_key => Err(LenkaError::new(
                ErrorCode::ClaimLost,
                format!(
                    "{} holds this task until {}. {}",
                    found.holder,
                    found.expires_at,
                    if caller.actor == Actor::Human {
                        "To change it now, use --override."
                    } else {
                        "Record a proposal with task.entry.add."
                    }
                ),
            )),
            Some(_) => Ok(()),
            // The claim of this caller ended, so another agent can hold newer work on this task.
            None if caller.actor != Actor::Human
                && index
                    .claims
                    .get(&id)
                    .is_some_and(|old| old.agent_key == caller.agent_key) =>
            {
                Err(claim_lost())
            }
            None => Ok(()),
        },
    }
}

/// Checks the claim for a stage operation, and returns the active claim of the caller.
fn check_stage_claim(
    index: &Index,
    id: TaskId,
    caller: &Caller,
    claim: &ClaimRef,
    now: Timestamp,
) -> Result<Option<Claim>> {
    check_write_claim(index, id, caller, claim, now)?;
    let active = index.active_claim(id, now).cloned();
    if matches!(claim, ClaimRef::Override) {
        return Ok(None);
    }
    if active.is_none() && caller.actor != Actor::Human {
        return Err(claim_lost());
    }
    Ok(active)
}

fn coordinator_of(index: &Index, id: TaskId) -> Option<TaskId> {
    let meta = index.tasks.get(&id)?;
    if meta.front.coordinated {
        return Some(id);
    }
    index.ancestors(id).into_iter().find(|ancestor| {
        index
            .tasks
            .get(ancestor)
            .is_some_and(|meta| meta.front.coordinated)
    })
}

fn check_coordinator(
    index: &Index,
    coordinator: TaskId,
    caller: &Caller,
    claim: &ClaimRef,
    now: Timestamp,
) -> Result<()> {
    if caller.actor == Actor::Human {
        return Ok(());
    }
    let holds = index.active_claim(coordinator, now).is_some_and(|active| {
        active.agent_key == caller.agent_key
            || matches!(claim, ClaimRef::Handle(handle) if *handle == active.handle)
    });
    if holds {
        return Ok(());
    }
    Err(LenkaError::new(
        ErrorCode::ActorDenied,
        format!(
            "The task {} is coordinated, so only the holder of its claim can change the plan and shared artifacts. Record a proposal with task.entry.add.",
            coordinator.short()
        ),
    ))
}

fn has_open_children(index: &Index, id: TaskId) -> bool {
    index.children.get(&id).is_some_and(|items| {
        items.iter().any(|child| {
            index
                .tasks
                .get(child)
                .is_some_and(|meta| !meta.state.status.is_closed())
        })
    })
}

fn local_blocks(index: &Index, target: TaskId) -> bool {
    // A task with invalid files and no record still blocks, because its state is not known.
    index.tasks.get(&target).map_or_else(
        || index.invalid.contains_key(&target),
        |meta| !meta.state.status.is_closed() && !meta.state.unblocked,
    )
}

fn is_blocked(index: &Index, project: &Project, id: TaskId, reader: &mut LinkReader) -> bool {
    std::iter::once(id).chain(index.ancestors(id)).any(|task| {
        index.tasks.get(&task).is_some_and(|meta| {
            meta.front.blocked_by.iter().any(|link| match link {
                Link::Local(target) => local_blocks(index, *target),
                Link::Remote(..) => reader.view(project, *link).blocks(),
            })
        })
    })
}

fn active_blockers(
    index: &Index,
    project: &Project,
    id: TaskId,
    reader: &mut LinkReader,
) -> Vec<BlockerView> {
    let mut result = Vec::new();
    for task in std::iter::once(id).chain(index.ancestors(id)) {
        let Some(meta) = index.tasks.get(&task) else {
            continue;
        };
        let via_parent = (task != id).then(|| task.short());
        for link in &meta.front.blocked_by {
            match link {
                Link::Local(target) => {
                    if local_blocks(index, *target) {
                        // A task with invalid files can have no record, but it still blocks.
                        let found = index.tasks.get(target);
                        result.push(BlockerView {
                            id: target.short(),
                            title: found.map(|meta| meta.front.title.clone()),
                            status: found.map(|meta| meta.state.status),
                            via_parent: via_parent.clone(),
                        });
                    }
                }
                Link::Remote(..) => {
                    let view = reader.view(project, *link);
                    if view.blocks() {
                        result.push(BlockerView {
                            id: view.link,
                            title: view.title,
                            status: view.status,
                            via_parent: via_parent.clone(),
                        });
                    }
                }
            }
        }
    }
    result
}

fn read_text(driver: &dyn lenka_plugin_api::StorageDriver, path: &str) -> Result<Option<String>> {
    match driver.read(&DocPath::new(path)?)? {
        Some(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| LenkaError::invalid_file(format!("{path} is not valid UTF-8."))),
        None => Ok(None),
    }
}

/// The caller for a change with no author in this process. Each claim holder gets a notice.
fn external_caller() -> Caller {
    Caller {
        actor: Actor::Human,
        label: EXTERNAL_AUTHOR.to_owned(),
        worktree: std::path::PathBuf::new(),
        agent_key: lenka_plugin_api::AgentKey(String::new()),
        scope: lenka_plugin_api::ProjectScope::All,
        default_project: None,
    }
}

fn read_version_meta(
    driver: &dyn lenka_plugin_api::StorageDriver,
    id: TaskId,
    version: u64,
) -> Option<VersionMeta> {
    let path = format!("{}/versions/{version:06}/meta.toml", root_of(id));
    let text = read_text(driver, &path).ok().flatten()?;
    toml::from_str(&text).ok()
}

/// Returns the newest stored version of a task. The usual case needs one file check.
fn newest_version(
    driver: &dyn lenka_plugin_api::StorageDriver,
    id: TaskId,
    known: u64,
) -> Result<u64> {
    let root = root_of(id);
    let next = DocPath::new(format!("{root}/versions/{:06}/meta.toml", known + 1))?;
    if driver.stat(&next)?.is_none() {
        return Ok(known);
    }
    let names = driver.list(&DocPath::new(format!("{root}/versions"))?)?;
    Ok(names
        .iter()
        .filter_map(|name| name.parse::<u64>().ok())
        .fold(known, u64::max))
}

fn stamps_differ(driver: &dyn lenka_plugin_api::StorageDriver, meta: &TaskMeta) -> bool {
    let Ok(paths) = crate::index::task_paths(meta.id()) else {
        return true;
    };
    paths
        .iter()
        .zip(&meta.stamps)
        .any(|(path, stamp)| driver.stat(path).ok().flatten() != *stamp)
}

fn differs_from_version(driver: &dyn lenka_plugin_api::StorageDriver, id: TaskId) -> bool {
    let root = root_of(id);
    let Ok(Some(state_text)) = read_text(driver, &format!("{root}/state.toml")) else {
        return false;
    };
    let Ok(state) = toml::from_str::<State>(&state_text) else {
        return false;
    };
    let dir = format!("{root}/versions/{:06}", state.version);
    let read = |path: String| read_text(driver, &path).ok().flatten();
    let Some(old_task) = read(format!("{dir}/task.md")) else {
        return false;
    };
    read(format!("{root}/task.md")) != Some(old_task)
        || read(format!("{root}/plan.md")) != read(format!("{dir}/plan.md"))
}

fn read_last_valid(
    driver: &dyn lenka_plugin_api::StorageDriver,
    meta: &TaskMeta,
) -> Option<LoadedTask> {
    let dir = format!("{}/versions/{:06}", root_of(meta.id()), meta.state.version);
    let text = read_text(driver, &format!("{dir}/task.md")).ok()??;
    let (_, body): (TaskFront, String) = split_front("task.md", &text).ok()?;
    Some(LoadedTask {
        meta: meta.clone(),
        body,
        plan: read_text(driver, &format!("{dir}/plan.md")).ok()?,
    })
}

fn read_entries(driver: &dyn lenka_plugin_api::StorageDriver, id: TaskId) -> Result<Vec<Entry>> {
    let dir = DocPath::new(format!("{}/entries", root_of(id)))?;
    let mut entries = Vec::new();
    for name in driver.list(&dir)? {
        if !name.ends_with(".md") {
            continue;
        }
        let Some(text) = read_text(driver, dir.join(&name)?.as_str())? else {
            continue;
        };
        match split_front::<Entry>(&name, &text) {
            Ok((mut entry, body)) => {
                entry.text = body;
                entries.push(entry);
            }
            Err(error) => tracing::warn!(error = %error, "skipped an invalid entry file"),
        }
    }
    entries.sort_by_key(|entry| entry.id);
    Ok(entries)
}

fn read_versions(
    driver: &dyn lenka_plugin_api::StorageDriver,
    id: TaskId,
) -> Result<Vec<VersionMeta>> {
    let dir = DocPath::new(format!("{}/versions", root_of(id)))?;
    let mut versions = Vec::new();
    for name in driver.list(&dir)? {
        let Some(text) = read_text(driver, &format!("{dir}/{name}/meta.toml"))? else {
            continue;
        };
        if let Ok(meta) = toml::from_str::<VersionMeta>(&text) {
            versions.push(meta);
        }
    }
    versions.sort_by_key(|meta| meta.version);
    Ok(versions)
}

/// Reads the record of an artifact.
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidFile`] if the record is not valid.
pub fn read_artifact(project: &Project, id: ArtifactId) -> Result<Option<ArtifactRecord>> {
    let Some(text) = read_text(project.driver(), &format!("artifacts/{id}/artifact.toml"))? else {
        return Ok(None);
    };
    let record: ArtifactRecord = toml::from_str(&text).map_err(|err| {
        LenkaError::invalid_file(format!("artifact.toml is not valid: {}", err.message()))
    })?;
    check_artifact_record(id, &record)?;
    Ok(Some(record))
}

/// Checks a record from a file. A merge or a person can change the file, so it is untrusted.
fn check_artifact_record(id: ArtifactId, record: &ArtifactRecord) -> Result<()> {
    let fail = |problem: &str| {
        Err(LenkaError::invalid_file(format!(
            "artifact.toml of {id}: {problem}"
        )))
    };
    if record.id != id {
        return fail("the ID is not the same as the directory name.");
    }
    let Some(last) = record.versions.last() else {
        return fail("the file has no versions.");
    };
    if last.version != record.version {
        return fail("the current version is not the last version.");
    }
    if !is_safe_file_name(&record.name)
        || !record
            .versions
            .iter()
            .all(|item| is_safe_file_name(&item.file))
    {
        return fail("a file name is not valid.");
    }
    Ok(())
}

/// Tells if a name can be the name of an artifact. A stored name is one path part, so a hostile
/// record cannot point out of the artifact directory.
#[must_use]
pub fn is_safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|item| item == '/' || item == '\\' || item.is_control())
}

/// Creates the view of an artifact.
#[must_use]
pub fn artifact_view(project: &Project, record: &ArtifactRecord) -> ArtifactView {
    let current = record.current();
    let path = project
        .artifacts()
        .local_path(record.id, record.version, &current.file);
    let downloaded = path.as_deref().is_none_or(|path| !is_lfs_pointer(path));
    ArtifactView {
        id: record.id.short(),
        full_id: record.id.to_string(),
        name: record.name.clone(),
        media_type: record.media_type.clone(),
        version: record.version,
        size: current.size,
        sha256: current.sha256.clone(),
        path: path.map(|path| path.display().to_string()),
        downloaded,
    }
}

fn is_lfs_pointer(path: &std::path::Path) -> bool {
    use std::io::Read;
    const PREFIX: &[u8] = b"version https://git-lfs.github.com/spec/";
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0_u8; 40];
    file.read_exact(&mut head).is_ok() && head == PREFIX[..]
}

fn read_loop_counts(
    driver: &dyn lenka_plugin_api::StorageDriver,
    key: &str,
    now: Timestamp,
) -> Result<Vec<i64>> {
    let Some(text) = read_text(driver, &loops_path(key))? else {
        return Ok(Vec::new());
    };
    let values: Vec<i64> = serde_json::from_str(&text).unwrap_or_default();
    let cutoff = now.unix_ms() - MS_PER_HOUR;
    Ok(values.into_iter().filter(|value| *value > cutoff).collect())
}

fn loop_count_op(
    driver: &dyn lenka_plugin_api::StorageDriver,
    key: &str,
    now: Timestamp,
) -> Result<Op> {
    let mut values = read_loop_counts(driver, key, now)?;
    values.push(now.unix_ms());
    let text = serde_json::to_string(&values)
        .map_err(|err| LenkaError::invalid_file(format!("Loop counter: {err}")))?;
    Ok(put(loops_path(key), text))
}

/// Searches the text of tasks, plans, and decisions on the disk. A task with a title match needs
/// no disk read.
fn search_tasks<'a>(
    driver: &dyn lenka_plugin_api::StorageDriver,
    candidates: Vec<&'a TaskMeta>,
    text: &str,
) -> Vec<&'a TaskMeta> {
    let matches_text = |meta: &TaskMeta| -> bool {
        if meta.front.title.to_lowercase().contains(text) {
            return true;
        }
        let id = meta.id();
        let root = root_of(id);
        let has = |path: String| {
            read_text(driver, &path)
                .ok()
                .flatten()
                .is_some_and(|body| body.to_lowercase().contains(text))
        };
        if has(format!("{root}/task.md")) || (meta.has_plan && has(format!("{root}/plan.md"))) {
            return true;
        }
        read_entries(driver, id).is_ok_and(|entries| {
            entries.iter().any(|entry| {
                entry.kind == EntryKind::Decision && entry.text.to_lowercase().contains(text)
            })
        })
    };
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(8);
    if candidates.len() < 256 || threads == 1 {
        return candidates
            .into_iter()
            .filter(|meta| matches_text(meta))
            .collect();
    }
    let size = candidates.len().div_ceil(threads);
    let mut found: Vec<&TaskMeta> = std::thread::scope(|scope| {
        let handles: Vec<_> = candidates
            .chunks(size)
            .map(|chunk| {
                let matches_text = &matches_text;
                scope.spawn(move || {
                    chunk
                        .iter()
                        .copied()
                        .filter(|meta| matches_text(meta))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .flatten()
            .collect()
    });
    found.sort_by_key(|meta| meta.id());
    found
}
