// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Tests that drive the real UI with a temporary project and no window.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Theme, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use lenka_artifacts::{DRAWING_MEDIA_TYPE, artifacts_of};
use lenka_core::{SecretPolicy, Stage, TypeDef};
use lenka_plugin_api::{Actor, Day, TaskId, Timestamp};
use lenka_tasks::{
    BoardRow, ClaimRef, EntryKind, NewTask, Priority, State, Status, TaskFront, TaskMeta, TaskPatch,
};

use crate::actions::View;
use crate::board::BoardState;
use crate::data::BoardModel;
use crate::drawing::model::Document;
use crate::fonts::load_system;
use crate::gantt::GanttState;
use crate::test_support::{Fixture, TestApp, drag, has_label, start, start_with, wait_until};
use crate::tree::TreeState;

const SIZE: Vec2 = Vec2::new(1500.0, 950.0);

fn new_task(title: &str) -> NewTask {
    NewTask {
        title: title.to_owned(),
        description: "The first text.".to_owned(),
        ..NewTask::default()
    }
}

/// Starts the UI on a project with some tasks. It returns the IDs of the tasks.
fn start_with_tasks(titles: &[&str]) -> (Arc<Fixture>, Harness<'static, TestApp>, Vec<TaskId>) {
    let fixture = Arc::new(Fixture::with_project());
    let (_host, tasks) = fixture.other_process();
    let person = fixture.person();
    let ids = titles
        .iter()
        .map(|title| tasks.create(&person, new_task(title)).expect("a task").id())
        .collect();
    let harness = start(&fixture, SIZE, Theme::Light);
    (fixture, harness, ids)
}

fn status_of(fixture: &Fixture, id: TaskId) -> Status {
    let (_host, tasks) = fixture.other_process();
    tasks.detail(id, &[]).expect("the task").meta.state.status
}

fn move_card(harness: &mut Harness<'_, TestApp>, title: &str, column: &str) {
    let from = harness.get_by_label(title).rect().center();
    let to = harness
        .get_by_label_contains(&format!("{column} column"))
        .rect()
        .center();
    drag(harness, from, to + Vec2::new(0.0, 120.0));
}

fn click(harness: &mut Harness<'_, TestApp>, label: &str) {
    harness.get_by_label(label).click();
    harness.run();
}

fn type_into(harness: &mut Harness<'_, TestApp>, label: &str, text: &str) {
    harness.get_by_label(label).click();
    harness.run();
    harness.get_by_label(label).type_text(text);
    harness.run();
}

#[test]
fn with_no_projects_the_start_screen_shows_and_creates_a_project() {
    let fixture = Arc::new(Fixture::empty());
    let mut harness = start(&fixture, SIZE, Theme::Light);
    assert!(has_label(&harness, "Welcome to lenka"));
    click(&mut harness, "Create a project");
    type_into(&mut harness, "Project name", "api");
    click(&mut harness, "Create project");
    assert_eq!(
        harness
            .state()
            .app()
            .session
            .as_ref()
            .map(|session| session.name.as_str()),
        Some("api")
    );
    assert!(has_label(&harness, "This project has no tasks yet"));
}

#[test]
fn a_task_from_the_dialog_becomes_a_card() {
    let fixture = Arc::new(Fixture::with_project());
    let mut harness = start(&fixture, SIZE, Theme::Light);
    click(&mut harness, "New task");
    type_into(&mut harness, "Title", "Parse the config file");
    click(&mut harness, "Create task");
    assert!(harness.state().app().dialog.is_none());
    let (_host, tasks) = fixture.other_process();
    let rows = tasks.board_rows().expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].meta.front.title, "Parse the config file");
    assert!(harness.query_all_by_label("Parse the config file").count() >= 1);
    assert!(has_label(&harness, "Todo column, 1 tasks"));
}

#[test]
fn a_dialog_shows_the_error_of_an_empty_title_and_stays_open() {
    let fixture = Arc::new(Fixture::with_project());
    let mut harness = start(&fixture, SIZE, Theme::Light);
    click(&mut harness, "New task");
    click(&mut harness, "Create task");
    assert!(harness.state().app().dialog.is_some());
    assert!(harness.query_by_label_contains("needs a title").is_some());
}

