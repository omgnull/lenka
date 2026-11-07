// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Renders PNG images of the views for a person to look at.
//!
//! Set `LENKA_SHOTS_DIR` to a directory to write the images. With no variable, or with no GPU
//! driver, the test prints a message and passes. The repository holds no reference images.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Theme, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use lenka_artifacts::{DRAWING_MEDIA_TYPE, Source, artifacts_of};
use lenka_plugin_api::{Day, TaskId};
use lenka_tasks::{ClaimRef, EntryKind, Link, NewEntry, NewTask, Priority, Status, TaskPatch};

use crate::actions::View;
use crate::dialogs::{Dialog, NewProjectForm};
use crate::fonts::load_system;
use crate::test_support::{Fixture, TestApp, start_with};

const SIZE: Vec2 = Vec2::new(1440.0, 900.0);

fn gpu_exists() -> bool {
    std::panic::catch_unwind(|| {
        egui_kittest::wgpu::create_render_state(
            egui_kittest::wgpu::default_wgpu_setup(),
            Default::default(),
        )
    })
    .is_ok()
}

fn day(offset: i32) -> Day {
    Day::from_number(Day::today().number() + offset)
}

struct Sample {
    drawing: lenka_plugin_api::ArtifactId,
    selected: TaskId,
}

#[expect(
    clippy::too_many_lines,
    reason = "this function is only a list of sample tasks"
)]
fn fill_sample(fixture: &Fixture) -> Sample {
    let (host, tasks) = fixture.other_process();
    let person = fixture.person();
    let agent = fixture.agent();
    let make = |kind: &str,
                title: &str,
                priority,
                labels: &[&str],
                parent,
                status,
                dates: Option<(i32, i32)>,
                text: &str,
                accept: &str| {
        tasks
            .create(
                &person,
                NewTask {
                    kind: kind.to_owned(),
                    title: title.to_owned(),
                    description: text.to_owned(),
                    acceptance: accept.to_owned(),
                    priority,
                    labels: labels.iter().map(|label| (*label).to_owned()).collect(),
                    parent,
                    status: Some(status),
                    start: dates.map(|(from, _)| day(from)),
                    due: dates.map(|(_, to)| day(to)),
                    ..NewTask::default()
                },
            )
            .expect("a sample task")
            .id()
    };
    use Priority::{High, Low, Normal, Urgent};
    use Status::{Backlog, Todo};
    let sync = make(
        "epic",
        "Offline sync for shared projects",
        High,
        &["sync"],
        None,
        Backlog,
        Some((-24, 40)),
        "Agents and people work on one project from two machines.",
        "",
    );
    let export = make(
        "epic",
        "Export of reports and task history",
        Normal,
        &["reports"],
        None,
        Backlog,
        Some((10, 75)),
        "People want a report for each release.",
        "",
    );
    let drawing_epic = make(
        "epic",
        "Drawing board for design notes",
        Normal,
        &["ui"],
        None,
        Backlog,
        Some((-50, -5)),
        "A single-user board inside the task.",
        "",
    );
    make(
        "epic",
        "Plugin registry",
        Low,
        &[],
        None,
        Backlog,
        None,
        "A later idea.",
        "",
    );
    let conflict = make(
        "task",
        "Detect conflicts in the change feed",
        Urgent,
        &["sync", "core"],
        Some(sync),
        Todo,
        Some((-14, 9)),
        "### Goal\n\nDetect two writers that change one task, and show **both** versions.\n\n- Read the feed offset\n- Compare the versions\n- Keep `state.toml` small\n",
        "- [x] Two writers on one machine produce one conflict record.\n- [ ] The board shows the conflict in a banner.\n- [ ] A test covers a crash in the middle of a write.",
    );
    let merge = make(
        "task",
        "Merge the task index after a pull",
        High,
        &["sync"],
        Some(sync),
        Todo,
        Some((-4, 12)),
        "Rebuild only the changed rows.",
        "",
    );
    let retry = make(
        "bug",
        "Retry a failed lock after a network drive drops",
        High,
        &["sync", "bug"],
        Some(sync),
        Todo,
        Some((-2, 4)),
        "The lock file stays after a drop.",
        "",
    );
    let docs = make(
        "task",
        "Write the sync guide for agents",
        Normal,
        &["docs"],
        Some(sync),
        Backlog,
        Some((14, 26)),
        "Explain the claim rules.",
        "",
    );
    make(
        "task",
        "Measure the sync cost with 10,000 tasks",
        Low,
        &["perf"],
        Some(sync),
        Backlog,
        None,
        "Use a release build.",
        "",
    );
    let chart = make(
        "task",
        "Add a burndown chart to the release report",
        Normal,
        &["reports"],
        Some(export),
        Backlog,
        Some((16, 38)),
        "One chart for each release.",
        "",
    );
    make(
        "task",
        "Export the history as CSV",
        Normal,
        &["reports"],
        Some(export),
        Todo,
        None,
        "Use one row for each version.",
        "",
    );
    let model = make(
        "task",
        "Shape model and JSON format",
        High,
        &["ui"],
        Some(drawing_epic),
        Todo,
        Some((-48, -30)),
        "Sorted, pretty JSON.",
        "",
    );
    let canvas = make(
        "task",
        "Canvas tools and undo",
        Normal,
        &["ui"],
        Some(drawing_epic),
        Todo,
        Some((-30, -12)),
        "Select, shapes, and text.",
        "",
    );
    let review = make(
        "bug",
        "Arrow head points the wrong way after a resize",
        Normal,
        &["ui", "bug"],
        Some(drawing_epic),
        Todo,
        Some((-12, -6)),
        "It shows after a flip.",
        "",
    );
    make(
        "task",
        "Rotate the signing key of the release build",
        Urgent,
        &["security"],
        None,
        Todo,
        Some((1, 2)),
        "The old key ends this month.",
        "",
    );
    make(
        "bug",
        "A long title of a task can wrap over two lines and must stop with an ellipsis, because the card has a fixed height",
        Normal,
        &["ui"],
        None,
        Backlog,
        None,
        "Check the card.",
        "",
    );
    make(
        "task",
        "Choose a default theme for the first start",
        Low,
        &["ui"],
        None,
        Backlog,
        None,
        "Follow the system theme.",
        "",
    );

    let block = |id: TaskId, blocker: TaskId| {
        let version = tasks.detail(id, &[]).expect("detail").meta.state.version;
        let patch = TaskPatch {
            expected_version: version,
            blocked_by_add: vec![Link::Local(blocker)],
            ..TaskPatch::default()
        };
        tasks.update(&person, id, patch).expect("a blocker");
    };
    block(merge, conflict);
    block(docs, merge);
    block(chart, docs);
    block(retry, merge);
    block(canvas, model);

    let set = |id: TaskId, status: Status| {
        tasks
            .set_status(&person, id, status, &ClaimRef::Implicit)
            .expect("a status");
    };
    let finish = |id: TaskId, proof: &str| {
        let finish = lenka_tasks::Finish::Done {
            evidence: proof.to_owned(),
            unchecked_reason: None,
        };
        tasks
            .finish(&person, id, &ClaimRef::Implicit, &finish)
            .expect("a finish");
    };
    finish(model, "cargo test -p lenka-desktop drawing passed");
    finish(canvas, "A person checked each tool.");
    set(review, Status::Paused);
    let held = tasks
        .claim(&agent, Some(conflict), None, None)
        .expect("a claim");
    let _ = held;
    tasks
        .add_entry(
            &agent,
            conflict,
            NewEntry {
                kind: EntryKind::Decision,
                text: "Keep the two versions in the feed. A merge tool is a later task.".to_owned(),
                replaces: None,
                secret_policy: lenka_core::SecretPolicy::Enforce,
            },
        )
        .expect("an entry");
    tasks
        .add_entry(
            &person,
            conflict,
            NewEntry {
                kind: EntryKind::Comment,
                text: "Please also check a crash in the middle of a write.".to_owned(),
                replaces: None,
                secret_policy: lenka_core::SecretPolicy::Enforce,
            },
        )
        .expect("an entry");

    let project = host
        .open_by_name(crate::test_support::PROJECT)
        .expect("project");
    let text = serde_json::json!({"format": 1, "shapes": [
        {"id": "a", "kind": "rect", "x": 40, "y": 40, "w": 220, "h": 110, "stroke": "#2563eb", "fill": "#dbeafe", "width": 2, "text": null, "points": null},
        {"id": "b", "kind": "text", "x": 62, "y": 78, "w": 160, "h": 24, "stroke": "#1f2937", "fill": null, "width": 1, "text": "Change feed", "points": null},
        {"id": "c", "kind": "rect", "x": 420, "y": 40, "w": 220, "h": 110, "stroke": "#16a34a", "fill": "#dcfce7", "width": 2, "text": null, "points": null},
        {"id": "d", "kind": "text", "x": 442, "y": 78, "w": 160, "h": 24, "stroke": "#1f2937", "fill": null, "width": 1, "text": "Task index", "points": null},
        {"id": "e", "kind": "arrow", "x": 260, "y": 95, "w": 160, "h": 0, "stroke": "#1f2937", "fill": null, "width": 2, "text": null, "points": [[260, 95], [420, 95]]},
        {"id": "f", "kind": "ellipse", "x": 230, "y": 230, "w": 220, "h": 110, "stroke": "#dc2626", "fill": "#fee2e2", "width": 2, "text": null, "points": null},
        {"id": "g", "kind": "text", "x": 290, "y": 272, "w": 120, "h": 24, "stroke": "#1f2937", "fill": null, "width": 1, "text": "Conflict", "points": null},
        {"id": "h", "kind": "line", "x": 340, "y": 95, "w": 0, "h": 135, "stroke": "#6b7280", "fill": null, "width": 1, "text": null, "points": [[340, 95], [340, 230]]},
        {"id": "i", "kind": "freehand", "x": 480, "y": 220, "w": 160, "h": 120, "stroke": "#7c3aed", "fill": null, "width": 2, "text": null, "points": [[480, 300], [510, 240], [540, 320], [575, 225], [610, 340], [640, 260]]}
    ]})
    .to_string();
    let store = artifacts_of(&project).expect("artifacts");
    let source = Source::Bytes {
        name: "sync.lenka-drawing.json",
        media_type: DRAWING_MEDIA_TYPE,
        bytes: text.as_bytes(),
    };
    let record = store
        .create(
            &person,
            source,
            Some("Sync design"),
            Some(DRAWING_MEDIA_TYPE),
        )
        .expect("a drawing");
    let version = tasks
        .detail(conflict, &[])
        .expect("detail")
        .meta
        .state
        .version;
    let patch = TaskPatch {
        expected_version: version,
        artifacts_add: vec![record.id],
        claim: ClaimRef::Override,
        ..TaskPatch::default()
    };
    tasks.update(&person, conflict, patch).expect("a link");
    Sample {
        drawing: record.id,
        selected: conflict,
    }
}

