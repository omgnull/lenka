// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The in-memory index of a project, with a snapshot file for a fast start.
//!
//! The snapshot is a cache. A process compares its stamps with the files at start, and reloads
//! only the tasks that changed. The change feed keeps a running process current.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use lenka_core::notices::FEED_PATH;
use lenka_core::{FeedEvent, FeedKind, Project};
use lenka_plugin_api::{
    ArtifactId, DocPath, DocStat, LenkaError, Result, StorageDriver, TaskId, Timestamp,
};
use serde::{Deserialize, Serialize};

use crate::markdown::{MAX_TEXT_BYTES, check_conflict_markers, split_front};
use crate::model::{Claim, Link, State, TaskFront, TaskMeta};

/// The count of bytes at the start of the feed that identify the file. Each event has a time and an
/// ID, so two feeds do not start with the same bytes.
const FEED_HEAD_BYTES: u64 = 256;

/// The length of a feed text with no line end at which lenka stops to wait for the line end.
const MAX_FEED_LINE_BYTES: usize = 32 * 1024 * 1024;

const SNAPSHOT_PATH: &str = ".runtime/index/snapshot.json";
const SNAPSHOT_FORMAT: u32 = 1;

/// The longest parent chain that lenka follows.
pub const MAX_DEPTH: usize = 256;

#[derive(Debug, Deserialize)]
struct Snapshot {
    format: u32,
    tasks: Vec<TaskMeta>,
}

/// The form of a snapshot for a write, with no copy of the records.
#[derive(Debug, Serialize)]
struct SnapshotRef<'a> {
    format: u32,
    tasks: Vec<&'a TaskMeta>,
}

/// The paths of the three current files of a task.
pub fn task_paths(id: TaskId) -> Result<[DocPath; 3]> {
    let root = format!("tasks/{id}");
    Ok([
        DocPath::new(format!("{root}/task.md"))?,
        DocPath::new(format!("{root}/plan.md"))?,
        DocPath::new(format!("{root}/state.toml"))?,
    ])
}

/// A task as it is on the disk.
#[derive(Debug, Clone)]
pub struct LoadedTask {
    /// The index record.
    pub meta: TaskMeta,
    /// The text of `task.md` after the front matter.
    pub body: String,
    /// The text of `plan.md`.
    pub plan: Option<String>,
}

fn read_utf8(driver: &dyn StorageDriver, path: &DocPath) -> Result<Option<String>> {
    match driver.read(path)? {
        None => Ok(None),
        Some(bytes) => {
            if bytes.len() > MAX_TEXT_BYTES {
                return Err(LenkaError::invalid_file(format!(
                    "{path} is larger than the limit of {MAX_TEXT_BYTES} bytes."
                )));
            }
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| LenkaError::invalid_file(format!("{path} is not valid UTF-8.")))
        }
    }
}

/// Reads and validates a task from its current files.
pub fn load_task(driver: &dyn StorageDriver, id: TaskId) -> Result<LoadedTask> {
    let [task_path, plan_path, state_path] = task_paths(id)?;
    let stamps = [
        driver.stat(&task_path)?,
        driver.stat(&plan_path)?,
        driver.stat(&state_path)?,
    ];
    let text = read_utf8(driver, &task_path)?
        .ok_or_else(|| LenkaError::not_found(format!("The task {} does not exist.", id.short())))?;
    check_conflict_markers(task_path.as_str(), &text)?;
    let (front, body): (TaskFront, String) = split_front(task_path.as_str(), &text)?;
    if front.id != id {
        return Err(LenkaError::invalid_file(format!(
            "{task_path} holds the ID of another task."
        )));
    }
    let plan = read_utf8(driver, &plan_path)?;
    if let Some(plan) = &plan {
        check_conflict_markers(plan_path.as_str(), plan)?;
    }
    let state_text = read_utf8(driver, &state_path)?
        .ok_or_else(|| LenkaError::invalid_file(format!("{state_path} is missing.")))?;
    check_conflict_markers(state_path.as_str(), &state_text)?;
    let state: State = toml::from_str(&state_text).map_err(|err| {
        LenkaError::invalid_file(format!("{state_path} is not valid: {}", err.message()))
    })?;
    // These names appear outside `untrusted_data` in tool results.
    if !lenka_core::input::is_safe_name(&front.kind)
        || !lenka_core::input::is_safe_name(&state.stage)
    {
        return Err(LenkaError::invalid_file(format!(
            "The type or the stage in the files of task {} is not a valid name.",
            id.short()
        )));
    }
    Ok(LoadedTask {
        meta: TaskMeta {
            has_plan: plan.is_some(),
            front,
            state,
            stamps,
        },
        body,
        plan,
    })
}