#[test]
fn a_drag_moves_a_card_to_another_column() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    move_card(&mut harness, "Write the parser", "In progress");
    assert_eq!(status_of(&fixture, ids[0]), Status::InProgress);
    assert!(has_label(&harness, "In progress column, 1 tasks"));
    assert!(has_label(&harness, "Todo column, 0 tasks"));
}

#[test]
fn a_drop_on_done_asks_for_evidence_and_finishes() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    move_card(&mut harness, "Write the parser", "Done");
    assert_eq!(
        status_of(&fixture, ids[0]),
        Status::Todo,
        "no change before the evidence"
    );
    assert!(has_label(&harness, "Finish the task"));
    type_into(&mut harness, "Evidence", "cargo test passed");
    click(&mut harness, "Finish task");
    assert_eq!(status_of(&fixture, ids[0]), Status::Done);
    assert!(has_label(&harness, "Done column, 1 tasks"));
    let (_host, tasks) = fixture.other_process();
    let detail = tasks.detail(ids[0], &["entries"]).expect("detail");
    assert!(
        detail
            .entries
            .iter()
            .any(|entry| entry.kind == EntryKind::Evidence && entry.text.contains("cargo test"))
    );
}

#[test]
fn a_claim_of_another_holder_asks_for_confirmation_and_overrides() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    let (_host, tasks) = fixture.other_process();
    tasks
        .claim(&fixture.agent(), Some(ids[0]), None, None)
        .expect("a claim");
    wait_until(&mut harness, "the claim shows", |harness| {
        harness
            .state()
            .app()
            .session
            .as_ref()
            .is_some_and(|session| session.model.rows[0].claimed_by.is_some())
    });
    move_card(&mut harness, "Write the parser", "Paused");
    assert!(has_label(&harness, "Override this stop?"));
    assert_eq!(
        status_of(&fixture, ids[0]),
        Status::InProgress,
        "no change before the confirmation"
    );
    click(&mut harness, "Override and continue");
    assert_eq!(status_of(&fixture, ids[0]), Status::Paused);
}

fn open_detail(harness: &mut Harness<'_, TestApp>, title: &str) {
    harness.get_by_label(title).click();
    harness.run();
    assert!(
        harness.state().app().detail.is_some(),
        "the detail panel opened"
    );
}

#[test]
fn an_edit_with_a_reason_saves_a_material_change() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_detail(&mut harness, "Write the parser");
    type_into(&mut harness, "Description", " More text.");
    assert!(has_label(&harness, "Unsaved changes"));
    click(&mut harness, "Save");
    assert!(has_label(&harness, "Why did this change?"));
    type_into(&mut harness, "Reason", "The scope grew");
    click(&mut harness, "Save with reason");
    let (_host, tasks) = fixture.other_process();
    let detail = tasks.detail(ids[0], &["versions"]).expect("detail");
    assert!(
        detail.description.contains("More text."),
        "{}",
        detail.description
    );
    let newest = detail
        .versions
        .iter()
        .max_by_key(|version| version.version)
        .expect("a version");
    assert_eq!(newest.reason, "The scope grew");
    assert!(harness.state().app().dialog.is_none());
    assert!(!has_label(&harness, "Unsaved changes"));
}

