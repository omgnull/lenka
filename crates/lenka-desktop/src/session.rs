// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The open project, its cached rows, and the background thread that finds changes.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use lenka_artifacts::{ArtifactService, artifacts_of};
use lenka_core::notices::FEED_PATH;
use lenka_core::{Host, Project};
use lenka_plugin_api::{DocPath, Result};
use lenka_tasks::{TaskService, service_of};

use crate::data::BoardModel;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// A claim can end by its timeout with no feed event, so the board reloads at this interval while
/// it shows a claim.
const CLAIM_RECHECK_POLLS: u32 = 60;

/// Returns the length of the change feed. The feed only grows, so a new length means a change.
///
/// Each read of the service also consumes new feed lines, so the result of `refresh` alone cannot
/// tell the UI thread about a change that an earlier read found.
fn feed_length(project: &Project) -> u64 {
    let Ok(feed) = DocPath::new(FEED_PATH) else {
        return 0;
    };
    project
        .driver()
        .stat(&feed)
        .ok()
        .flatten()
        .map_or(0, |stat| stat.len)
}

#[derive(Debug, Default)]
struct Shared {
    stop: AtomicBool,
    /// The newest feed length that the thread found.
    feed: AtomicU64,
    /// Tells the UI thread to reload with no new feed line.
    recheck: AtomicBool,
    /// Tells the thread that the UI shows at least one claim.
    claims_visible: AtomicBool,
    error: Mutex<Option<String>>,
}

/// The background thread. It wakes each second, and asks for a repaint only after a change.
#[derive(Debug)]
struct Watcher {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Watcher {
    fn start(
        project: Arc<Project>,
        tasks: Arc<TaskService>,
        ctx: egui::Context,
        seen: u64,
    ) -> Self {
        let shared = Arc::new(Shared {
            feed: AtomicU64::new(seen),
            ..Shared::default()
        });
        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("lenka-watcher".to_owned())
            .spawn(move || watch(&project, &tasks, &ctx, &thread_shared))
            .ok();
        Self { shared, thread }
    }
}

fn watch(project: &Project, tasks: &TaskService, ctx: &egui::Context, shared: &Shared) {
    let mut polls = 0_u32;
    let mut last_error = None::<String>;
    while !shared.stop.load(Ordering::Relaxed) {
        thread::park_timeout(POLL_INTERVAL);
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
        let error = tasks.refresh().err().map(|err| err.to_string());
        if error != last_error {
            last_error.clone_from(&error);
            if error.is_some() {
                *shared.error.lock().unwrap_or_else(PoisonError::into_inner) = error;
                ctx.request_repaint();
            }
        }
        let length = feed_length(project);
        if shared.feed.swap(length, Ordering::Relaxed) != length {
            ctx.request_repaint();
        }
        polls += 1;
        if polls >= CLAIM_RECHECK_POLLS {
            polls = 0;
            if shared.claims_visible.load(Ordering::Relaxed) {
                shared.recheck.store(true, Ordering::Relaxed);
                ctx.request_repaint();
            }
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ignored = thread.join();
        }
    }
}

/// An open project with the cached rows of the board.
#[derive(Debug)]
pub(crate) struct Session {
    pub name: String,
    pub project: Arc<Project>,
    pub tasks: Arc<TaskService>,
    pub artifacts: Arc<ArtifactService>,
    pub model: BoardModel,
    /// Grows with each reload. Views compare it to find out if their caches are old.
    pub generation: u64,
    /// The feed length of the current rows.
    seen_feed: u64,
    watcher: Watcher,
}

impl Session {
    /// Opens a project, and starts the background thread.
    pub(crate) fn open(host: &Host, name: &str, ctx: &egui::Context) -> Result<Self> {
        let project = host.open_by_name(name)?;
        let tasks = service_of(&project)?;
        let artifacts = artifacts_of(&project)?;
        let seen_feed = feed_length(&project);
        let model = BoardModel::new(tasks.board_rows()?);
        let watcher = Watcher::start(
            Arc::clone(&project),
            Arc::clone(&tasks),
            ctx.clone(),
            seen_feed,
        );
        let session = Self {
            name: name.to_owned(),
            project,
            tasks,
            artifacts,
            model,
            generation: 1,
            seen_feed,
            watcher,
        };
        session.note_claims();
        Ok(session)
    }

    fn note_claims(&self) {
        let any = self.model.rows.iter().any(|row| row.claimed_by.is_some());
        self.watcher
            .shared
            .claims_visible
            .store(any, Ordering::Relaxed);
    }

    /// Reads all rows again.
    pub(crate) fn reload(&mut self) -> Result<()> {
        // Read the length first, so that a change during the read causes one more reload.
        let length = feed_length(&self.project);
        self.model = BoardModel::new(self.tasks.board_rows()?);
        self.seen_feed = length;
        // The thread can hold a newer length. It finds the difference at its next poll.
        self.watcher.shared.feed.store(length, Ordering::Relaxed);
        self.generation += 1;
        self.note_claims();
        Ok(())
    }

    /// Tells if the rows are old. Each frame can call this, because it only reads atomic values.
    pub(crate) fn has_news(&self) -> bool {
        let shared = &self.watcher.shared;
        shared.recheck.swap(false, Ordering::Relaxed)
            || shared.feed.load(Ordering::Relaxed) != self.seen_feed
    }

    /// Returns an error from the background thread, once.
    pub(crate) fn take_error(&self) -> Option<String> {
        self.watcher
            .shared
            .error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}
