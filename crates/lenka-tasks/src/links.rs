// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Soft links between projects. A link shows only the title, status, and stage of its target.

use std::collections::HashMap;
use std::sync::Arc;

use lenka_core::settings::{PROJECT_FILE, ProjectConfig};
use lenka_core::{PeerLookup, Project};
use lenka_plugin_api::{DocPath, ProjectId, Result, StorageDriver, TaskId};
use serde::Serialize;

use crate::markdown::split_front;
use crate::model::{Link, State, Status, TaskFront};

/// The state of a link to another project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkState {
    /// Lenka can read the target.
    Ok,
    /// This machine does not have the project. A `blocked_by` link still blocks.
    Unavailable,
    /// The target is deleted, or a policy does not allow the link. A `blocked_by` link no longer
    /// blocks. The result does not show the cause, so a link cannot reveal that a task exists.
    Broken,
}

/// The visible data of a link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkView {
    /// The stored link.
    pub link: String,
    /// The link state.
    pub state: LinkState,
    /// The target title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The target status.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<Status>,
    /// The target stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// The target version. Change detection uses it, and results do not show it.
    #[serde(skip)]
    pub version: u64,
    /// Tells if the target completed its `unblocks` stage.
    #[serde(skip)]
    pub unblocked: bool,
}

impl LinkView {
    fn empty(link: Link, state: LinkState) -> Self {
        Self {
            link: link.to_string(),
            state,
            title: None,
            status: None,
            stage: None,
            version: 0,
            unblocked: false,
        }
    }

    /// Tells if this link is an active blocker (section 2.9 and 2.11 of the design).
    #[must_use]
    pub fn blocks(&self) -> bool {
        match self.state {
            LinkState::Unavailable => true,
            LinkState::Broken => false,
            LinkState::Ok => !(self.unblocked || self.status.is_some_and(Status::is_closed)),
        }
    }
}

/// The storage and the settings of another project.
type Peer = (Arc<dyn StorageDriver>, ProjectConfig);

/// A short-lived cache for one query, so that many links read each project once.
#[derive(Debug, Default)]
pub struct LinkReader {
    peers: HashMap<ProjectId, Option<Peer>>,
    views: HashMap<Link, LinkView>,
}

impl LinkReader {
    /// Reads the visible data of a link from another project.
    pub fn view(&mut self, project: &Project, link: Link) -> LinkView {
        if let Some(found) = self.views.get(&link) {
            return found.clone();
        }
        let view = match link {
            Link::Local(task) => LinkView::empty(Link::Local(task), LinkState::Ok),
            Link::Remote(target, task) => self.read_remote(project, link, target, task),
        };
        self.views.insert(link, view.clone());
        view
    }

    fn read_remote(
        &mut self,
        project: &Project,
        link: Link,
        target: ProjectId,
        task: TaskId,
    ) -> LinkView {
        let peer = self
            .peers
            .entry(target)
            .or_insert_with(|| open_peer(project, target));
        let Some((driver, config)) = peer else {
            return LinkView::empty(link, LinkState::Unavailable);
        };
        let source = project.config();
        if !source.links.allows_link_to(target) || !config.links.accepts_from(source.id) {
            return LinkView::empty(link, LinkState::Broken);
        }
        match read_visible(driver.as_ref(), task) {
            Some((front, state)) => LinkView {
                link: link.to_string(),
                state: LinkState::Ok,
                title: Some(front.title),
                status: Some(state.status),
                stage: Some(state.stage),
                version: state.version,
                unblocked: state.unblocked,
            },
            None => LinkView::empty(link, LinkState::Broken),
        }
    }
}

fn open_peer(project: &Project, target: ProjectId) -> Option<Peer> {
    let PeerLookup::Found(driver) = project.peers().lookup(target).ok()? else {
        return None;
    };
    let path = DocPath::new(PROJECT_FILE).ok()?;
    let bytes = driver.read(&path).ok()??;
    let text = String::from_utf8(bytes).ok()?;
    let config = ProjectConfig::parse(&text).ok()?;
    (config.id == target).then_some((driver, config))
}

fn read_visible(driver: &dyn StorageDriver, task: TaskId) -> Option<(TaskFront, State)> {
    let root = format!("tasks/{task}");
    let text = read_text(driver, &format!("{root}/task.md"))?;
    let (front, _): (TaskFront, String) = split_front("task.md", &text).ok()?;
    let state_text = read_text(driver, &format!("{root}/state.toml"))?;
    let state: State = toml::from_str(&state_text).ok()?;
    (front.id == task).then_some((front, state))
}

fn read_text(driver: &dyn StorageDriver, path: &str) -> Option<String> {
    let path = DocPath::new(path).ok()?;
    String::from_utf8(driver.read(&path).ok()??).ok()
}

/// Checks a new link against the policy of both sides. A project that this machine does not have
/// can only pass the check of this side.
///
/// # Errors
///
/// Returns [`lenka_plugin_api::ErrorCode::LinkDenied`] if a policy does not allow the link, or if
/// the target does not exist. The message is the same for both cases.
pub fn check_new_link(project: &Project, target: ProjectId, task: TaskId) -> Result<()> {
    let denied = || {
        lenka_plugin_api::LenkaError::new(
            lenka_plugin_api::ErrorCode::LinkDenied,
            "The link is not allowed, or the target does not exist. An admin of each project can set [links] in lenka.toml.",
        )
    };
    let source = project.config();
    if !source.links.allows_link_to(target) {
        return Err(denied());
    }
    let Some((driver, config)) = open_peer(project, target) else {
        return Ok(());
    };
    if !config.links.accepts_from(source.id) || read_visible(driver.as_ref(), task).is_none() {
        return Err(denied());
    }
    Ok(())
}