#[test]
fn a_stale_version_keeps_the_text_and_offers_a_reload() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_detail(&mut harness, "Write the parser");
    type_into(&mut harness, "Description", " Mine.");
    let (_host, tasks) = fixture.other_process();
    let patch = TaskPatch {
        expected_version: 1,
        description: Some("The text of an agent.".to_owned()),
        reason: Some("A new fact".to_owned()),
        ..TaskPatch::default()
    };
    tasks
        .update(&fixture.agent(), ids[0], patch)
        .expect("an update");
    click(&mut harness, "Save");
    if has_label(&harness, "Why did this change?") {
        type_into(&mut harness, "Reason", "My reason");
        click(&mut harness, "Save with reason");
    }
    assert!(has_label(&harness, "Another writer saved this task first."));
    let draft = harness
        .state()
        .app()
        .detail
        .as_ref()
        .map(|detail| detail.draft.description.clone());
    assert!(
        draft.is_some_and(|text| text.contains("Mine.")),
        "the text stays"
    );
    click(&mut harness, "Reload");
    assert!(!has_label(
        &harness,
        "Another writer saved this task first."
    ));
    let draft = harness
        .state()
        .app()
        .detail
        .as_ref()
        .map(|detail| detail.draft.description.clone());
    assert!(
        draft.is_some_and(|text| text.contains("Mine.")),
        "the text stays after the reload"
    );
    click(&mut harness, "Save");
    if has_label(&harness, "Why did this change?") {
        type_into(&mut harness, "Reason", "My reason");
        click(&mut harness, "Save with reason");
    }
    let detail = tasks.detail(ids[0], &[]).expect("detail");
    assert!(
        detail.description.contains("Mine."),
        "{}",
        detail.description
    );
}

#[test]
fn a_secret_needs_an_explicit_choice_of_the_person() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_detail(&mut harness, "Write the parser");
    let secret = concat!("ghp_", "abcdefghijklmnopqrstuvwxyz0123456789");
    type_into(&mut harness, "Description", &format!(" {secret}"));
    click(&mut harness, "Save");
    type_into(&mut harness, "Reason", "Add the token");
    click(&mut harness, "Save with reason");
    let (_host, tasks) = fixture.other_process();
    assert!(
        !tasks
            .detail(ids[0], &[])
            .expect("detail")
            .description
            .contains(secret),
        "no save yet"
    );
    assert!(has_label(&harness, "Save with this value"));
    click(&mut harness, "Save with this value");
    tasks.refresh().expect("a refresh");
    assert!(
        tasks
            .detail(ids[0], &[])
            .expect("detail")
            .description
            .contains(secret)
    );
}

#[test]
fn a_section_heading_in_a_field_stops_the_save_and_keeps_the_text() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_detail(&mut harness, "Write the parser");
    type_into(&mut harness, "Description", "\n## Goal\nMine.");
    click(&mut harness, "Save");
    assert!(
        harness
            .query_by_label_contains("begins a new section")
            .is_some()
    );
    let (_host, tasks) = fixture.other_process();
    assert_eq!(
        tasks
            .detail(ids[0], &[])
            .expect("detail")
            .description
            .trim(),
        "The first text."
    );
    assert!(
        has_label(&harness, "Unsaved changes"),
        "the text stays in the editor"
    );
}

#[test]
fn a_write_of_another_process_shows_live() {
    let (fixture, mut harness, _ids) = start_with_tasks(&["First task"]);
    let (_host, tasks) = fixture.other_process();
    tasks
        .create(&fixture.agent(), new_task("A task from an agent"))
        .expect("a task");
    wait_until(&mut harness, "the new card shows", |harness| {
        has_label(harness, "A task from an agent")
    });
    assert!(has_label(&harness, "Todo column, 2 tasks"));
}

#[test]
fn a_change_of_the_open_task_shows_live_and_keeps_a_draft() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_detail(&mut harness, "Write the parser");
    let (_host, tasks) = fixture.other_process();
    let patch = TaskPatch {
        expected_version: 1,
        title: Some("Write the new parser".to_owned()),
        ..TaskPatch::default()
    };
    tasks
        .update(&fixture.agent(), ids[0], patch)
        .expect("an update");
    wait_until(&mut harness, "the new title shows", |harness| {
        harness.query_all_by_label("Write the new parser").count() > 0
    });
}

