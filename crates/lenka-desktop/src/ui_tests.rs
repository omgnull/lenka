// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Tests that drive the real UI with a temporary project and no window.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Theme, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use lenka_artifacts::{DRAWING_MEDIA_TYPE, artifacts_of};
use lenka_core::{ProjectsFile, SecretPolicy, Stage, TypeDef};
use lenka_plugin_api::{Actor, Day, TaskId, Timestamp};
use lenka_tasks::{
    BoardRow, ClaimRef, EntryKind, NewTask, Priority, State, Status, TaskFront, TaskMeta, TaskPatch,
};

use crate::actions::View;
use crate::board::BoardState;
use crate::data::BoardModel;
use crate::dialogs::{Dialog, NewProjectForm};
use crate::drawing::model::Document;
use crate::folder_pick::{FolderPick, PendingFolder};
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

/// Opens the dialog for a new project from the start screen.
fn start_new_project(fixture: &Arc<Fixture>) -> Harness<'static, TestApp> {
    let mut harness = start(fixture, SIZE, Theme::Light);
    click(&mut harness, "Create a project");
    harness
}

fn project_form<'a>(harness: &'a mut Harness<'static, TestApp>) -> &'a mut NewProjectForm {
    match harness.state_mut().app_mut().dialog.as_mut() {
        Some(Dialog::NewProject(form)) => form,
        other => panic!("the new project dialog must be open, but found {other:?}"),
    }
}

/// Sets the path as a folder pick does. A dialog takes a new place one frame after a new size, so
/// the test waits for that frame before the next click.
fn set_directory(harness: &mut Harness<'static, TestApp>, folder: &std::path::Path) {
    project_form(harness).directory = folder.display().to_string();
    harness.run_steps(3);
}

fn pending_pick(harness: &mut Harness<'static, TestApp>) -> PendingFolder {
    project_form(harness)
        .pending
        .clone()
        .expect("the folder dialog must be open")
}

#[test]
fn the_new_project_dialog_asks_for_the_name_then_the_driver_then_its_fields() {
    let fixture = Arc::new(Fixture::empty());
    let mut harness = start_new_project(&fixture);
    let name = harness.get_by_label("Project name").rect().top();
    let driver = harness
        .query_all_by_label("Storage driver")
        .map(|node| node.rect().top())
        .fold(f32::MAX, f32::min);
    let directory = harness.get_by_label("Data directory").rect().top();
    assert!(
        name < driver && driver < directory,
        "{name} {driver} {directory}"
    );
    assert!(harness.query_by_label("Browse").is_some());
    let form = project_form(&mut harness);
    assert_eq!(form.drivers, ["local"]);
    assert_eq!(form.driver, "local");
    let hint = fixture.paths.shared_project_dir("api");
    type_into(&mut harness, "Project name", "api");
    assert!(has_label(
        &harness,
        &format!("With no path, lenka uses {}", hint.display())
    ));
}

#[test]
fn browse_fills_the_path_and_the_project_uses_it() {
    let fixture = Arc::new(Fixture::empty());
    let mut harness = start_new_project(&fixture);
    type_into(&mut harness, "Project name", "api");
    click(&mut harness, "Browse");
    let pending = pending_pick(&mut harness);
    assert!(has_label(
        &harness,
        "Select a directory in the folder dialog."
    ));

    // A good selection ends the error of an earlier path.
    project_form(&mut harness).error = Some(crate::dialogs::FormError::text("An old error."));
    let selected = fixture.root.join("chosen");
    pending.finish(FolderPick::Selected(selected.clone()));
    harness.run();
    let form = project_form(&mut harness);
    assert!(form.pending.is_none());
    assert!(form.error.is_none());
    assert_eq!(form.directory, selected.display().to_string());

    click(&mut harness, "Create project");
    assert!(harness.state().app().dialog.is_none());
    assert!(selected.join("lenka.toml").is_file());
    let projects = ProjectsFile::load(&fixture.paths).expect("the list");
    assert_eq!(projects.projects["api"].directory, selected);
}

#[test]
fn a_cancelled_folder_dialog_keeps_the_typed_path() {
    let fixture = Arc::new(Fixture::empty());
    let mut harness = start_new_project(&fixture);
    type_into(&mut harness, "Data directory", "/typed/path");
    click(&mut harness, "Browse");
    if let Some(pending) = &mut project_form(&mut harness).pending {
        pending.make_old();
    }
    pending_pick(&mut harness).finish(FolderPick::Cancelled);
    harness.run();
    let form = project_form(&mut harness);
    assert!(form.pending.is_none());
    assert_eq!(form.directory, "/typed/path");
    assert!(form.error.is_none());
}

#[test]
fn a_folder_dialog_that_does_not_open_gives_a_message() {
    let fixture = Arc::new(Fixture::empty());
    let mut harness = start_new_project(&fixture);
    click(&mut harness, "Browse");
    pending_pick(&mut harness).finish(FolderPick::Cancelled);
    harness.run();
    let form = project_form(&mut harness);
    assert!(form.pending.is_none());
    assert!(form.error.is_some());
}

#[test]
fn a_new_project_needs_an_absolute_path_and_a_clean_directory() {
    let fixture = Arc::new(Fixture::empty());
    let mut harness = start_new_project(&fixture);
    type_into(&mut harness, "Project name", "api");
    type_into(&mut harness, "Data directory", "relative/data");
    click(&mut harness, "Create project");
    let message = project_form(&mut harness)
        .error
        .clone()
        .expect("an error")
        .message;
    assert!(message.contains("absolute path"), "{message}");
    assert!(!fixture.root.join("work/relative").exists());

    let folder = fixture.root.join("documents");
    std::fs::create_dir_all(&folder).expect("a folder");
    std::fs::write(folder.join("notes.txt"), "text").expect("a file");
    set_directory(&mut harness, &folder);
    click(&mut harness, "Create project");
    let message = project_form(&mut harness)
        .error
        .clone()
        .expect("an error")
        .message;
    assert!(message.contains("other files"), "{message}");
    assert!(!folder.join("tasks").exists());
    assert!(harness.state().app().session.is_none());
}