/// Reads the last valid record of a task with invalid current files, from its newest version.
fn load_last_valid(driver: &dyn StorageDriver, id: TaskId) -> Option<TaskMeta> {
    let [task_path, plan_path, state_path] = task_paths(id).ok()?;
    let state_text = read_utf8(driver, &state_path).ok()??;
    let state: State = toml::from_str(&state_text).ok()?;
    let dir = format!("tasks/{id}/versions/{:06}", state.version);
    let text = read_utf8(driver, &DocPath::new(format!("{dir}/task.md")).ok()?).ok()??;
    let (front, _): (TaskFront, String) = split_front("task.md", &text).ok()?;
    if front.id != id {
        return None;
    }
    let has_plan = driver
        .stat(&DocPath::new(format!("{dir}/plan.md")).ok()?)
        .ok()?
        .is_some();
    Some(TaskMeta {
        front,
        state,
        has_plan,
        stamps: [
            driver.stat(&task_path).ok()?,
            driver.stat(&plan_path).ok()?,
            driver.stat(&state_path).ok()?,
        ],
    })
}

/// Reads the newest stored version of a task whose state file is not valid.
fn load_with_open_state(driver: &dyn StorageDriver, id: TaskId) -> Option<TaskMeta> {
    let [task_path, plan_path, state_path] = task_paths(id).ok()?;
    let dir = DocPath::new(format!("tasks/{id}/versions")).ok()?;
    let newest = driver.list(&dir).ok()?.into_iter().next_back()?;
    let text = read_utf8(driver, &dir.join(&newest).ok()?.join("task.md").ok()?).ok()??;
    let (front, _): (TaskFront, String) = split_front("task.md", &text).ok()?;
    if front.id != id {
        return None;
    }
    let state = State {
        version: newest.parse().ok()?,
        status: crate::model::Status::Todo,
        stage: String::new(),
        unblocked: false,
        loop_stopped: false,
        loop_until_done: false,
    };
    Some(TaskMeta {
        front,
        state,
        has_plan: driver.stat(&plan_path).ok()?.is_some(),
        stamps: [
            driver.stat(&task_path).ok()?,
            driver.stat(&plan_path).ok()?,
            driver.stat(&state_path).ok()?,
        ],
    })
}

/// The in-memory index of a project.
#[derive(Debug, Default)]
pub struct Index {
    /// All valid tasks.
    pub tasks: HashMap<TaskId, TaskMeta>,
    /// All task IDs, in time order.
    pub order: BTreeSet<TaskId>,
    /// The children of each task.
    pub children: HashMap<TaskId, BTreeSet<TaskId>>,
    /// The tasks that a task blocks.
    pub blocks: HashMap<TaskId, BTreeSet<TaskId>>,
    /// The tasks that link to a task with `relates`.
    pub related_from: HashMap<TaskId, BTreeSet<TaskId>>,
    /// The tasks that link to an artifact.
    pub artifact_users: HashMap<ArtifactId, BTreeSet<TaskId>>,
    /// The active and expired claims.
    pub claims: HashMap<TaskId, Claim>,
    /// The tasks with invalid files, with the problem.
    pub invalid: BTreeMap<TaskId, String>,
    /// The directories with a name that is not a valid ID.
    pub bad_names: Vec<String>,
    /// The offset of the next unread byte in the feed.
    pub feed_offset: u64,
    /// The start of the feed. It identifies the file.
    feed_head: Vec<u8>,
}

impl Index {
    fn link(&mut self, meta: &TaskMeta) {
        let id = meta.id();
        self.order.insert(id);
        if let Some(parent) = meta.front.parent {
            self.children.entry(parent).or_default().insert(id);
        }
        for target in meta.front.blocked_by.iter().filter_map(|link| link.local()) {
            self.blocks.entry(target).or_default().insert(id);
        }
        for target in meta.front.relates.iter().filter_map(|link| link.local()) {
            self.related_from.entry(target).or_default().insert(id);
        }
        for artifact in &meta.front.artifacts {
            self.artifact_users.entry(*artifact).or_default().insert(id);
        }
    }