#[test]
fn a_comment_goes_into_the_entries() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_detail(&mut harness, "Write the parser");
    click(&mut harness, "Entries");
    type_into(&mut harness, "Entry text", "Check the error path");
    click(&mut harness, "Add entry");
    let (_host, tasks) = fixture.other_process();
    let detail = tasks.detail(ids[0], &["entries"]).expect("detail");
    assert_eq!(detail.entries.len(), 1);
    assert_eq!(detail.entries[0].text, "Check the error path");
    assert_eq!(detail.entries[0].author, "human:igor");
    assert!(has_label(&harness, "Check the error path"));
}

#[test]
fn task_text_shows_as_text_and_runs_nothing() {
    let (fixture, mut harness, ids) = start_with_tasks(&["[Click me](file:///etc/passwd)"]);
    open_detail(&mut harness, "[Click me](file:///etc/passwd)");
    let (_host, tasks) = fixture.other_process();
    assert_eq!(
        tasks.detail(ids[0], &[]).expect("detail").meta.state.status,
        Status::Todo
    );
}

#[test]
fn a_service_error_shows_a_banner_that_closes_and_does_not_panic() {
    let (_fixture, mut harness, _ids) = start_with_tasks(&["Write the parser"]);
    harness
        .state_mut()
        .app_mut()
        .banners
        .push("The disk is full.".to_owned());
    harness.run();
    assert!(has_label(&harness, "The disk is full."));
    click(&mut harness, "Dismiss");
    assert!(!has_label(&harness, "The disk is full."));
}

#[test]
fn an_idle_app_asks_for_no_repaint() {
    let (_fixture, mut harness, _ids) = start_with_tasks(&["Write the parser"]);
    harness.run();
    assert!(harness.run_ok().is_some());
    assert_eq!(
        harness.run(),
        1,
        "one frame is enough, because nothing asks for a next frame"
    );
    assert!(
        !harness.ctx.has_requested_repaint(),
        "an idle app must not repaint"
    );
}

#[test]
fn the_board_shows_a_large_project_with_few_widgets() {
    let fixture = Arc::new(Fixture::with_project());
    let (_host, tasks) = fixture.other_process();
    let person = fixture.person();
    for number in 0..400 {
        tasks
            .create(
                &person,
                NewTask {
                    title: format!("Task {number}"),
                    ..NewTask::default()
                },
            )
            .expect("a task");
    }
    let harness = start(&fixture, SIZE, Theme::Light);
    assert!(has_label(&harness, "Todo column, 400 tasks"));
    let cards = harness.query_all_by_label_contains("Task ").count();
    assert!(cards < 60, "only visible cards exist, and {cards} exist");
}

fn open_new_drawing(harness: &mut Harness<'_, TestApp>) {
    open_detail(harness, "Write the parser");
    click(harness, "Artifacts");
    click(harness, "New drawing");
    type_into(harness, "Drawing name", "Parser sketch");
    click(harness, "Create drawing");
    assert_eq!(harness.state().app().view, View::Drawing);
}

fn draw_rectangle(harness: &mut Harness<'_, TestApp>) {
    click(harness, "Rectangle");
    let canvas = harness.get_by_label("Drawing canvas").rect();
    let from = canvas.center() - Vec2::new(90.0, 60.0);
    let to = canvas.center() + Vec2::new(60.0, 40.0);
    drag(harness, from, to);
}

fn shapes_of(harness: &Harness<'_, TestApp>) -> Vec<crate::drawing::model::Shape> {
    let board = harness.state().app().drawing.as_ref().expect("a drawing");
    board.editor.doc().shapes.clone()
}

#[test]
fn a_new_drawing_saves_links_and_reloads() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_new_drawing(&mut harness);
    draw_rectangle(&mut harness);
    let shapes = shapes_of(&harness);
    assert_eq!(shapes.len(), 1);
    assert!(shapes[0].w > 50.0 && shapes[0].h > 50.0, "{:?}", shapes[0]);
    assert!(has_label(&harness, "Shapes (1)"));
    click(&mut harness, "Save drawing");

    let (host, tasks) = fixture.other_process();
    let detail = tasks.detail(ids[0], &[]).expect("detail");
    let artifact = detail.meta.front.artifacts[0];
    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    let store = artifacts_of(&project).expect("artifacts");
    let record = store.record(artifact).expect("record");
    assert_eq!(record.media_type, DRAWING_MEDIA_TYPE);
    assert_eq!(record.version, 1);
    let bytes = store.read_bytes(artifact, None).expect("bytes");
    let saved = Document::from_bytes(&bytes).expect("a valid drawing");
    assert_eq!(saved.shapes, shapes);

    // A close and an open read the file again.
    click(&mut harness, "Close drawing");
    assert!(harness.state().app().drawing.is_none());
    click(&mut harness, "Open drawing");
    assert_eq!(shapes_of(&harness), shapes);
    assert!(has_label(&harness, "Shapes (1)"));
}