#[test]
fn another_driver_hides_the_local_fields_and_cannot_create_a_project() {
    let fixture = Arc::new(Fixture::empty());
    let mut harness = start_new_project(&fixture);
    type_into(&mut harness, "Project name", "api");
    project_form(&mut harness).driver = "remote";
    harness.run();
    assert!(harness.query_by_label("Browse").is_none());
    assert!(harness.query_by_label("Data directory").is_none());
    click(&mut harness, "Create project");
    let message = project_form(&mut harness)
        .error
        .clone()
        .expect("an error")
        .message;
    assert!(message.contains("only with the local driver"), "{message}");
    assert!(
        ProjectsFile::load(&fixture.paths)
            .expect("the list")
            .projects
            .is_empty()
    );
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

/// Writes a new version of the first drawing of a task, as another process does.
fn write_remote_version(fixture: &Fixture, task: TaskId, expected_version: u64) {
    let (host, tasks) = fixture.other_process();
    let artifact = tasks
        .detail(task, &[])
        .expect("detail")
        .meta
        .front
        .artifacts[0];
    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    let text = "{\"format\": 1, \"shapes\": []}";
    let request = lenka_artifacts::NewVersion {
        expected_version,
        source: lenka_artifacts::Source::Bytes {
            name: "remote.json",
            media_type: DRAWING_MEDIA_TYPE,
            bytes: text.as_bytes(),
        },
        reason: "An agent changed the drawing",
        mode: lenka_artifacts::UpdateMode::Keep,
        claim: ClaimRef::Implicit,
    };
    artifacts_of(&project)
        .expect("artifacts")
        .update(&fixture.agent(), artifact, &request)
        .expect("a new version");
}

fn newest_drawing_version(fixture: &Fixture, task: TaskId) -> u64 {
    let (host, tasks) = fixture.other_process();
    let artifact = tasks
        .detail(task, &[])
        .expect("detail")
        .meta
        .front
        .artifacts[0];
    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    artifacts_of(&project)
        .expect("artifacts")
        .record(artifact)
        .expect("record")
        .version
}

#[test]
fn keep_my_edits_lets_the_next_save_replace_the_other_version() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_new_drawing(&mut harness);
    draw_rectangle(&mut harness);
    click(&mut harness, "Save drawing");
    draw_rectangle(&mut harness);
    write_remote_version(&fixture, ids[0], 1);
    wait_until(&mut harness, "the banner shows", |harness| {
        harness
            .query_by_label_contains("Another writer saved version 2")
            .is_some()
    });
    click(&mut harness, "Keep my edits");
    click(&mut harness, "Save drawing");
    type_into(&mut harness, "Reason", "Keep the second box");
    click(&mut harness, "Save version");
    assert!(
        harness.state().app().dialog.is_none(),
        "the save works with the new version"
    );
    assert_eq!(newest_drawing_version(&fixture, ids[0]), 3);
    assert_eq!(shapes_of(&harness).len(), 2);
}

#[test]
fn text_that_a_person_types_stays_when_another_writer_saves() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_new_drawing(&mut harness);
    click(&mut harness, "Text");
    let canvas = harness.get_by_label("Drawing canvas").rect();
    harness.hover_at(canvas.center());
    harness.step();
    harness.drag_at(canvas.center());
    harness.step();
    harness.drop_at(canvas.center());
    harness.run();
    click(&mut harness, "Save drawing");
    assert_eq!(shapes_of(&harness).len(), 1);
    type_into(&mut harness, "Shape text", " with more");
    assert!(
        shapes_of(&harness)[0]
            .text
            .as_deref()
            .is_some_and(|text| text.contains("with more"))
    );
    write_remote_version(&fixture, ids[0], 1);
    wait_until(&mut harness, "the banner shows", |harness| {
        harness
            .query_by_label_contains("Another writer saved version 2")
            .is_some()
    });
    assert!(
        shapes_of(&harness)[0]
            .text
            .as_deref()
            .is_some_and(|text| text.contains("with more")),
        "the reload waits for a choice of the person"
    );
}

#[test]
fn a_delete_during_a_drag_changes_no_shape() {
    let (_fixture, mut harness, _ids) = start_with_tasks(&["Write the parser"]);
    open_new_drawing(&mut harness);
    draw_rectangle(&mut harness);
    let canvas = harness.get_by_label("Drawing canvas").rect();
    click(&mut harness, "Rectangle");
    drag(
        &mut harness,
        canvas.center() + Vec2::new(150.0, 150.0),
        canvas.center() + Vec2::new(250.0, 210.0),
    );
    let before = shapes_of(&harness);
    assert_eq!(before.len(), 2);

    // The top edge of the first rectangle.
    let from = canvas.center() - Vec2::new(0.0, 60.0);
    harness.hover_at(from);
    harness.step();
    harness.drag_at(from);
    harness.step();
    harness.hover_at(from + Vec2::new(30.0, 30.0));
    harness.step();
    harness.key_press(egui::Key::Delete);
    harness.step();
    harness.hover_at(from + Vec2::new(40.0, 40.0));
    harness.step();
    harness.drop_at(from + Vec2::new(40.0, 40.0));
    harness.run();
    let after = shapes_of(&harness);
    assert_eq!(after.len(), 2, "the delete waits for the end of the drag");
    assert_eq!(after[1], before[1], "the other shape stays the same");
    assert_ne!(after[0], before[0], "the drag moved the first shape");
}

