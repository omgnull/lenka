// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! A temporary project and a harness that runs the real UI with no window.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Pos2, Theme, UiBuilder, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use lenka_core::init::{Location, create_project};
use lenka_core::{Builder, Host, Paths};
use lenka_plugin_api::{Actor, Caller, ProjectScope};
use lenka_tasks::{TaskService, service_of};

use crate::app::DesktopApp;

/// A temporary directory with isolated lenka paths.
pub(crate) struct Fixture {
    _temp: tempfile::TempDir,
    pub root: PathBuf,
    pub paths: Paths,
}

/// The name of the project of the tests.
pub(crate) const PROJECT: &str = "demo";

impl Fixture {
    /// Creates isolated paths with no project.
    pub(crate) fn empty() -> Self {
        let temp = tempfile::tempdir().expect("a temporary directory");
        let root = temp.path().to_path_buf();
        let paths = Paths::new(root.join("config"), root.join("data"));
        std::fs::create_dir_all(root.join("work")).expect("a work directory");
        Self {
            _temp: temp,
            root,
            paths,
        }
    }

    /// Creates isolated paths with one project.
    pub(crate) fn with_project() -> Self {
        let fixture = Self::empty();
        create_project(
            &fixture.paths,
            &fixture.root.join("work"),
            Some(PROJECT),
            &Location::Shared,
        )
        .expect("a project");
        fixture
    }

    /// Builds a host on the paths. Two hosts act as two processes.
    pub(crate) fn host(&self) -> Host {
        Builder::new()
            .plugin(lenka_driver_local::plugin())
            .plugin(lenka_artifact_fs::plugin())
            .plugin(lenka_tasks::plugin())
            .paths(self.paths.clone())
            .build()
            .expect("a host")
    }

    fn caller(&self, actor: Actor, label: &str, directory: &str) -> Caller {
        let worktree = self.root.join(directory);
        let agent_key = lenka_core::notices::agent_key(&worktree, None);
        Caller {
            actor,
            label: label.to_owned(),
            worktree,
            agent_key,
            scope: ProjectScope::All,
            default_project: None,
        }
    }

    /// Returns the caller of the desktop app.
    pub(crate) fn person(&self) -> Caller {
        self.caller(Actor::Human, "human:igor", "work")
    }

    /// Returns an agent caller.
    pub(crate) fn agent(&self) -> Caller {
        self.caller(Actor::Agent, "agent:claude-code", "agent-work")
    }

    /// Opens the task service of a second process.
    pub(crate) fn other_process(&self) -> (Host, Arc<TaskService>) {
        let host = self.host();
        let project = host.open_by_name(PROJECT).expect("the project");
        let tasks = service_of(&project).expect("the service");
        (host, tasks)
    }
}

/// The state of a harness: the app starts in the first frame, because it needs the context.
pub(crate) struct TestApp {
    start: Option<(Host, Caller)>,
    pub app: Option<DesktopApp>,
    /// Keeps the temporary directories until the end of the test.
    _keep: Arc<Fixture>,
}

impl TestApp {
    /// Returns the app.
    pub(crate) fn app(&self) -> &DesktopApp {
        self.app.as_ref().expect("the first frame ran")
    }

    /// Returns the app for a change.
    pub(crate) fn app_mut(&mut self) -> &mut DesktopApp {
        self.app.as_mut().expect("the first frame ran")
    }
}

/// Starts the real UI on a fixture.
///
/// The UI has no font here, so text shows as boxes. The tests find widgets by accessible names.
pub(crate) fn start(fixture: &Arc<Fixture>, size: Vec2, theme: Theme) -> Harness<'static, TestApp> {
    start_with(fixture, size, theme, |builder| {
        *builder = std::mem::take(builder).allow_missing_glyphs();
    })
}

/// Starts the real UI, and changes the harness builder.
pub(crate) fn start_with(
    fixture: &Arc<Fixture>,
    size: Vec2,
    theme: Theme,
    change: impl FnOnce(&mut egui_kittest::HarnessBuilder<TestApp>),
) -> Harness<'static, TestApp> {
    let state = TestApp {
        start: Some((fixture.host(), fixture.person())),
        app: None,
        _keep: Arc::clone(fixture),
    };
    let mut builder = Harness::builder();
    builder = builder.with_size(size).with_theme(theme).with_max_steps(16);
    change(&mut builder);
    let mut harness = builder.build_ui_state(
        |ui, state: &mut TestApp| {
            let area = ui.ctx().content_rect();
            let mut full = ui.new_child(UiBuilder::new().max_rect(area));
            if let Some((host, caller)) = state.start.take() {
                state.app = Some(DesktopApp::new(full.ctx(), host, caller, None));
            }
            if let Some(app) = &mut state.app {
                app.show(&mut full);
            }
        },
        state,
    );
    harness.run();
    harness
}

/// Drags with the pointer from one place to another, and drops.
pub(crate) fn drag(harness: &mut Harness<'_, TestApp>, from: Pos2, to: Pos2) {
    harness.hover_at(from);
    harness.step();
    harness.drag_at(from);
    harness.step();
    let middle = from + (to - from) * 0.5;
    harness.hover_at(middle);
    harness.step();
    harness.hover_at(to);
    harness.step();
    harness.drop_at(to);
    harness.run();
}

/// Runs frames until a condition holds, in real time, for the work of the background thread.
pub(crate) fn wait_until(
    harness: &mut Harness<'_, TestApp>,
    what: &str,
    condition: impl Fn(&Harness<'_, TestApp>) -> bool,
) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        harness.step();
        if condition(harness) {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the wait for this condition ended with no result: {what}");
}

/// Tells if a node with exactly this name exists.
pub(crate) fn has_label(harness: &Harness<'_, TestApp>, label: &str) -> bool {
    harness.query_by_label(label).is_some()
}