#[test]
fn a_new_version_of_a_drawing_needs_a_reason() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_new_drawing(&mut harness);
    draw_rectangle(&mut harness);
    click(&mut harness, "Save drawing");
    draw_rectangle(&mut harness);
    assert_eq!(shapes_of(&harness).len(), 2);
    click(&mut harness, "Save drawing");
    type_into(&mut harness, "Reason", "Add the second box");
    click(&mut harness, "Save version");
    assert!(harness.state().app().dialog.is_none());
    let (host, tasks) = fixture.other_process();
    let artifact = tasks
        .detail(ids[0], &[])
        .expect("detail")
        .meta
        .front
        .artifacts[0];
    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    let record = artifacts_of(&project)
        .expect("artifacts")
        .record(artifact)
        .expect("record");
    assert_eq!(record.version, 2);
    assert_eq!(record.current().reason, "Add the second box");
}

#[test]
fn a_drawing_reloads_after_a_remote_change_or_shows_a_banner_over_local_edits() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_new_drawing(&mut harness);
    draw_rectangle(&mut harness);
    click(&mut harness, "Save drawing");
    let (host, tasks) = fixture.other_process();
    let artifact = tasks
        .detail(ids[0], &[])
        .expect("detail")
        .meta
        .front
        .artifacts[0];
    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    let store = artifacts_of(&project).expect("artifacts");

    let write = |version: u64, shapes: &str| {
        let text = format!("{{\"format\": 1, \"shapes\": [{shapes}]}}");
        let source = lenka_artifacts::Source::Bytes {
            name: "remote.json",
            media_type: DRAWING_MEDIA_TYPE,
            bytes: text.as_bytes(),
        };
        let request = lenka_artifacts::NewVersion {
            expected_version: version,
            source,
            reason: "An agent changed the drawing",
            mode: lenka_artifacts::UpdateMode::Keep,
            claim: ClaimRef::Implicit,
        };
        store
            .update(&fixture.agent(), artifact, &request)
            .expect("a new version");
    };
    write(1, "");
    wait_until(&mut harness, "the empty version loads", |harness| {
        shapes_of(harness).is_empty()
    });
    assert!(
        !harness
            .state()
            .app()
            .drawing
            .as_ref()
            .is_some_and(|board| board.editor.is_dirty())
    );

    draw_rectangle(&mut harness);
    write(2, "");
    wait_until(&mut harness, "the banner shows", |harness| {
        harness
            .query_by_label_contains("Another writer saved version 3")
            .is_some()
    });
    assert_eq!(shapes_of(&harness).len(), 1, "the local edit stays");
    click(&mut harness, "Load the new version and lose my edits");
    assert!(shapes_of(&harness).is_empty());
}

#[test]
fn the_drawing_keys_work_and_undo_restores_the_shape() {
    let (_fixture, mut harness, _ids) = start_with_tasks(&["Write the parser"]);
    open_new_drawing(&mut harness);
    draw_rectangle(&mut harness);
    assert_eq!(shapes_of(&harness).len(), 1);
    harness.key_press(egui::Key::Delete);
    harness.run();
    assert!(
        shapes_of(&harness).is_empty(),
        "the new shape stays selected, so a delete removes it"
    );
    harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    harness.run();
    assert_eq!(shapes_of(&harness).len(), 1);
    harness.key_press(egui::Key::E);
    harness.run();
    assert_eq!(
        harness
            .state()
            .app()
            .drawing
            .as_ref()
            .map(|board| board.tool),
        Some(crate::drawing::canvas::Tool::Ellipse)
    );
}