#[test]
fn escape_during_a_move_puts_the_shape_back() {
    let (_fixture, mut harness, _ids) = start_with_tasks(&["Write the parser"]);
    open_new_drawing(&mut harness);
    draw_rectangle(&mut harness);
    let before = shapes_of(&harness);
    let canvas = harness.get_by_label("Drawing canvas").rect();
    let from = canvas.center() - Vec2::new(0.0, 60.0);
    harness.hover_at(from);
    harness.step();
    harness.drag_at(from);
    harness.step();
    harness.hover_at(from + Vec2::new(30.0, 30.0));
    harness.step();
    assert_ne!(shapes_of(&harness), before, "the drag moves the shape");
    harness.key_press(egui::Key::Escape);
    harness.step();
    harness.drop_at(from + Vec2::new(30.0, 30.0));
    harness.run();
    assert_eq!(shapes_of(&harness), before);
}

#[test]
fn a_drawing_name_with_a_path_is_refused_in_the_dialog() {
    let (_fixture, mut harness, _ids) = start_with_tasks(&["Write the parser"]);
    open_detail(&mut harness, "Write the parser");
    click(&mut harness, "Artifacts");
    click(&mut harness, "New drawing");
    type_into(&mut harness, "Drawing name", "plans/first");
    click(&mut harness, "Create drawing");
    assert!(harness.state().app().drawing.is_none());
    assert!(harness.state().app().dialog.is_some());
    assert!(harness.query_by_label_contains("one file name").is_some());
}

#[test]
fn a_drawing_outside_a_limit_does_not_save() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_new_drawing(&mut harness);
    draw_rectangle(&mut harness);
    let board = harness
        .state_mut()
        .app_mut()
        .drawing
        .as_mut()
        .expect("a board");
    let mut shape =
        crate::drawing::model::Shape::new(crate::drawing::model::Kind::Text, "#000000", None, 1.0);
    shape.text = Some("x".repeat(70 * 1024));
    board.editor.add(shape);
    harness.run();
    click(&mut harness, "Save drawing");
    let app = harness.state().app();
    assert!(
        app.banners.iter().any(|text| text.contains("larger than")),
        "{:?}",
        app.banners
    );
    assert!(
        app.drawing
            .as_ref()
            .is_some_and(|board| board.artifact.is_none())
    );
    let (_host, tasks) = fixture.other_process();
    let detail = tasks.detail(ids[0], &[]).expect("detail");
    assert!(detail.meta.front.artifacts.is_empty());
}