    fn unlink(&mut self, id: TaskId) {
        let Some(meta) = self.tasks.remove(&id) else {
            return;
        };
        self.order.remove(&id);
        if let Some(parent) = meta.front.parent {
            remove_from(&mut self.children, parent, id);
        }
        for target in meta.front.blocked_by.iter().filter_map(|link| link.local()) {
            remove_from(&mut self.blocks, target, id);
        }
        for target in meta.front.relates.iter().filter_map(|link| link.local()) {
            remove_from(&mut self.related_from, target, id);
        }
        for artifact in &meta.front.artifacts {
            remove_from(&mut self.artifact_users, *artifact, id);
        }
    }

    /// Adds or replaces a task.
    pub fn insert(&mut self, meta: TaskMeta) {
        let id = meta.id();
        self.unlink(id);
        self.link(&meta);
        self.tasks.insert(id, meta);
    }

    /// Removes a task.
    pub fn remove(&mut self, id: TaskId) {
        self.unlink(id);
        self.claims.remove(&id);
        self.invalid.remove(&id);
    }

    /// Reloads one task from the disk. An invalid file keeps the last valid record.
    pub fn reload(&mut self, driver: &dyn StorageDriver, id: TaskId) {
        match load_task(driver, id) {
            Ok(loaded) => {
                self.invalid.remove(&id);
                self.insert(loaded.meta);
            }
            Err(error) if error.code() == lenka_plugin_api::ErrorCode::NotFound => self.remove(id),
            Err(error) => self.mark_invalid(driver, id, error.message()),
        }
    }

    /// Reports an invalid task, and keeps its last valid record.
    fn mark_invalid(&mut self, driver: &dyn StorageDriver, id: TaskId, problem: &str) {
        // A task with no valid state still counts as an open task, so that it holds its parent and
        // the tasks that it blocks.
        if let Some(meta) = load_last_valid(driver, id).or_else(|| load_with_open_state(driver, id))
        {
            self.insert(meta);
        }
        self.invalid.insert(id, problem.to_owned());
    }