fn save(harness: &mut Harness<'_, TestApp>, dir: &Path, name: &str) {
    harness.run_ok();
    let image = harness.render().expect("an image");
    image
        .save(dir.join(format!("{name}.png")))
        .expect("a PNG file");
}

fn click(harness: &mut Harness<'_, TestApp>, label: &str) {
    harness.get_by_label(label).click();
    harness.run_ok();
}

fn shoot_theme(fixture: &Arc<Fixture>, sample: &Sample, dir: &Path, theme: Theme) {
    let suffix = if theme == Theme::Dark {
        "dark"
    } else {
        "light"
    };
    let fonts = load_system().expect("the caller checked the fonts");
    let mut harness = start_with(fixture, SIZE, theme, |builder| {
        *builder = std::mem::take(builder)
            .wgpu()
            .with_context_setup(move |ctx| ctx.set_fonts(fonts));
    });
    save(&mut harness, dir, &format!("board-{suffix}"));

    harness.state_mut().app_mut().select(sample.selected);
    harness.run_ok();
    save(&mut harness, dir, &format!("detail-{suffix}"));
    for tab in ["Entries", "Relations", "Artifacts", "History"] {
        click(&mut harness, tab);
        save(
            &mut harness,
            dir,
            &format!("detail-{}-{suffix}", tab.to_lowercase()),
        );
    }
    click(&mut harness, "Details");
    click(&mut harness, "Close");

    click(&mut harness, "Tree");
    save(&mut harness, dir, &format!("tree-{suffix}"));
    click(&mut harness, "Roadmap");
    save(&mut harness, dir, &format!("roadmap-{suffix}"));
    click(&mut harness, "Gantt");
    save(&mut harness, dir, &format!("gantt-{suffix}"));
    click(&mut harness, "Settings");
    harness.state_mut().app_mut().settings.doctor = None;
    save(&mut harness, dir, &format!("settings-{suffix}"));

    click(&mut harness, "Board");
    click(&mut harness, "New task");
    save(&mut harness, dir, &format!("dialog-{suffix}"));
    harness.state_mut().app_mut().dialog = None;
    harness.run_ok();

    harness.state_mut().app_mut().dialog = Some(Dialog::NewProject(NewProjectForm::new(
        &fixture.host(),
        &fixture.root,
    )));
    harness.run_ok();
    save(&mut harness, dir, &format!("new-project-{suffix}"));
    harness.state_mut().app_mut().dialog = None;
    harness.run_ok();

    harness.state_mut().app_mut().select(sample.selected);
    harness.run_ok();
    let actions_target = sample.drawing;
    harness
        .state_mut()
        .app_mut()
        .open_drawing_for_test(actions_target);
    harness.run_ok();
    assert_eq!(harness.state().app().view, View::Drawing);
    save(&mut harness, dir, &format!("drawing-{suffix}"));
}

#[test]
fn render_images_of_the_views() {
    let Some(dir) = std::env::var_os("LENKA_SHOTS_DIR").map(PathBuf::from) else {
        println!("LENKA_SHOTS_DIR is not set, so this test writes no images.");
        return;
    };
    if let Err(error) = load_system() {
        println!("This test writes no images. {error}");
        return;
    }
    if !gpu_exists() {
        println!("This machine has no GPU driver, so this test writes no images.");
        return;
    }
    std::fs::create_dir_all(&dir).expect("the image directory");
    let fixture = Arc::new(Fixture::with_project());
    let sample = fill_sample(&fixture);
    for theme in [Theme::Light, Theme::Dark] {
        shoot_theme(&fixture, &sample, &dir, theme);
    }
}