#[test]
fn large_ellipses_cost_few_vertices() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    let mut document = Document::default();
    for offset in 0..100_u32 {
        let mut shape = crate::drawing::model::Shape::new(
            crate::drawing::model::Kind::Ellipse,
            "#000000",
            Some("#ffffff"),
            1.0,
        );
        shape.x = f64::from(offset);
        shape.y = f64::from(offset);
        shape.w = 9_000_000.0;
        shape.h = 9_000_000.0;
        document.shapes.push(shape);
    }
    let (host, _tasks) = fixture.other_process();
    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    let text = document.to_text();
    let record = artifacts_of(&project)
        .expect("artifacts")
        .create(
            &fixture.person(),
            lenka_artifacts::Source::Bytes {
                name: "large.lenka-drawing.json",
                media_type: DRAWING_MEDIA_TYPE,
                bytes: text.as_bytes(),
            },
            None,
            Some(DRAWING_MEDIA_TYPE),
        )
        .expect("an artifact");
    let _ = ids;
    harness
        .state_mut()
        .app_mut()
        .open_drawing_for_test(record.id);
    harness.run();
    assert_eq!(shapes_of(&harness).len(), 100);
    let output = harness.output().shapes.clone();
    let meshes = harness.ctx.tessellate(output, 1.0);
    let vertices: usize = meshes
        .iter()
        .map(|mesh| match &mesh.primitive {
            egui::epaint::Primitive::Mesh(mesh) => mesh.vertices.len(),
            egui::epaint::Primitive::Callback(_) => 0,
        })
        .sum();
    assert!(vertices < 1_000_000, "{vertices} vertices");
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
                    loop_stopped: false,
                    loop_until_done: false,
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
    wait_until(&mut harness, "the file is linked", |harness| {
        let session = harness.state().app().session.as_ref().expect("a session");
        !session
            .tasks
            .detail(ids[0], &[])
            .expect("detail")
            .meta
            .front
            .artifacts
            .is_empty()
    });
    harness.run();
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
fn the_project_check_shows_a_stuck_write() {
    let (_fixture, mut harness, _ids) = start_with_tasks(&["A task"]);
    click(&mut harness, "Settings");
    harness.state_mut().app_mut().settings.doctor = Some(lenka_tasks::DoctorReport {
        unfinished_write: Some("The path tasks/a is a directory.".to_owned()),
        missing_links: vec!["a missing link".to_owned()],
        artifact_problems: vec!["an artifact problem".to_owned()],
        temp_files: vec!["a temporary file".to_owned()],
        oversize_inboxes: vec!["a large inbox".to_owned()],
        ..lenka_tasks::DoctorReport::default()
    });
    harness.run_steps(3);
    assert!(has_label(&harness, "The path tasks/a is a directory."));
    for item in [
        "a missing link",
        "an artifact problem",
        "a temporary file",
        "a large inbox",
    ] {
        assert!(has_label(&harness, item), "{item}");
    }
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

/// Starts the UI on a project with three tasks, with a second project with one task.
fn start_with_two_projects() -> (Arc<Fixture>, Harness<'static, TestApp>) {
    let fixture = Arc::new(Fixture::with_project());
    let (_host, tasks) = fixture.other_process();
    let person = fixture.person();
    for title in ["First", "Second", "Third"] {
        tasks.create(&person, new_task(title)).expect("a task");
    }
    let created = lenka_core::init::create_project(
        &fixture.paths,
        &fixture.root.join("small-work"),
        Some("small"),
        &lenka_core::init::Location::Shared,
    )
    .expect("a project");
    assert_eq!(created.name, "small");
    let host = fixture.host();
    let project = host.open_by_name("small").expect("the project");
    lenka_tasks::service_of(&project)
        .expect("tasks")
        .create(&person, new_task("Only"))
        .expect("a task");
    let harness = start(&fixture, SIZE, Theme::Light);
    (fixture, harness)
}

fn select_project(harness: &mut Harness<'_, TestApp>, name: &str) {
    click(harness, "Project");
    click(harness, name);
}

fn open_project_name(harness: &Harness<'_, TestApp>) -> String {
    let session = harness.state().app().session.as_ref().expect("a session");
    session.name.clone()
}

#[test]
fn a_switch_to_a_smaller_project_does_not_panic_in_any_view() {
    let (_fixture, mut harness) = start_with_two_projects();
    assert_eq!(open_project_name(&harness), crate::test_support::PROJECT);
    for view in [View::Board, View::Tree, View::Gantt] {
        harness.state_mut().app_mut().view = view;
        harness.run();
        select_project(&mut harness, "small");
        assert_eq!(open_project_name(&harness), "small");
        select_project(&mut harness, crate::test_support::PROJECT);
        assert_eq!(open_project_name(&harness), crate::test_support::PROJECT);
    }
}

#[test]
fn a_project_switch_asks_before_it_loses_text() {
    let (_fixture, mut harness) = start_with_two_projects();
    open_detail(&mut harness, "First");
    type_into(&mut harness, "Description", " Unsaved.");
    select_project(&mut harness, "small");
    assert_eq!(open_project_name(&harness), crate::test_support::PROJECT);
    click(&mut harness, "Cancel");
    let detail = harness.state().app().detail.as_ref().expect("the panel");
    assert!(detail.draft.description.contains("Unsaved."));
    select_project(&mut harness, "small");
    click(&mut harness, "Discard and continue");
    assert_eq!(open_project_name(&harness), "small");
}

#[test]
fn a_close_of_the_panel_asks_before_it_loses_text() {
    let (_fixture, mut harness, _ids) = start_with_tasks(&["Write the parser"]);
    open_detail(&mut harness, "Write the parser");
    type_into(&mut harness, "Description", " Unsaved.");
    click(&mut harness, "Close");
    assert!(harness.state().app().detail.is_some());
    click(&mut harness, "Discard and continue");
    assert!(harness.state().app().detail.is_none());
}

#[test]
fn a_close_of_the_window_asks_before_it_loses_text() {
    let (_fixture, mut harness, _ids) = start_with_tasks(&["Write the parser"]);
    let close_request = |harness: &mut Harness<'_, TestApp>| {
        harness
            .input_mut()
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
    };
    close_request(&mut harness);
    harness.step();
    assert!(
        harness.state().app().dialog.is_none(),
        "no text, no question"
    );

    open_detail(&mut harness, "Write the parser");
    type_into(&mut harness, "Description", " Unsaved.");
    close_request(&mut harness);
    harness.step();
    let cancelled = harness
        .output()
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .is_some_and(|output| {
            output
                .commands
                .contains(&egui::ViewportCommand::CancelClose)
        });
    assert!(cancelled);
    harness.run();
    assert!(matches!(
        harness.state().app().dialog,
        Some(Dialog::Unsaved(crate::dialogs::AfterUnsaved::CloseWindow))
    ));
}

#[test]
fn a_task_that_another_writer_deletes_keeps_the_text_on_the_screen() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser", "Other"]);
    open_detail(&mut harness, "Write the parser");
    type_into(&mut harness, "Description", " Unsaved.");
    let (_host, tasks) = fixture.other_process();
    tasks.delete(&fixture.person(), ids[0]).expect("a delete");
    wait_until(&mut harness, "the task leaves the board", |harness| {
        harness.query_by_label("Other").is_some()
            && harness.query_by_label("Write the parser").is_none()
    });
    harness.run();
    let detail = harness
        .state()
        .app()
        .detail
        .as_ref()
        .expect("the panel stays");
    assert!(detail.draft.description.contains("Unsaved."));
    assert!(
        harness
            .query_by_label_contains("deleted this task")
            .is_some()
    );

    // With no text, the panel closes.
    let (fixture2, mut harness2, ids2) = start_with_tasks(&["Clean", "Other"]);
    open_detail(&mut harness2, "Clean");
    let (_host, tasks) = fixture2.other_process();
    tasks.delete(&fixture2.person(), ids2[0]).expect("a delete");
    wait_until(&mut harness2, "the panel closes", |harness| {
        harness.state().app().detail.is_none()
    });
}

#[test]
fn a_project_that_cannot_open_keeps_the_open_project() {
    let (fixture, mut harness) = start_with_two_projects();
    let directory = fixture.paths.shared_project_dir("small");
    std::fs::remove_dir_all(&directory).expect("a remove");
    open_detail(&mut harness, "First");
    select_project(&mut harness, "small");
    assert_eq!(open_project_name(&harness), crate::test_support::PROJECT);
    assert!(harness.state().app().detail.is_some());
    assert!(!harness.state().app().banners.is_empty());
}