    /// Reads the claims again.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub fn reload_claims(&mut self, driver: &dyn StorageDriver) -> Result<()> {
        let dir = DocPath::new(".runtime/claims")?;
        self.claims.clear();
        for name in driver.list(&dir)? {
            let Some(stem) = name.strip_suffix(".toml") else {
                continue;
            };
            let Ok(id) = TaskId::parse(stem) else {
                continue;
            };
            let Some(bytes) = driver.read(&dir.join(&name)?)? else {
                continue;
            };
            let parsed = String::from_utf8(bytes)
                .ok()
                .and_then(|text| toml::from_str::<Claim>(&text).ok());
            match parsed {
                Some(claim) if claim.task == id => {
                    self.claims.insert(id, claim);
                }
                _ => tracing::warn!(task = %id, "skipped an invalid claim file"),
            }
        }
        Ok(())
    }

    /// Returns the claim on a task if it is still active.
    #[must_use]
    pub fn active_claim(&self, id: TaskId, now: Timestamp) -> Option<&Claim> {
        self.claims.get(&id).filter(|claim| claim.expires_at > now)
    }

    /// Applies new lines of the change feed.
    ///
    /// # Errors
    ///
    /// Returns an error if the feed cannot be read.
    pub fn refresh(&mut self, driver: &dyn StorageDriver) -> Result<()> {
        let feed = DocPath::new(FEED_PATH)?;
        let len = driver.stat(&feed)?.map_or(0, |stat: DocStat| stat.len);
        if len == self.feed_offset {
            return Ok(());
        }
        // The feed can be lost and grow again, so the length alone does not identify the file.
        let head = driver.read_start(&feed, FEED_HEAD_BYTES)?;
        let shared = head.len().min(self.feed_head.len());
        let same_feed = head[..shared] == self.feed_head[..shared];
        self.feed_head = head;
        if len < self.feed_offset || !same_feed {
            self.feed_offset = 0;
            self.scan(driver)?;
            // A lost feed also lost the events of the claims, so the claim files can differ.
            self.reload_claims(driver)?;
            return Ok(());
        }
        let bytes = driver.read_from(&feed, self.feed_offset)?;
        // A writer can be in the middle of a line, so read only complete lines.
        let complete = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |i| i + 1);
        let mut claims_changed = false;
        let mut changed: HashSet<TaskId> = HashSet::new();
        // A bad line can hide the event of a writer, so only a full scan is sure to see the change.
        let mut rescan = false;
        for line in String::from_utf8_lossy(&bytes[..complete]).lines() {
            let Ok(event) = serde_json::from_str::<FeedEvent>(line) else {
                rescan = true;
                continue;
            };
            match event.kind {
                FeedKind::Task | FeedKind::State | FeedKind::TaskDeleted => {
                    if let Ok(id) = TaskId::parse(&event.id) {
                        changed.insert(id);
                    }
                }
                FeedKind::Claim => claims_changed = true,
                FeedKind::Reindex => rescan = true,
                FeedKind::Entry
                | FeedKind::Artifact
                | FeedKind::ArtifactDeleted
                | FeedKind::Memory => {}
            }
        }
        self.feed_offset += complete as u64;
        // No event is this long. Without this step, the read never reaches the end of the line.
        if complete == 0 && bytes.len() >= MAX_FEED_LINE_BYTES {
            self.feed_offset += bytes.len() as u64;
            rescan = true;
        }
        if rescan {
            let offset = self.feed_offset;
            self.scan(driver)?;
            self.feed_offset = offset;
            return self.reload_claims(driver);
        }
        for id in changed {
            self.reload(driver, id);
        }
        if claims_changed {
            self.reload_claims(driver)?;
        }
        Ok(())
    }

    /// Compares all stamps with the files, and reloads each changed task. Returns the reloaded
    /// tasks.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub fn scan(&mut self, driver: &dyn StorageDriver) -> Result<Vec<TaskId>> {
        let names = driver.list(&DocPath::new("tasks")?)?;
        let mut seen = HashSet::with_capacity(names.len());
        self.bad_names.clear();
        let mut ids = Vec::with_capacity(names.len());
        for name in names {
            match TaskId::parse(&name) {
                Ok(id) => {
                    seen.insert(id);
                    ids.push(id);
                }
                Err(_) => self.bad_names.push(name),
            }
        }
        // A task with invalid files can have no record, so it is only in the invalid list.
        let gone: Vec<TaskId> = self
            .order
            .iter()
            .chain(self.invalid.keys())
            .filter(|id| !seen.contains(id))
            .copied()
            .collect();
        for id in gone {
            self.remove(id);
        }
        let stale = self.stale_tasks(driver, &ids);
        let loaded = load_many(driver, &stale);
        for (id, result) in loaded {
            match result {
                Ok(task) => {
                    self.invalid.remove(&id);
                    // A copy moves the record out of the allocator area of the worker thread.
                    // The area then becomes free, which keeps the memory of a process low.
                    self.insert(task.meta.clone());
                }
                Err(error) if error.code() == lenka_plugin_api::ErrorCode::NotFound => {
                    self.remove(id);
                }
                Err(error) => self.mark_invalid(driver, id, error.message()),
            }
        }
        Ok(stale)
    }

    fn stale_tasks(&self, driver: &dyn StorageDriver, ids: &[TaskId]) -> Vec<TaskId> {
        let check = |chunk: &[TaskId]| -> Vec<TaskId> {
            chunk
                .iter()
                .filter(|id| {
                    let Some(meta) = self.tasks.get(id) else {
                        return true;
                    };
                    let Ok(paths) = task_paths(**id) else {
                        return true;
                    };
                    paths
                        .iter()
                        .zip(&meta.stamps)
                        .any(|(path, stamp)| driver.stat(path).ok().flatten() != *stamp)
                })
                .copied()
                .collect()
        };
        parallel_chunks(ids, &check).into_iter().flatten().collect()
    }

    /// Loads the snapshot if it is valid. A bad snapshot is not an error, because a scan can
    /// rebuild the index.
    pub fn load_snapshot(&mut self, driver: &dyn StorageDriver) {
        let Ok(path) = DocPath::new(SNAPSHOT_PATH) else {
            return;
        };
        let Ok(Some(bytes)) = driver.read(&path) else {
            return;
        };
        let Ok(snapshot) = serde_json::from_slice::<Snapshot>(&bytes) else {
            tracing::warn!("ignored an invalid index snapshot");
            return;
        };
        if snapshot.format != SNAPSHOT_FORMAT {
            return;
        }
        for meta in snapshot.tasks {
            self.insert(meta);
        }
    }

    /// Stores the snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the write fails.
    pub fn save_snapshot(&self, project: &Project) -> Result<()> {
        let snapshot = SnapshotRef {
            format: SNAPSHOT_FORMAT,
            // An invalid task stays out, so that the next process reads its files and finds the
            // problem again. Its record has the stamps of the invalid files.
            tasks: self
                .tasks
                .iter()
                .filter(|(id, _)| !self.invalid.contains_key(id))
                .map(|(_, meta)| meta)
                .collect(),
        };
        let bytes = serde_json::to_vec(&snapshot)
            .map_err(|err| LenkaError::invalid_file(format!("Snapshot: {err}")))?;
        project.put_cache(&DocPath::new(SNAPSHOT_PATH)?, &bytes)
    }

    /// Finds relation cycles in the project. A Git merge can create one.
    #[must_use]
    pub fn find_cycles(&self) -> Vec<Vec<TaskId>> {
        let mut state: HashMap<TaskId, u8> = HashMap::with_capacity(self.tasks.len());
        let mut cycles = Vec::new();
        for start in &self.order {
            if state.contains_key(start) {
                continue;
            }
            // An iterative walk, because a hostile file can make a very deep chain.
            let mut stack: Vec<(TaskId, Vec<TaskId>)> = vec![(*start, self.wait_edges(*start))];
            let mut path = vec![*start];
            state.insert(*start, 1);
            while let Some((_, pending)) = stack.last_mut() {
                if let Some(next) = pending.pop() {
                    match state.get(&next) {
                        None => {
                            state.insert(next, 1);
                            path.push(next);
                            let edges = self.wait_edges(next);
                            stack.push((next, edges));
                        }
                        Some(1) => {
                            if let Some(position) = path.iter().position(|id| *id == next) {
                                cycles.push(path[position..].to_vec());
                            }
                        }
                        Some(_) => {}
                    }
                } else if let Some((done, _)) = stack.pop() {
                    state.insert(done, 2);
                    path.pop();
                }
            }
        }
        cycles
    }

    /// The tasks that must close before a task can close: its blockers, the blockers of its
    /// ancestors, which it inherits, and its children.
    #[must_use]
    pub fn wait_edges(&self, id: TaskId) -> Vec<TaskId> {
        let mut edges: Vec<TaskId> = Vec::new();
        for task in std::iter::once(id).chain(self.ancestors(id)) {
            if let Some(meta) = self.tasks.get(&task) {
                edges.extend(
                    meta.front
                        .blocked_by
                        .iter()
                        .filter_map(|link| link.local())
                        .filter(|target| self.tasks.contains_key(target)),
                );
            }
        }
        if let Some(children) = self.children.get(&id) {
            edges.extend(children.iter().copied());
        }
        edges
    }

    /// Tells if `from` waits for `to`, also through other tasks. A task waits for itself.
    #[must_use]
    pub fn waits_for(&self, from: TaskId, to: TaskId) -> bool {
        let mut seen = HashSet::new();
        let mut stack = vec![from];
        while let Some(current) = stack.pop() {
            if current == to {
                return true;
            }
            if seen.insert(current) {
                stack.extend(self.wait_edges(current));
            }
        }
        false
    }

    /// Returns a task and all its descendants.
    #[must_use]
    pub fn with_descendants(&self, id: TaskId) -> Vec<TaskId> {
        let mut seen = HashSet::new();
        let mut stack = vec![id];
        let mut result = Vec::new();
        while let Some(current) = stack.pop() {
            if seen.insert(current) {
                result.push(current);
                if let Some(children) = self.children.get(&current) {
                    stack.extend(children.iter().copied());
                }
            }
        }
        result
    }

    /// Tells if a new blocker `target` on `id` makes a group of tasks wait for itself. All
    /// descendants of `id` inherit the blocker.
    #[must_use]
    pub fn blocker_makes_cycle(&self, id: TaskId, target: TaskId) -> bool {
        self.with_descendants(id)
            .into_iter()
            .any(|task| self.waits_for(target, task))
    }

    /// Tells if a new parent `parent` on `id` makes a group of tasks wait for itself. The parent
    /// then waits for `id`, and `id` and its descendants inherit the blockers of the parent.
    #[must_use]
    pub fn parent_makes_cycle(&self, id: TaskId, parent: TaskId) -> bool {
        if self.waits_for(id, parent) {
            return true;
        }
        let inherited: Vec<TaskId> = std::iter::once(parent)
            .chain(self.ancestors(parent))
            .filter_map(|task| self.tasks.get(&task))
            .flat_map(|meta| meta.front.blocked_by.iter().filter_map(|link| link.local()))
            .collect();
        let tasks = self.with_descendants(id);
        inherited
            .iter()
            .any(|blocker| tasks.iter().any(|task| self.waits_for(*blocker, *task)))
    }

    /// Returns the ancestors of a task, nearest first. A loop ends the walk.
    #[must_use]
    pub fn ancestors(&self, id: TaskId) -> Vec<TaskId> {
        let mut result = Vec::new();
        let mut current = id;
        while result.len() < MAX_DEPTH {
            let Some(parent) = self.tasks.get(&current).and_then(|meta| meta.front.parent) else {
                break;
            };
            if parent == id || result.contains(&parent) {
                break;
            }
            result.push(parent);
            current = parent;
        }
        result
    }

    /// Returns the local links of a task in both directions, without the task itself.
    #[must_use]
    pub fn neighbors(&self, id: TaskId) -> BTreeSet<TaskId> {
        let mut result = BTreeSet::new();
        if let Some(meta) = self.tasks.get(&id) {
            result.extend(meta.front.parent);
            let links = meta.front.blocked_by.iter().chain(&meta.front.relates);
            result.extend(links.filter_map(|link: &Link| link.local()));
            for artifact in &meta.front.artifacts {
                if let Some(users) = self.artifact_users.get(artifact) {
                    result.extend(users.iter().copied());
                }
            }
        }
        for map in [&self.children, &self.blocks, &self.related_from] {
            if let Some(items) = map.get(&id) {
                result.extend(items.iter().copied());
            }
        }
        result.remove(&id);
        result
    }
}