#[test]
fn a_gantt_drag_moves_the_dates() {
    let fixture = Arc::new(Fixture::with_project());
    let (_host, tasks) = fixture.other_process();
    let start_day = lenka_plugin_api::Day::parse("2026-03-10").expect("a day");
    let due_day = lenka_plugin_api::Day::parse("2026-03-20").expect("a day");
    let id = tasks
        .create(
            &fixture.person(),
            NewTask {
                title: "Dated task".to_owned(),
                start: Some(start_day),
                due: Some(due_day),
                ..NewTask::default()
            },
        )
        .expect("a task")
        .id();
    let mut harness = start(&fixture, SIZE, Theme::Light);
    click(&mut harness, "Gantt");
    let bar = harness.get_by_label("Bar of Dated task").rect();
    drag(
        &mut harness,
        bar.center(),
        bar.center() + Vec2::new(bar.width() / 2.0, 0.0),
    );
    let meta = tasks
        .refresh()
        .and_then(|_| tasks.detail(id, &[]))
        .expect("detail")
        .meta;
    let moved = meta.front.start.expect("a start").number() - start_day.number();
    assert!(moved > 0, "the start moved by {moved} days");
    assert_eq!(
        meta.front.due.expect("a due date").number() - due_day.number(),
        moved
    );
}

#[test]
fn a_task_with_no_dates_gets_dates_from_the_gantt_view() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Undated task"]);
    click(&mut harness, "Gantt");
    click(&mut harness, "Show tasks with no dates");
    click(&mut harness, "Set dates of Undated task");
    type_into(&mut harness, "Start date", "2026-03-10");
    type_into(&mut harness, "Due date", "2026-03-12");
    click(&mut harness, "Set dates");
    assert!(harness.state().app().dialog.is_none());
    let (_host, tasks) = fixture.other_process();
    assert_eq!(
        tasks
            .detail(ids[0], &[])
            .expect("detail")
            .meta
            .front
            .start
            .map(|day| day.to_string()),
        Some("2026-03-10".to_owned())
    );
}

#[test]
fn settings_refuse_to_remove_a_used_type() {
    let (_fixture, mut harness, _ids) = start_with_tasks(&["Write the parser"]);
    click(&mut harness, "Settings");
    click(&mut harness, "Remove the type task");
    click(&mut harness, "Save settings");
    assert!(
        harness
            .query_by_label_contains("use the type task")
            .is_some()
    );

    harness.state_mut().app_mut().settings.reset();
    let session = harness.state().app().session.as_ref().expect("a session");
    let config = session.project.config();
    assert!(
        config.types.contains_key("task"),
        "the refused change did not save"
    );
}

fn synthetic_rows(count: usize) -> Vec<BoardRow> {
    let day = Day::parse("2026-03-10").expect("a day");
    (0..count)
        .map(|number| BoardRow {
            meta: TaskMeta {
                front: TaskFront {
                    id: TaskId::generate(),
                    kind: "task".to_owned(),
                    title: format!("Task {number}"),
                    parent: None,
                    blocked_by: Vec::new(),
                    relates: Vec::new(),
                    artifacts: Vec::new(),
                    priority: Priority::Normal,
                    labels: Vec::new(),
                    created_by: "t".to_owned(),
                    created_at: Timestamp::from_unix_ms(0),
                    start: Some(day),
                    due: Some(day),
                    coordinated: false,
                },
                state: State {
                    version: 1,
                    status: Status::Todo,
                    stage: "work".to_owned(),
                    unblocked: false,
                },
                has_plan: false,
                stamps: [None, None, None],
            },
            blocked: false,
            claimed_by: None,
            child_count: 0,
            closed_children: 0,
            invalid: false,
        })
        .collect()
}