#[test]
fn save_with_this_value_saves_only_the_text_that_the_check_read() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_detail(&mut harness, "Write the parser");
    let secret = concat!("ghp_", "abcdefghijklmnopqrstuvwxyz0123456789");
    let saved = |fixture: &Fixture| {
        let (_host, tasks) = fixture.other_process();
        tasks.detail(ids[0], &[]).expect("detail").description
    };
    type_into(&mut harness, "Description", &format!(" {secret}"));
    click(&mut harness, "Save");
    type_into(&mut harness, "Reason", "Add a value");
    click(&mut harness, "Save with reason");
    assert!(has_label(&harness, "Save with this value"));

    // The person removes the value, and then selects the old choice.
    harness
        .state_mut()
        .app_mut()
        .detail
        .as_mut()
        .expect("the panel")
        .draft
        .description = "A clean text.".to_owned();
    harness.run();
    click(&mut harness, "Save with this value");
    if harness.state().app().dialog.is_some() {
        type_into(&mut harness, "Reason", "A clean text");
        click(&mut harness, "Save with reason");
    }
    assert!(
        saved(&fixture).contains("A clean text."),
        "{}",
        saved(&fixture)
    );
    assert!(!saved(&fixture).contains(secret));

    // A new value needs a new choice.
    type_into(&mut harness, "Description", &format!(" {secret}"));
    click(&mut harness, "Save");
    type_into(&mut harness, "Reason", "Add a value");
    click(&mut harness, "Save with reason");
    let detail = harness
        .state_mut()
        .app_mut()
        .detail
        .as_mut()
        .expect("the panel");
    detail.draft.description.push_str(" More.");
    harness.run();
    click(&mut harness, "Save with this value");
    if harness.state().app().dialog.is_some() {
        type_into(&mut harness, "Reason", "Add more");
        click(&mut harness, "Save with reason");
    }
    assert!(!saved(&fixture).contains(secret), "the check runs again");
    assert!(has_label(&harness, "Save with this value"));
}

#[test]
fn a_failed_link_removes_the_new_artifact() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    let path = fixture.root.join("notes.txt");
    std::fs::write(&path, "Some notes").expect("a file");
    open_detail(&mut harness, "Write the parser");
    click(&mut harness, "Artifacts");
    type_into(&mut harness, "File path", &path.display().to_string());
    // A closed task refuses the link.
    let (host, tasks) = fixture.other_process();
    tasks
        .set_status(
            &fixture.person(),
            ids[0],
            Status::Cancelled,
            &ClaimRef::Implicit,
        )
        .expect("a status");
    click(&mut harness, "Add file");
    wait_until(&mut harness, "the banner shows", |harness| {
        harness
            .state()
            .app()
            .banners
            .iter()
            .any(|text| text.contains("removed the new artifact"))
    });
    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    let list = artifacts_of(&project)
        .expect("artifacts")
        .list()
        .expect("a list");
    assert!(list.is_empty(), "{list:?}");
}

#[test]
fn a_cancelled_override_removes_the_new_artifact() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    let path = fixture.root.join("notes.txt");
    std::fs::write(&path, "Some notes").expect("a file");
    let (host, tasks) = fixture.other_process();
    tasks
        .claim(&fixture.agent(), Some(ids[0]), None, None)
        .expect("a claim");
    wait_until(&mut harness, "the claim shows", |harness| {
        let session = harness.state().app().session.as_ref().expect("a session");
        session
            .model
            .row(ids[0])
            .is_some_and(|row| row.claimed_by.is_some())
    });
    open_detail(&mut harness, "Write the parser");
    click(&mut harness, "Artifacts");
    type_into(&mut harness, "File path", &path.display().to_string());
    click(&mut harness, "Add file");
    wait_until(&mut harness, "the question shows", |harness| {
        matches!(harness.state().app().dialog, Some(Dialog::Confirm { .. }))
    });
    harness.run();
    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    let store = artifacts_of(&project).expect("artifacts");
    assert_eq!(store.list().expect("a list").len(), 1);
    click(&mut harness, "Cancel");
    assert!(store.list().expect("a list").is_empty());
}

#[test]
fn a_failed_link_of_a_new_drawing_keeps_the_drawing_unsaved() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_new_drawing(&mut harness);
    draw_rectangle(&mut harness);
    let (_host, tasks) = fixture.other_process();
    tasks
        .set_status(
            &fixture.person(),
            ids[0],
            Status::Cancelled,
            &ClaimRef::Implicit,
        )
        .expect("a status");
    click(&mut harness, "Save drawing");
    let board = harness.state().app().drawing.as_ref().expect("the board");
    assert!(board.artifact.is_none());
    assert!(board.is_dirty());
    assert_eq!(shapes_of(&harness).len(), 1);
}

#[test]
fn a_refused_stage_completion_changes_nothing() {
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
    tasks
        .set_status(&fixture.person(), id, Status::Backlog, &ClaimRef::Implicit)
        .expect("a status");
    let mut harness = start(&fixture, SIZE, Theme::Light);
    open_detail(&mut harness, "Write the parser");
    click(&mut harness, "Complete stage");
    let secret = concat!("ghp_", "abcdefghijklmnopqrstuvwxyz0123456789");
    type_into(&mut harness, "Evidence", secret);
    click(&mut harness, "Complete the stage");
    assert!(harness.state().app().dialog.is_some(), "the form stays");
    assert_eq!(status_of(&fixture, id), Status::Backlog);
    let (_other_host, other) = fixture.other_process();
    assert!(other.detail(id, &[]).expect("detail").claim.is_none());
}