fn remove_from<K: std::hash::Hash + Eq>(
    map: &mut HashMap<K, BTreeSet<TaskId>>,
    key: K,
    id: TaskId,
) {
    if let Some(items) = map.get_mut(&key) {
        items.remove(&id);
        if items.is_empty() {
            map.remove(&key);
        }
    }
}

fn load_many(driver: &dyn StorageDriver, ids: &[TaskId]) -> Vec<(TaskId, Result<LoadedTask>)> {
    let load = |chunk: &[TaskId]| -> Vec<(TaskId, Result<LoadedTask>)> {
        chunk
            .iter()
            .map(|id| (*id, load_task(driver, *id)))
            .collect()
    };
    parallel_chunks(ids, &load).into_iter().flatten().collect()
}

/// Runs work over chunks on some threads. A small input stays on the current thread.
fn parallel_chunks<T: Send + Sync, R: Send>(
    items: &[T],
    work: &(dyn Fn(&[T]) -> R + Sync),
) -> Vec<R> {
    const SMALL: usize = 256;
    if items.len() <= SMALL {
        return vec![work(items)];
    }
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(8);
    let size = items.len().div_ceil(threads);
    std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(size)
            .map(|chunk| scope.spawn(move || work(chunk)))
            .collect();
        handles
            .into_iter()
            // A lost part of a scan would look like a good scan, so a panic must continue.
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    })
}

/// Loads the index for a project: the snapshot, then a compare with the files.
///
/// # Errors
///
/// Returns an error if the files cannot be read.
pub fn open_index(project: &Arc<Project>) -> Result<(Index, Vec<TaskId>)> {
    let driver = project.driver();
    let mut index = Index::default();
    let feed = DocPath::new(FEED_PATH)?;
    index.feed_offset = driver.stat(&feed)?.map_or(0, |stat| stat.len);
    index.feed_head = driver.read_start(&feed, FEED_HEAD_BYTES)?;
    index.load_snapshot(driver);
    let changed = index.scan(driver)?;
    index.reload_claims(driver)?;
    if !changed.is_empty() {
        index.save_snapshot(project)?;
    }
    Ok((index, changed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "a bug in a worker")]
    fn a_panic_in_a_worker_does_not_look_like_a_good_scan() {
        let items = vec![0_u8; 1024];
        let work = |_: &[u8]| -> u8 { panic!("a bug in a worker") };
        let _ = parallel_chunks(&items, &work);
    }
}