/// Runs some frames, checks the count of widgets, and returns the time of one frame.
fn check_large_view<State>(harness: &mut Harness<'_, State>) -> Duration {
    harness.run();
    let cards = harness.query_all_by_label_contains("Task ").count();
    assert!(cards < 200, "only visible rows exist, and {cards} exist");
    let start = Instant::now();
    for _ in 0..10 {
        harness.step();
    }
    start.elapsed() / 10
}

#[test]
fn views_with_10000_tasks_draw_only_the_visible_rows() {
    let model = BoardModel::new(synthetic_rows(10_000));
    let visible: Vec<usize> = (0..10_000).collect();
    let mut limit_check = Vec::new();

    let mut board = BoardState::default();
    let mut harness = Harness::builder()
        .with_size(SIZE)
        .allow_missing_glyphs()
        .build_ui(|ui| {
            board.show(ui, &model, &visible, 1, None, false, &mut Vec::new());
        });
    limit_check.push(("board", check_large_view(&mut harness)));
    drop(harness);

    let mut tree = TreeState::default();
    let mut harness = Harness::builder()
        .with_size(SIZE)
        .allow_missing_glyphs()
        .build_ui(|ui| {
            tree.show(ui, &model, &visible, 1, None, &mut Vec::new());
        });
    limit_check.push(("tree", check_large_view(&mut harness)));
    drop(harness);

    let mut gantt = GanttState::default();
    let mut harness = Harness::builder()
        .with_size(SIZE)
        .allow_missing_glyphs()
        .build_ui(|ui| {
            gantt.show(ui, &model, &visible, 1, None, &mut Vec::new());
        });
    limit_check.push(("gantt", check_large_view(&mut harness)));
    drop(harness);

    for (view, frame) in limit_check {
        println!("One frame of the {view} with 10,000 tasks takes {frame:?} in this build.");
        // This limit is wide, because a test machine can be slow. It finds work for each task.
        assert!(
            frame < Duration::from_millis(500),
            "the {view} takes {frame:?} for one frame"
        );
    }
}

#[test]
fn a_file_path_becomes_an_artifact_of_the_task() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    let path = fixture.root.join("notes.txt");
    std::fs::write(&path, "Some notes").expect("a file");
    open_detail(&mut harness, "Write the parser");
    click(&mut harness, "Artifacts");
    type_into(&mut harness, "File path", &path.display().to_string());
    click(&mut harness, "Add file");
    let (_host, tasks) = fixture.other_process();
    let detail = tasks.detail(ids[0], &[]).expect("detail");
    assert_eq!(detail.meta.front.artifacts.len(), 1);
    assert!(
        harness.state().app().banners.is_empty(),
        "{:?}",
        harness.state().app().banners
    );
    assert!(has_label(&harness, "notes.txt"));
}

#[test]
fn settings_save_a_valid_change() {
    let (fixture, mut harness, _ids) = start_with_tasks(&["Write the parser"]);
    click(&mut harness, "Settings");
    click(&mut harness, "Agents can use the delete tools");
    click(&mut harness, "Save settings");
    assert!(has_label(&harness, "Saved"));
    let (host, _tasks) = fixture.other_process();
    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    assert!(!project.config().limits.agent_delete);
}

#[test]
fn a_stage_completes_with_evidence_and_returns_with_a_reason() {
    let fixture = Arc::new(Fixture::with_project());
    let (host, tasks) = fixture.other_process();
    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    let mut config = (*project.config()).clone();
    let stage = |name: &str| Stage {
        name: name.to_owned(),
        actor: Actor::Any,
        unblocks: false,
    };
    config.types.insert(
        "task".to_owned(),
        TypeDef {
            stages: vec![stage("build"), stage("review")],
        },
    );
    project.save_config(config).expect("settings");
    let id = tasks
        .create(&fixture.person(), new_task("Write the parser"))
        .expect("a task")
        .id();
    let mut harness = start(&fixture, SIZE, Theme::Light);
    open_detail(&mut harness, "Write the parser");

    click(&mut harness, "Complete stage");
    click(&mut harness, "Complete the stage");
    assert!(
        harness.query_by_label_contains("needs evidence").is_some(),
        "the evidence is required"
    );
    type_into(&mut harness, "Evidence", "cargo test passed");
    click(&mut harness, "Complete the stage");
    assert!(harness.state().app().dialog.is_none());
    let (_other_host, other) = fixture.other_process();
    assert_eq!(
        other.detail(id, &[]).expect("detail").meta.state.stage,
        "review"
    );

    click(&mut harness, "Return to stage");
    click(&mut harness, "Return the task");
    assert!(
        harness.query_by_label_contains("needs a reason").is_some(),
        "the reason is required"
    );
    type_into(&mut harness, "Reason", "The review found a bug");
    click(&mut harness, "Return the task");
    assert!(harness.state().app().dialog.is_none());
    let (_other_host, other) = fixture.other_process();
    assert_eq!(
        other.detail(id, &[]).expect("detail").meta.state.stage,
        "build"
    );
}