#[test]
fn a_settings_save_stops_after_another_change_and_keeps_other_tables() {
    let (fixture, mut harness, _ids) = start_with_tasks(&["Write the parser"]);
    click(&mut harness, "Settings");
    click(&mut harness, "Agents can use the delete tools");
    let file = fixture
        .paths
        .shared_project_dir(crate::test_support::PROJECT)
        .join("lenka.toml");
    let old = std::fs::read_to_string(&file).expect("the file");
    let edited = old.replace("agent_delete = true", "agent_delete = false");
    assert_ne!(edited, old);
    std::fs::write(&file, format!("{edited}\n[custom]\nkey = \"value\"\n")).expect("a write");
    click(&mut harness, "Save settings");
    assert!(
        harness
            .query_by_label_contains("Another writer changed lenka.toml")
            .is_some(),
        "the save stops"
    );
    let text = std::fs::read_to_string(&file).expect("the file");
    assert!(text.contains("[custom]"), "{text}");

    click(&mut harness, "Load the file and lose my changes");
    assert!(harness.query_by_label_contains("Another writer").is_none());
    click(&mut harness, "Agents can use the delete tools");
    click(&mut harness, "Save settings");
    assert!(has_label(&harness, "Saved"), "a new load can save");
    let text = std::fs::read_to_string(&file).expect("the file");
    assert!(text.contains("[custom]"), "another table stays: {text}");
    assert!(text.contains("key = \"value\""), "{text}");
}

#[test]
fn the_dialog_connects_a_plain_directory_only_if_the_person_selects_this() {
    let fixture = Arc::new(Fixture::empty());
    let mut harness = start_new_project(&fixture);
    assert!(
        !project_form(&mut harness).attach,
        "a plain directory starts with no connection"
    );
    let start = project_form(&mut harness).start_directory.clone();
    type_into(&mut harness, "Project name", "private");
    click(&mut harness, "Create project");
    assert_eq!(open_project_name(&harness), "private");
    let paths = &fixture.paths;
    assert!(lenka_core::init::find_clone_file(paths, &start).is_none());
    assert!(lenka_core::init::find_clone_file(paths, &start.join("below")).is_none());
}

#[test]
fn a_failed_connection_gives_a_warning_and_the_project_opens() {
    let fixture = Arc::new(Fixture::empty());
    let mut harness = start_new_project(&fixture);
    let start = project_form(&mut harness).start_directory.clone();
    std::fs::create_dir_all(&start).expect("a directory");
    // A file in the place of the directory of the connections stops the connection.
    let connection = fixture.paths.plain_clone_file(&start);
    let blocker = connection.parent().expect("a parent");
    std::fs::create_dir_all(blocker.parent().expect("a parent")).expect("a directory");
    std::fs::write(blocker, "").expect("a file");
    project_form(&mut harness).attach = true;
    type_into(&mut harness, "Project name", "api");
    click(&mut harness, "Create project");
    assert!(harness.state().app().dialog.is_none());
    assert_eq!(open_project_name(&harness), "api");
    let banners = &harness.state().app().banners;
    assert!(
        banners.iter().any(|text| text.contains("cannot connect")),
        "{banners:?}"
    );
}

#[test]
fn the_dialog_keeps_a_path_as_it_is_and_clears_an_old_error() {
    let fixture = Arc::new(Fixture::empty());
    let mut harness = start_new_project(&fixture);
    type_into(&mut harness, "Project name", "api");
    type_into(&mut harness, "Data directory", "relative");
    click(&mut harness, "Create project");
    assert!(project_form(&mut harness).error.is_some());
    type_into(&mut harness, "Data directory", "x");
    assert!(
        project_form(&mut harness).error.is_none(),
        "an edit clears the error"
    );

    // Windows removes a space at the end of a folder name, so only other systems can test it.
    let folder = fixture
        .root
        .join(if cfg!(windows) { " chosen" } else { "chosen " });
    std::fs::create_dir_all(&folder).expect("a folder");
    set_directory(&mut harness, &folder);
    click(&mut harness, "Create project");
    assert_eq!(open_project_name(&harness), "api");
    assert!(
        folder.join("lenka.toml").is_file(),
        "the selected folder has the project"
    );
    assert!(!fixture.root.join("chosen").exists());
}

#[test]
fn a_folder_with_another_driver_keeps_the_open_project_and_the_list() {
    let (fixture, mut harness, _ids) = start_with_tasks(&["Write the parser"]);
    let folder = fixture.root.join("foreign");
    std::fs::create_dir_all(&folder).expect("a folder");
    let mut config = lenka_core::ProjectConfig::new("foreign");
    config.storage.driver = "s3".to_owned();
    std::fs::write(folder.join("lenka.toml"), config.to_text().expect("text")).expect("a write");
    click(&mut harness, "Project");
    click(&mut harness, "New project");
    type_into(&mut harness, "Project name", "foreign");
    set_directory(&mut harness, &folder);
    click(&mut harness, "Create project");
    let message = project_form(&mut harness)
        .error
        .clone()
        .expect("an error")
        .message;
    assert!(message.contains("another driver"), "{message}");
    assert_eq!(open_project_name(&harness), crate::test_support::PROJECT);
    let list = ProjectsFile::load(&fixture.paths).expect("the list");
    assert!(!list.projects.contains_key("foreign"));
}

#[test]
fn a_person_sees_a_loop_stop_and_can_let_agents_continue() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    let (_host, tasks) = fixture.other_process();
    let agent = fixture.agent();
    let id = ids[0];
    let mut stopped = false;
    for _ in 0..12 {
        let version = tasks.detail(id, &[]).expect("detail").meta.state.version;
        let patch = TaskPatch {
            expected_version: version,
            description: Some(format!("Text {version}")),
            reason: Some("A retry".to_owned()),
            ..TaskPatch::default()
        };
        if tasks.update(&agent, id, patch).is_err() {
            stopped = true;
            break;
        }
    }
    assert!(stopped, "the agent reaches the limit");
    let label = "Let agents repeat this task until it is done";
    wait_until(&mut harness, "the task shows as paused", |harness| {
        let session = harness.state().app().session.as_ref().expect("a session");
        session
            .model
            .row(id)
            .is_some_and(|row| row.meta.state.loop_stopped)
    });
    open_detail(&mut harness, "Write the parser");
    assert!(
        harness
            .query_by_label_contains("loop limit stopped agents")
            .is_some()
    );
    click(&mut harness, label);
    let (_other_host, other) = fixture.other_process();
    let state = other.detail(id, &[]).expect("detail").meta.state;
    assert!(state.loop_until_done);
    assert!(!state.loop_stopped);
    assert!(other.claim(&agent, Some(id), None, None).is_ok());
}