#[test]
fn the_text_filter_hides_other_cards() {
    let (_fixture, mut harness, _ids) = start_with_tasks(&["Write the parser", "Fix the login"]);
    type_into(&mut harness, "Filter", "login");
    assert!(has_label(&harness, "Fix the login"));
    assert!(!has_label(&harness, "Write the parser"));
}

#[test]
fn the_tree_collapses_a_parent() {
    let fixture = Arc::new(Fixture::with_project());
    let (_host, tasks) = fixture.other_process();
    let person = fixture.person();
    let parent = tasks
        .create(&person, new_task("Parent task"))
        .expect("a task")
        .id();
    tasks
        .create(
            &person,
            NewTask {
                parent: Some(parent),
                ..new_task("Child task")
            },
        )
        .expect("a task");
    let mut harness = start(&fixture, SIZE, Theme::Light);
    click(&mut harness, "Tree");
    assert!(has_label(&harness, "Child task"));
    click(&mut harness, "Collapse Parent task");
    assert!(!has_label(&harness, "Child task"));
    click(&mut harness, "Expand Parent task");
    assert!(has_label(&harness, "Child task"));
}

#[test]
fn a_roadmap_row_selects_its_task() {
    let (_fixture, mut harness, ids) = start_with_tasks(&["Top task"]);
    click(&mut harness, "Roadmap");
    click(&mut harness, "Top task");
    assert_eq!(
        harness
            .state()
            .app()
            .detail
            .as_ref()
            .map(|detail| detail.id),
        Some(ids[0])
    );
}

#[test]
fn each_view_needs_only_glyphs_of_a_system_font() {
    let fonts = match load_system() {
        Ok(fonts) => fonts,
        Err(error) => {
            println!("This test checks nothing. {error}");
            return;
        }
    };
    let fixture = Arc::new(Fixture::with_project());
    let (_host, tasks) = fixture.other_process();
    tasks.create(&fixture.person(), new_task("A title that is long enough to wrap, and to end with the overflow mark on a small card")).expect("a task");
    // This harness keeps the policy of `egui_kittest`: a missing glyph is a panic.
    let mut harness = start_with(&fixture, SIZE, Theme::Dark, |builder| {
        *builder = std::mem::take(builder).with_context_setup(move |ctx| ctx.set_fonts(fonts));
    });
    for view in ["Tree", "Roadmap", "Gantt", "Settings", "Board"] {
        click(&mut harness, view);
    }
    click(&mut harness, "New task");
    click(&mut harness, "Cancel");
}

#[test]
fn the_secret_policy_of_a_comment_needs_a_person() {
    let (fixture, _harness, ids) = start_with_tasks(&["Write the parser"]);
    let (_host, tasks) = fixture.other_process();
    let new = lenka_tasks::NewEntry {
        kind: EntryKind::Comment,
        text: concat!("ghp_", "abcdefghijklmnopqrstuvwxyz0123456789").to_owned(),
        replaces: None,
        secret_policy: SecretPolicy::PersonOverride,
    };
    assert!(
        tasks.add_entry(&fixture.agent(), ids[0], new).is_err(),
        "an agent cannot use the override"
    );
}