/// A plugin that fails after each event, as a failed Git push does.
#[derive(Debug)]
struct FailingHook;

impl lenka_core::EventHook for FailingHook {
    fn on_event(
        &self,
        _project: &lenka_core::Project,
        _event: &lenka_core::ProjectEvent,
    ) -> lenka_plugin_api::Result<()> {
        Err(lenka_plugin_api::LenkaError::invalid_argument(
            "The push failed.",
        ))
    }
}

#[derive(Debug)]
struct FailingPlugin;

impl lenka_core::Plugin for FailingPlugin {
    fn info(&self) -> lenka_core::PluginInfo {
        lenka_core::PluginInfo {
            name: "failing",
            description: "A plugin that fails after each event.",
            sensitive: false,
        }
    }

    fn register(&self, registry: &mut lenka_core::Registry) {
        registry.hook(Arc::new(FailingHook));
    }
}

#[test]
fn a_warning_of_a_plugin_shows_as_a_banner() {
    let fixture = Arc::new(Fixture::with_project());
    let (_host, tasks) = fixture.other_process();
    tasks
        .create(&fixture.person(), new_task("Write the parser"))
        .expect("a task");
    let host = fixture
        .host_builder()
        .plugin(FailingPlugin)
        .build()
        .expect("a host");
    let mut harness = crate::test_support::start_on_host(&fixture, host, SIZE, Theme::Light);
    move_card(&mut harness, "Write the parser", "Done");
    type_into(&mut harness, "Evidence", "cargo test passed");
    click(&mut harness, "Finish task");
    assert!(harness.state().app().dialog.is_none());
    let banners = &harness.state().app().banners;
    assert!(
        banners.iter().any(|text| text.contains("The push failed.")),
        "{banners:?}"
    );
}

/// The list of rows can be older than the task when a file ends its slow copy.
#[test]
fn a_link_uses_the_newest_version_of_the_task() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    let path = fixture.root.join("notes.txt");
    std::fs::write(&path, "Some notes").expect("a file");
    open_detail(&mut harness, "Write the parser");
    click(&mut harness, "Artifacts");
    type_into(&mut harness, "File path", &path.display().to_string());
    let session = harness.state().app().session.as_ref().expect("a session");
    let tasks = Arc::clone(&session.tasks);
    let patch = TaskPatch {
        expected_version: tasks
            .detail(ids[0], &[])
            .expect("detail")
            .meta
            .state
            .version,
        description: Some("Another text.".to_owned()),
        reason: Some("A change".to_owned()),
        ..TaskPatch::default()
    };
    tasks
        .update(&fixture.agent(), ids[0], patch)
        .expect("an update");
    click(&mut harness, "Add file");
    wait_until(&mut harness, "the file is linked", |harness| {
        let session = harness.state().app().session.as_ref().expect("a session");
        !session
            .tasks
            .detail(ids[0], &[])
            .expect("detail")
            .meta
            .front
            .artifacts
            .is_empty()
    });
}

/// Another process holds the write lock, so the add of the file waits. The frames must continue.
#[test]
fn a_slow_file_does_not_stop_the_ui() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    let path = fixture.root.join("slow.txt");
    std::fs::write(&path, "text").expect("a file");
    open_detail(&mut harness, "Write the parser");
    click(&mut harness, "Artifacts");
    type_into(&mut harness, "File path", &path.display().to_string());
    let host = fixture.host();
    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    project
        .write_locked(|_locked| {
            click(&mut harness, "Add file");
            assert!(has_label(&harness, "Adding a file"), "the frames continue");
            Ok(())
        })
        .expect("the lock");
    wait_until(&mut harness, "the file is done", |harness| {
        !has_label(harness, "Adding a file")
    });
    let (_host, tasks) = fixture.other_process();
    let detail = tasks.detail(ids[0], &[]).expect("detail");
    assert_eq!(detail.meta.front.artifacts.len(), 1);
}

#[test]
fn a_new_drawing_does_not_replace_unsaved_edits() {
    let (_fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    open_new_drawing(&mut harness);
    draw_rectangle(&mut harness);
    let count = shapes_of(&harness).len();
    assert!(count > 0);
    harness.state_mut().app_mut().dialog = Some(Dialog::NewDrawing {
        task: ids[0],
        name: "Another".to_owned(),
        error: None,
    });
    harness.run();
    click(&mut harness, "Create drawing");
    assert_eq!(shapes_of(&harness).len(), count, "the edits stay");
    let error = match &harness.state().app().dialog {
        Some(Dialog::NewDrawing { error, .. }) => error.clone(),
        other => panic!("the dialog must stay open, but found {other:?}"),
    };
    assert!(error.expect("an error").message.contains("open drawing"));
}

#[test]
fn a_remote_link_as_a_parent_gives_an_error_and_keeps_the_parent() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Parent task", "Child task"]);
    let (host, tasks) = fixture.other_process();
    let person = fixture.person();
    let version = tasks
        .detail(ids[1], &[])
        .expect("detail")
        .meta
        .state
        .version;
    tasks
        .update(
            &person,
            ids[1],
            TaskPatch {
                expected_version: version,
                parent: Some(Some(ids[0])),
                reason: Some("A move".to_owned()),
                ..TaskPatch::default()
            },
        )
        .expect("an update");
    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    harness.run_steps(5);
    open_detail(&mut harness, "Child task");
    click(&mut harness, "Relations");
    let remote = format!("{}/{}", lenka_plugin_api::ProjectId::generate(), ids[0]);
    {
        let detail = harness
            .state_mut()
            .app_mut()
            .detail
            .as_mut()
            .expect("a detail");
        detail.link_kind = crate::detail::LinkKind::Parent;
        detail.link_search = remote;
    }
    harness.run_steps(3);
    click(&mut harness, "Add link");
    let detail = harness.state().app().detail.as_ref().expect("a detail");
    let error = detail.field_error.clone().expect("an error");
    assert!(error.contains("parent"), "{error}");
    let after = lenka_tasks::service_of(&project).expect("service");
    assert_eq!(
        after.detail(ids[1], &[]).expect("detail").meta.front.parent,
        Some(ids[0])
    );
}

/// A person must answer for each file, so a second file cannot replace the first question.
#[test]
fn two_finished_files_for_a_held_task_ask_one_at_a_time() {
    let (fixture, mut harness, ids) = start_with_tasks(&["Write the parser"]);
    let (host, tasks) = fixture.other_process();
    tasks
        .claim(&fixture.agent(), Some(ids[0]), None, None)
        .expect("a claim");
    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    let store = lenka_artifacts::artifacts_of(&project).expect("the store");
    let person = fixture.person();
    let add = |name: &str| {
        store
            .create(
                &person,
                lenka_artifacts::Source::Bytes {
                    name,
                    media_type: "text/plain",
                    bytes: b"Some notes",
                },
                None,
                None,
            )
            .expect("an artifact")
    };
    let (first, second) = (add("a.txt"), add("b.txt"));
    harness.run_steps(5);
    {
        let app = harness.state_mut().app_mut();
        app.add_finished_file_for_test(ids[0], first);
        app.add_finished_file_for_test(ids[0], second);
    }
    harness.run_steps(3);
    assert!(matches!(
        harness.state().app().dialog,
        Some(Dialog::Confirm { .. })
    ));
    assert_eq!(harness.state().app().pending_file_count_for_test(), 1);
    click(&mut harness, "Cancel");
    harness.run_steps(3);
    assert!(matches!(
        harness.state().app().dialog,
        Some(Dialog::Confirm { .. })
    ));
    assert_eq!(harness.state().app().pending_file_count_for_test(), 0);
}

#[test]
fn entry_text_stays_when_a_person_selects_another_task() {
    let (_fixture, mut harness, ids) = start_with_tasks(&["First task", "Second task"]);
    open_detail(&mut harness, "First task");
    click(&mut harness, "Entries");
    type_into(&mut harness, "Entry text", "A note with no save");
    let before = harness.state().app().detail.as_ref().expect("a detail").id;
    harness.state_mut().app_mut().select(ids[1]);
    harness.run_steps(3);
    let detail = harness.state().app().detail.as_ref().expect("a detail");
    assert_eq!(detail.id, before);
    assert_eq!(detail.entry_text, "A note with no save");
}

#[test]
fn link_text_in_the_settings_stays_after_a_return_to_the_settings() {
    let (_fixture, mut harness, _ids) = start_with_tasks(&["A task"]);
    click(&mut harness, "Settings");
    harness.state_mut().app_mut().settings.link_to = "edited".to_owned();
    harness.run_steps(3);
    click(&mut harness, "Tree");
    click(&mut harness, "Settings");
    assert_eq!(harness.state().app().settings.link_to, "edited");
}

#[test]
fn settings_show_after_a_project_switch() {
    let (_fixture, mut harness) = start_with_two_projects();
    click(&mut harness, "Settings");
    assert!(harness.state().app().settings.draft.is_some());
    select_project(&mut harness, "small");
    assert_eq!(open_project_name(&harness), "small");
    assert!(harness.state().app().settings.draft.is_some());
}

#[test]
fn a_gantt_bar_dropped_outside_the_chart_still_moves_the_dates() {
    let fixture = Arc::new(Fixture::with_project());
    let (_host, tasks) = fixture.other_process();
    let start_day = lenka_plugin_api::Day::parse("2026-03-10").expect("a day");
    let id = tasks
        .create(
            &fixture.person(),
            NewTask {
                title: "Dated task".to_owned(),
                start: Some(start_day),
                due: lenka_plugin_api::Day::parse("2026-03-20").ok(),
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
        bar.center() + Vec2::new(SIZE.x * 3.0, 0.0),
    );
    let meta = tasks
        .refresh()
        .and_then(|_| tasks.detail(id, &[]))
        .expect("detail")
        .meta;
    assert!(meta.front.start.expect("a start").number() > start_day.number());
}

#[test]
fn a_date_that_a_person_types_stays_after_another_change() {
    let (fixture, mut harness, ids) = start_with_tasks(&["A task"]);
    open_detail(&mut harness, "A task");
    type_into(&mut harness, "Start", "2026-0");
    let (_host, tasks) = fixture.other_process();
    tasks
        .update(
            &fixture.person(),
            ids[0],
            TaskPatch {
                expected_version: 1,
                title: Some("A new title".to_owned()),
                ..TaskPatch::default()
            },
        )
        .expect("a change");
    wait_until(&mut harness, "the new version loads", |harness| {
        harness
            .state()
            .app()
            .detail
            .as_ref()
            .is_some_and(|detail| detail.data.meta.state.version == 2)
    });
    let detail = harness.state().app().detail.as_ref().expect("a panel");
    assert_eq!(detail.start_input_text(), "2026-0");
}
