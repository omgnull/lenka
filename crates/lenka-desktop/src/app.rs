// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The app state, the top bar, the filters, and the code that runs the requests of the views.

use std::path::PathBuf;

use egui::{
    Button, CentralPanel, ComboBox, Context, Id, Modal, Panel, RichText, Stroke, TextEdit, Ui, Vec2,
};
use lenka_artifacts::{DRAWING_MEDIA_TYPE, NewVersion, Source, UpdateMode};
use lenka_core::init::{Location, create_project};
use lenka_core::{Host, SecretPolicy};
use lenka_plugin_api::{ArtifactId, Caller, ErrorCode, LenkaError, Result, TaskId};
use lenka_tasks::{ClaimRef, Finish, NewEntry, NewTask, Priority, Status, TaskPatch};

use crate::actions::{Action, TaskOp, View};
use crate::board::BoardState;
use crate::data::{TaskFilter, apply_filter};
use crate::detail::{DetailState, Problem, Reload, Tab, has_section_heading, stage_place};
use crate::dialogs::{
    DatesForm, Dialog, EvidenceForm, FormError, LOCAL_DRIVER, NewProjectForm, NewTaskForm, Outcome,
    ReasonForm, ReturnForm,
};
use crate::drawing::canvas::{DrawingBoard, DrawingEvent, file_name_of};
use crate::gantt::GanttState;
use crate::roadmap::RoadmapState;
use crate::session::Session;
use crate::settings_view::SettingsState;
use crate::theme::{Palette, install, priority_name};
use crate::timeline::parse_day_field;
use crate::tree::TreeState;
use crate::widgets::{combo_info, primary_button, underline_tab};

const HEADING_MESSAGE: &str = "A line that starts with \"## \" begins a new section of the task file, \
     so lenka would move the text out of this field. Use \"### \" for a heading in the text.";

/// The native app.
#[derive(Debug)]
pub(crate) struct DesktopApp {
    pub host: Host,
    pub caller: Caller,
    pub ctx: Context,
    pub projects: Vec<String>,
    pub session: Option<Session>,
    pub view: View,
    pub filter: TaskFilter,
    pub show_cancelled: bool,
    visible: Vec<usize>,
    visible_key: (u64, TaskFilter),
    visible_token: u64,
    board: BoardState,
    tree: TreeState,
    roadmap: RoadmapState,
    gantt: GanttState,
    pub settings: SettingsState,
    pub drawing: Option<DrawingBoard>,
    pub detail: Option<DetailState>,
    pub dialog: Option<Dialog>,
    pub banners: Vec<String>,
    actions: Vec<Action>,
}

impl DesktopApp {
    /// Creates the app, and opens the first project.
    pub(crate) fn new(ctx: &Context, host: Host, caller: Caller, project: Option<String>) -> Self {
        install(ctx);
        let mut app = Self {
            host,
            caller,
            ctx: ctx.clone(),
            projects: Vec::new(),
            session: None,
            view: View::Board,
            filter: TaskFilter::default(),
            show_cancelled: false,
            visible: Vec::new(),
            visible_key: (0, TaskFilter::default()),
            visible_token: 1,
            board: BoardState::default(),
            tree: TreeState::default(),
            roadmap: RoadmapState::default(),
            gantt: GanttState::default(),
            settings: SettingsState::default(),
            drawing: None,
            detail: None,
            dialog: None,
            banners: Vec::new(),
            actions: Vec::new(),
        };
        app.load_project_list();
        let first = project
            .or_else(|| app.caller.default_project.clone())
            .or_else(|| app.projects.first().cloned());
        if let Some(name) = first {
            app.open_project(&name);
        }
        app
    }

    fn load_project_list(&mut self) {
        match self.host.available_projects(&self.caller) {
            Ok(list) => self.projects = list.into_keys().collect(),
            Err(error) => self.report(&error),
        }
    }

    pub(crate) fn report(&mut self, error: &LenkaError) {
        self.banners
            .push(format!("{} ({})", error.message(), error.code().as_str()));
    }

    fn open_project(&mut self, name: &str) {
        // Stop the thread of the old project before the new project opens.
        self.session = None;
        match Session::open(&self.host, name, &self.ctx) {
            Ok(session) => {
                self.session = Some(session);
                self.detail = None;
                self.drawing = None;
                self.settings.reset();
                self.board = BoardState::default();
                self.tree = TreeState::default();
                self.roadmap = RoadmapState::default();
                self.gantt = GanttState::default();
                self.filter = TaskFilter::default();
                self.visible_key = (0, TaskFilter::default());
                if self.view == View::Drawing {
                    self.view = View::Board;
                }
            }
            Err(error) => self.report(&error),
        }
    }

    /// Reads a new state after an external change. Each frame calls this, and it costs nothing
    /// with no change.
    fn poll(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        if let Some(message) = session.take_error() {
            self.banners.push(message);
        }
        if session.has_news() {
            self.reload_all(Reload::External);
        }
    }

    fn reload_all(&mut self, mode: Reload) {
        let Some(session) = &mut self.session else {
            return;
        };
        let mut errors = Vec::new();
        if let Err(error) = session.reload() {
            errors.push(error);
        }
        let session = &*session;
        let mut gone = false;
        if let Some(detail) = &mut self.detail
            && let Err(error) = detail.reload(session, mode)
        {
            if error.code() == ErrorCode::NotFound {
                gone = true;
            } else {
                errors.push(error);
            }
        }
        if gone {
            self.detail = None;
        }
        if mode == Reload::External
            && let Some(board) = &mut self.drawing
        {
            sync_drawing(board, session, &mut errors);
        }
        for error in errors {
            self.report(&error);
        }
    }

    fn update_visible(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        if self.visible_key.0 == session.generation && self.visible_key.1 == self.filter {
            return;
        }
        self.visible_key = (session.generation, self.filter.clone());
        self.visible = apply_filter(
            &session.model.rows,
            &self.filter,
            &session.project.config(),
            self.caller.actor,
        );
        self.visible_token += 1;
    }

    /// Draws one frame.
    pub(crate) fn show(&mut self, ui: &mut Ui) {
        self.poll();
        self.update_visible();
        self.dropped_files(ui.ctx());
        if self.dialog.is_none()
            && self.session.is_some()
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::N))
        {
            self.actions.push(Action::OpenNewTask { parent: None });
        }
        let palette = Palette::of(ui);
        Panel::top("top_bar")
            .frame(
                egui::Frame::new()
                    .fill(palette.surface)
                    .inner_margin(egui::Margin {
                        left: 14,
                        right: 14,
                        top: 8,
                        bottom: 0,
                    }),
            )
            .show_separator_line(true)
            .show(ui, |ui| self.top_bar(ui));
        self.banner_panel(ui);
        if self.session.is_none() {
            CentralPanel::default()
                .frame(egui::Frame::new().fill(palette.bg).inner_margin(40))
                .show(ui, |ui| self.start_screen(ui));
        } else {
            self.main_views(ui);
        }
        self.show_dialog(ui.ctx());
        self.file_drop_hint(ui.ctx());
        self.run_actions();
    }

    fn main_views(&mut self, ui: &mut Ui) {
        let palette = Palette::of(ui);
        if matches!(self.view, View::Board | View::Tree | View::Gantt) {
            Panel::top("filters")
                .frame(
                    egui::Frame::new()
                        .fill(palette.bg)
                        .inner_margin(egui::Margin {
                            left: 14,
                            right: 14,
                            top: 10,
                            bottom: 0,
                        }),
                )
                .show_separator_line(false)
                .show(ui, |ui| self.filter_bar(ui));
        }
        if matches!(
            self.view,
            View::Board | View::Tree | View::Roadmap | View::Gantt
        ) {
            let Self {
                detail,
                session,
                actions,
                ..
            } = self;
            if let (Some(detail), Some(session)) = (detail, session.as_ref()) {
                Panel::right("detail_panel")
                    .frame(egui::Frame::new().fill(palette.surface))
                    .default_size(470.0)
                    .min_size(380.0)
                    .max_size(760.0)
                    .show(ui, |ui| detail.show(ui, session, actions));
            }
        }
        CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(palette.bg)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| self.central(ui));
    }

    fn central(&mut self, ui: &mut Ui) {
        let selected = self.detail.as_ref().map(|detail| detail.id);
        let keys = self.dialog.is_none() && !ui.ctx().egui_wants_keyboard_input();
        let Self {
            session,
            board,
            tree,
            roadmap,
            gantt,
            settings,
            drawing,
            actions,
            visible,
            visible_token,
            show_cancelled,
            host,
            view,
            ..
        } = self;
        let Some(session) = session.as_ref() else {
            return;
        };
        let model = &session.model;
        let empty = model.rows.is_empty();
        match view {
            View::Board if empty => empty_board(ui, actions),
            View::Board => board.show(
                ui,
                model,
                visible,
                *visible_token,
                selected,
                *show_cancelled,
                actions,
            ),
            View::Tree => tree.show(ui, model, visible, *visible_token, selected, actions),
            View::Roadmap => roadmap.show(ui, model, session.generation, selected, actions),
            View::Gantt => gantt.show(ui, model, visible, *visible_token, selected, actions),
            View::Settings => settings.show(ui, host, actions),
            View::Drawing => {
                if let Some(board) = drawing {
                    let mut events = Vec::new();
                    board.show(ui, keys, &mut events);
                    self.drawing_events(&events);
                } else {
                    *view = View::Board;
                }
            }
        }
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        let palette = Palette::of(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new("lenka").size(18.0).color(palette.accent));
            ui.add_space(6.0);
            self.project_picker(ui);
            ui.add_space(14.0);
            if self.session.is_some() {
                ui.spacing_mut().item_spacing.x = 2.0;
                let mut tabs = vec![
                    (View::Board, "Board"),
                    (View::Tree, "Tree"),
                    (View::Roadmap, "Roadmap"),
                    (View::Gantt, "Gantt"),
                ];
                if self.drawing.is_some() {
                    tabs.push((View::Drawing, "Drawing"));
                }
                tabs.push((View::Settings, "Settings"));
                for (view, name) in tabs {
                    if underline_tab(ui, name, self.view == view).clicked() {
                        self.view = view;
                        if view == View::Settings
                            && let Some(session) = &self.session
                        {
                            self.settings.load(session);
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if primary_button(ui, "New task")
                        .on_hover_text("Ctrl+N")
                        .clicked()
                    {
                        self.actions.push(Action::OpenNewTask { parent: None });
                    }
                });
            }
        });
        ui.add_space(0.0);
    }

    fn project_picker(&mut self, ui: &mut Ui) {
        let current = self
            .session
            .as_ref()
            .map_or_else(|| "No project".to_owned(), |session| session.name.clone());
        let mut chosen = None;
        ComboBox::from_id_salt("project_picker")
            .selected_text(current.clone())
            .width(140.0)
            .show_ui(ui, |ui| {
                for name in &self.projects {
                    if ui
                        .selectable_label(*name == current, name.as_str())
                        .clicked()
                        && *name != current
                    {
                        chosen = Some(name.clone());
                    }
                }
                ui.separator();
                if ui.selectable_label(false, "New project").clicked() {
                    self.dialog = Some(Dialog::NewProject(NewProjectForm::new(&self.host)));
                }
            })
            .response
            .widget_info(|| combo_info("Project"));
        if let Some(name) = chosen {
            self.open_project(&name);
        }
    }

    fn banner_panel(&mut self, ui: &mut Ui) {
        if self.banners.is_empty() {
            return;
        }
        let palette = Palette::of(ui);
        let mut dismiss = None;
        Panel::top("banners")
            .frame(
                egui::Frame::new()
                    .fill(palette.danger_soft)
                    .inner_margin(egui::Margin::symmetric(14, 8)),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                for (index, message) in self.banners.iter().enumerate() {
                    ui.horizontal_wrapped(|ui| {
                        let response = ui.button("Dismiss");
                        if response.clicked() {
                            dismiss = Some(index);
                        }
                        ui.label(RichText::new(message).color(palette.danger));
                    });
                }
            });
        if let Some(index) = dismiss {
            self.banners.remove(index);
        }
    }

    fn start_screen(&mut self, ui: &mut Ui) {
        let palette = Palette::of(ui);
        ui.vertical_centered(|ui| {
            ui.add_space(80.0);
            ui.label(RichText::new("Welcome to lenka").size(26.0).color(palette.text));
            ui.add_space(8.0);
            ui.label(RichText::new("Create a project to store tasks, plans, and artifacts for your agents and for you.").color(palette.text_muted));
            ui.add_space(14.0);
            if primary_button(ui, "Create a project").clicked() {
                self.dialog = Some(Dialog::NewProject(NewProjectForm::new(&self.host)));
            }
            ui.add_space(8.0);
            ui.label(RichText::new("You can also run `lenka init` in a repository.").small().color(palette.text_muted));
        });
    }

    fn filter_bar(&mut self, ui: &mut Ui) {
        let palette = Palette::of(ui);
        let Some(session) = &self.session else {
            return;
        };
        let model = &session.model;
        let filter = &mut self.filter;
        ui.horizontal_wrapped(|ui| {
            let search = ui.label(RichText::new("Filter").color(palette.text_muted));
            ui.add(
                TextEdit::singleline(&mut filter.text)
                    .hint_text("Title or short ID")
                    .desired_width(170.0),
            )
            .labelled_by(search.id);
            option_combo(
                ui,
                "Type filter",
                "Any type",
                &mut filter.kind,
                model.kinds.iter(),
            );
            option_combo(
                ui,
                "Label filter",
                "Any label",
                &mut filter.label,
                model.labels.iter(),
            );
            ComboBox::from_id_salt("filter_priority")
                .selected_text(filter.priority.map_or("Any priority", priority_name))
                .width(90.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut filter.priority, None, "Any priority");
                    for priority in Priority::ALL.iter().rev() {
                        ui.selectable_value(
                            &mut filter.priority,
                            Some(*priority),
                            priority_name(*priority),
                        );
                    }
                })
                .response
                .widget_info(|| combo_info("Priority filter"));
            ui.add_space(4.0);
            ui.checkbox(&mut filter.blocked_only, "Blocked only");
            ui.checkbox(&mut filter.ready_only, "Ready only");
            if self.view == View::Board {
                ui.checkbox(&mut self.show_cancelled, "Show cancelled");
            }
            if !filter.is_empty() && ui.button("Clear").clicked() {
                *filter = TaskFilter::default();
            }
            ui.label(
                RichText::new(format!(
                    "{} of {} tasks",
                    self.visible.len(),
                    model.rows.len()
                ))
                .small()
                .color(palette.text_muted),
            );
        });
        ui.add_space(0.0);
    }

    fn dropped_files(&mut self, ctx: &Context) {
        let dropped = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect::<Vec<PathBuf>>()
        });
        if dropped.is_empty() {
            return;
        }
        let Some(id) = self.detail.as_ref().map(|detail| detail.id) else {
            self.banners.push(
                "Select a task first. A dropped file becomes an artifact of the selected task."
                    .to_owned(),
            );
            return;
        };
        for path in dropped {
            self.actions.push(Action::AddFile {
                id,
                path: path.display().to_string(),
            });
        }
    }

    fn file_drop_hint(&self, ctx: &Context) {
        let hovering = ctx.input(|input| !input.raw.hovered_files.is_empty());
        if !hovering {
            return;
        }
        let palette = Palette::for_dark(ctx.global_style().visuals.dark_mode);
        let rect = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            Id::unique("file_drop"),
        ));
        painter.rect_filled(rect, 0.0, palette.accent_soft.gamma_multiply(0.8));
        let text = if self.detail.is_some() {
            "Drop the file to add it to the selected task."
        } else {
            "Select a task first."
        };
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(20.0),
            palette.text,
        );
    }

    // ---- Dialogs ----

    fn show_dialog(&mut self, ctx: &Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let palette = Palette::for_dark(ctx.style_of(ctx.theme()).visuals.dark_mode);
        let frame = egui::Frame::new()
            .fill(palette.surface)
            .stroke(Stroke::new(1.0, palette.border_strong))
            .corner_radius(10)
            .inner_margin(18)
            .shadow(egui::Shadow {
                offset: [0, 10],
                blur: 34,
                spread: 0,
                color: egui::Color32::from_black_alpha(70),
            });
        let modal = Modal::new(Id::unique("lenka_dialog"))
            .frame(frame)
            .show(ctx, |ui| dialog.show(ui));
        let mut outcome = modal.inner;
        if outcome == Outcome::Open
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            outcome = Outcome::Cancel;
        }
        match outcome {
            Outcome::Open => {
                // A submit can set a next dialog, and an open dialog stays only if none exists.
                if self.dialog.is_none() {
                    self.dialog = Some(dialog);
                }
            }
            Outcome::Cancel => {}
            Outcome::Submit => self.submit_dialog(dialog, SecretPolicy::Enforce),
            Outcome::SubmitWithSecret => self.submit_dialog(dialog, SecretPolicy::PersonOverride),
        }
    }

    fn submit_dialog(&mut self, dialog: Dialog, policy: SecretPolicy) {
        match dialog {
            Dialog::NewTask(mut form) => {
                if let Err(error) = self.create_task(&form, policy) {
                    form.error = Some(error);
                    self.dialog = Some(Dialog::NewTask(form));
                }
            }
            Dialog::NewProject(mut form) => {
                if let Err(error) = self.create_project(&form) {
                    form.error = Some(error);
                    self.dialog = Some(Dialog::NewProject(form));
                }
            }
            Dialog::Evidence(mut form) => {
                if let Err(error) = self.submit(form.op()) {
                    form.error = Some(FormError::of(&error));
                    self.dialog = Some(Dialog::Evidence(form));
                }
            }
            Dialog::Return(mut form) => {
                if form.reason.trim().is_empty() {
                    form.error = Some(FormError::text(
                        "A return to an earlier stage needs a reason.",
                    ));
                    self.dialog = Some(Dialog::Return(form));
                    return;
                }
                let op = TaskOp::ReturnStage {
                    id: form.id,
                    stage: form.target.clone(),
                    reason: form.reason.clone(),
                };
                if let Err(error) = self.submit(op) {
                    form.error = Some(FormError::of(&error));
                    self.dialog = Some(Dialog::Return(form));
                }
            }
            Dialog::Reason(mut form) => {
                if form.reason.trim().is_empty() {
                    form.error = Some(FormError::text("A material change needs a reason."));
                    self.dialog = Some(Dialog::Reason(form));
                    return;
                }
                let mut patch = form.patch;
                patch.reason = Some(form.reason.trim().to_owned());
                self.send_draft(form.id, patch);
            }
            Dialog::Dates(mut form) => {
                if let Err(error) = self.submit_dates(&form) {
                    form.error = Some(error);
                    self.dialog = Some(Dialog::Dates(form));
                }
            }
            Dialog::Confirm { op, .. } => self.run_with_override(&op),
            Dialog::Delete { id, title, .. } => self.delete_task(id, title),
            Dialog::NewDrawing { task, name, error } => {
                if name.trim().is_empty() {
                    let error = error.or_else(|| Some(FormError::text("A drawing needs a name.")));
                    self.dialog = Some(Dialog::NewDrawing { task, name, error });
                    return;
                }
                self.drawing = Some(DrawingBoard::new_unsaved(name.trim(), task));
                self.view = View::Drawing;
            }
            Dialog::SaveDrawing { reason, .. } => {
                if let Err(error) = self.save_drawing_version(&reason) {
                    self.dialog = Some(Dialog::SaveDrawing {
                        reason,
                        error: Some(FormError::of(&error)),
                    });
                }
            }
            Dialog::CloseDrawing => {
                self.drawing = None;
                self.view = View::Board;
            }
        }
    }

    fn create_task(
        &mut self,
        form: &NewTaskForm,
        policy: SecretPolicy,
    ) -> std::result::Result<(), FormError> {
        let Some(session) = &self.session else {
            return Err(FormError::text("No project is open."));
        };
        if form.title.trim().is_empty() {
            return Err(FormError::text("A task needs a title."));
        }
        if [&form.description, &form.requirements, &form.acceptance]
            .iter()
            .any(|text| has_section_heading(text))
        {
            return Err(FormError::text(HEADING_MESSAGE));
        }
        let parent = if form.parent.trim().is_empty() {
            None
        } else {
            Some(
                session
                    .tasks
                    .resolve(form.parent.trim())
                    .map_err(|error| FormError::of(&error))?,
            )
        };
        let start = parse_day_field(&form.start).map_err(FormError::text)?;
        let due = parse_day_field(&form.due).map_err(FormError::text)?;
        let new = NewTask {
            kind: form.kind.clone(),
            title: form.title.trim().to_owned(),
            description: form.description.clone(),
            requirements: form.requirements.clone(),
            acceptance: form.acceptance.clone(),
            parent,
            priority: form.priority,
            labels: form
                .labels
                .split(',')
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .map(str::to_owned)
                .collect(),
            status: Some(form.status),
            start,
            due,
            secret_policy: policy,
            ..NewTask::default()
        };
        let meta = session
            .tasks
            .create(&self.caller, new)
            .map_err(|error| FormError::of(&error))?;
        self.reload_all(Reload::OwnField);
        self.select(meta.id());
        Ok(())
    }

    fn create_project(&mut self, form: &NewProjectForm) -> std::result::Result<(), FormError> {
        let name = form.name.trim();
        if name.is_empty() {
            return Err(FormError::text("A project needs a name."));
        }
        if form.driver != LOCAL_DRIVER {
            return Err(FormError::text(
                "The desktop app can create a project only with the local driver.",
            ));
        }
        let directory = form.directory.trim();
        let location = if directory.is_empty() {
            Location::Shared
        } else {
            let path = PathBuf::from(directory);
            if !path.is_absolute() {
                return Err(FormError::text(
                    "The data directory must be an absolute path. Use Browse to select a directory.",
                ));
            }
            Location::Directory(path)
        };
        let created = create_project(
            self.host.paths(),
            &self.caller.worktree,
            Some(name),
            &location,
        )
        .map_err(|error| FormError::of(&error))?;
        self.load_project_list();
        self.open_project(&created.name);
        Ok(())
    }

    fn submit_dates(&mut self, form: &DatesForm) -> std::result::Result<(), FormError> {
        let (start, due) = form.parsed().map_err(FormError::text)?;
        let patch = TaskPatch {
            expected_version: form.version,
            start: Some(start),
            due: Some(due),
            ..TaskPatch::default()
        };
        self.submit(TaskOp::Update {
            id: form.id,
            patch: Box::new(patch),
            from_draft: false,
        })
        .map_err(|error| FormError::of(&error))
    }

    fn delete_task(&mut self, id: TaskId, title: String) {
        let Some(session) = &self.session else {
            return;
        };
        match session.tasks.delete(&self.caller, id) {
            Ok(()) => {
                self.detail = None;
                self.reload_all(Reload::External);
            }
            Err(error) => {
                self.dialog = Some(Dialog::Delete {
                    id,
                    title,
                    error: Some(FormError::of(&error)),
                });
            }
        }
    }

    // ---- Task operations ----

    fn run_op(&self, op: &TaskOp, claim: &ClaimRef) -> Result<()> {
        let Some(session) = &self.session else {
            return Err(LenkaError::invalid_argument("No project is open."));
        };
        let tasks = &session.tasks;
        let caller = &self.caller;
        match op {
            TaskOp::SetStatus { id, status } => {
                tasks.set_status(caller, *id, *status, claim).map(|_| ())
            }
            TaskOp::Finish {
                id,
                evidence,
                unchecked_reason,
            } => {
                let finish = Finish::Done {
                    evidence: evidence.clone(),
                    unchecked_reason: unchecked_reason.clone(),
                };
                tasks.finish(caller, *id, claim, &finish).map(|_| ())
            }
            TaskOp::CompleteStage { id, evidence } => {
                if evidence.trim().is_empty() {
                    return Err(LenkaError::invalid_argument(
                        "A stage completion needs evidence, such as a command and its result.",
                    ));
                }
                if *claim == ClaimRef::Override {
                    return tasks
                        .advance(caller, *id, claim, evidence, None)
                        .map(|_| ());
                }
                let held = tasks.claim(caller, Some(*id), None, None)?;
                let handle = ClaimRef::Handle(held.handle);
                match tasks.advance(caller, *id, &handle, evidence, None) {
                    Ok(_) => Ok(()),
                    Err(error) => {
                        // Give the claim back, so that a failed stage completion holds nothing.
                        let _ignored = tasks.finish(caller, *id, &handle, &Finish::Released);
                        Err(error)
                    }
                }
            }
            TaskOp::ReturnStage { id, stage, reason } => tasks
                .advance(caller, *id, claim, reason, Some(stage))
                .map(|_| ()),
            TaskOp::Pause { id } => tasks.pause(caller, *id, claim).map(|_| ()),
            TaskOp::Update { id, patch, .. } => {
                let mut patch = patch.as_ref().clone();
                patch.claim = claim.clone();
                tasks.update(caller, *id, patch).map(|_| ())
            }
        }
    }

    /// Runs an operation. A stop that only a person can override opens a dialog that asks for
    /// confirmation, so that function returns `Ok` also in that case.
    pub(crate) fn submit(&mut self, op: TaskOp) -> Result<()> {
        if let Some(holder) = self.other_holder(&op) {
            let message = format!("{holder} holds a claim on this task.");
            self.dialog = Some(Dialog::Confirm { message, op });
            return Ok(());
        }
        match self.run_op(&op, &ClaimRef::Implicit) {
            Ok(()) => {
                self.after_op(&op);
                Ok(())
            }
            Err(error) if can_override(&error, &op) => {
                self.dialog = Some(Dialog::Confirm {
                    message: error.message().to_owned(),
                    op,
                });
                Ok(())
            }
            Err(error) => {
                // A failed call can also write data, as the loop limit does.
                self.reload_all(Reload::External);
                Err(error)
            }
        }
    }

    /// Returns the holder of a claim on the task if the holder is not this person.
    ///
    /// The service compares worktrees, so it accepts a person in the worktree of an agent. The
    /// person must still see the claim of that agent.
    fn other_holder(&self, op: &TaskOp) -> Option<String> {
        let row = self.session.as_ref()?.model.row(op_task(op))?;
        row.claimed_by
            .clone()
            .filter(|holder| *holder != self.caller.label)
    }

    fn run_with_override(&mut self, op: &TaskOp) {
        match self.run_op(op, &ClaimRef::Override) {
            Ok(()) => self.after_op(op),
            Err(error) => {
                if let TaskOp::Update {
                    from_draft: true,
                    patch,
                    ..
                } = op
                {
                    self.note_draft_problem(&error, patch);
                } else {
                    self.report(&error);
                }
            }
        }
    }

    fn after_op(&mut self, op: &TaskOp) {
        let from_draft = matches!(
            op,
            TaskOp::Update {
                from_draft: true,
                ..
            }
        );
        let id = op_task(op);
        let own = self.detail.as_ref().is_some_and(|detail| detail.id == id);
        let mode = match (own, from_draft) {
            (true, true) => Reload::Saved,
            (true, false) => Reload::OwnField,
            (false, _) => Reload::External,
        };
        self.reload_all(mode);
    }

    fn run_actions(&mut self) {
        for action in std::mem::take(&mut self.actions) {
            self.run_action(action);
        }
    }

    fn run_task_action(&mut self, op: TaskOp) {
        if let TaskOp::Update {
            id,
            patch,
            from_draft: true,
        } = op
        {
            self.send_draft(id, patch);
        } else if let Err(error) = self.submit(op) {
            self.report(&error);
        }
    }

    fn run_action(&mut self, action: Action) {
        match action {
            Action::Select(id) => self.select(id),
            Action::CloseDetail => self.detail = None,
            Action::MoveTo { id, status } => {
                if status == Status::Done {
                    self.open_finish(id);
                } else {
                    self.run_task_action(TaskOp::SetStatus { id, status });
                }
            }
            Action::Task(op) => self.run_task_action(op),
            Action::Open(dialog) => self.dialog = Some(dialog),
            Action::OpenFinish(id) => self.open_finish(id),
            Action::OpenCompleteStage(id) => self.open_complete_stage(id),
            Action::OpenReturnStage(id) => self.open_return_stage(id),
            Action::OpenNewTask { parent } => self.open_new_task(parent),
            Action::SaveDraft => self.save_draft(),
            Action::ReloadDetail => self.reload_detail(Reload::Rebase),
            Action::DiscardDraft => self.reload_detail(Reload::Discard),
            Action::SaveWithSecret => self.save_with_secret(),
            Action::AddEntry {
                id,
                kind,
                text,
                policy,
            } => self.add_entry(id, kind, text, policy),
            Action::AddFile { id, path } => self.add_file(id, &path),
            Action::OpenDrawing { artifact } => self.open_drawing(artifact),
            Action::ShowVersion { id, version } => self.show_version(id, version),
            Action::SaveSettings => self.save_settings(),
            Action::RunDoctor => self.run_doctor(),
        }
    }

    pub(crate) fn select(&mut self, id: TaskId) {
        let Some(session) = &self.session else {
            return;
        };
        if self.detail.as_ref().is_some_and(|detail| detail.id == id) {
            return;
        }
        if self.detail.as_ref().is_some_and(DetailState::is_dirty) {
            self.banners
                .push("Save or discard your text before you select another task.".to_owned());
            return;
        }
        let tab = self
            .detail
            .as_ref()
            .map_or(Tab::Details, |detail| detail.tab);
        match DetailState::open(session, id) {
            Ok(mut detail) => {
                detail.tab = tab;
                self.detail = Some(detail);
            }
            Err(error) => self.report(&error),
        }
    }

    fn reload_detail(&mut self, mode: Reload) {
        let Some(session) = &self.session else {
            return;
        };
        if let Some(detail) = &mut self.detail
            && let Err(error) = detail.reload(session, mode)
        {
            self.report(&error);
        }
    }

    fn evidence_form(&self, id: TaskId, finish: bool) -> Result<EvidenceForm> {
        let Some(session) = &self.session else {
            return Err(LenkaError::invalid_argument("No project is open."));
        };
        let data = session.tasks.detail(id, &[])?;
        Ok(EvidenceForm {
            id,
            title: data.meta.front.title.clone(),
            stage: data.meta.state.stage.clone(),
            finish,
            evidence: String::new(),
            unchecked: data.criteria.iter().filter(|item| !item.done).count(),
            unchecked_reason: String::new(),
            error: None,
        })
    }

    fn open_finish(&mut self, id: TaskId) {
        match self.evidence_form(id, true) {
            Ok(form) => self.dialog = Some(Dialog::Evidence(form)),
            Err(error) => self.report(&error),
        }
    }

    fn open_complete_stage(&mut self, id: TaskId) {
        match self.evidence_form(id, false) {
            Ok(form) => self.dialog = Some(Dialog::Evidence(form)),
            Err(error) => self.report(&error),
        }
    }

    fn open_return_stage(&mut self, id: TaskId) {
        let Some(session) = &self.session else {
            return;
        };
        let Some(row) = session.model.row(id) else {
            return;
        };
        let config = session.project.config();
        let Some(place) = stage_place(&config, &row.meta.front.kind, &row.meta.state.stage) else {
            self.banners.push(
                "The task is on a stage that its type does not have. Fix the settings first."
                    .to_owned(),
            );
            return;
        };
        let stages: Vec<String> = place.stages[..place.position].to_vec();
        let Some(target) = stages.last().cloned() else {
            return;
        };
        self.dialog = Some(Dialog::Return(ReturnForm {
            id,
            title: row.meta.front.title.clone(),
            stages,
            target,
            reason: String::new(),
            error: None,
        }));
    }

    fn open_new_task(&mut self, parent: Option<TaskId>) {
        let Some(session) = &self.session else {
            return;
        };
        let kinds: Vec<String> = session.project.config().types.keys().cloned().collect();
        let kind = if kinds.iter().any(|kind| kind == "task") {
            "task".to_owned()
        } else {
            kinds.first().cloned().unwrap_or_default()
        };
        self.dialog = Some(Dialog::NewTask(Box::new(NewTaskForm {
            title: String::new(),
            kind,
            kinds,
            priority: Priority::Normal,
            status: Status::Todo,
            parent: parent.map_or_else(String::new, |id| id.to_string()),
            description: String::new(),
            requirements: String::new(),
            acceptance: String::new(),
            labels: String::new(),
            start: String::new(),
            due: String::new(),
            error: None,
        })));
    }

    fn save_draft(&mut self) {
        let Some(detail) = &self.detail else {
            return;
        };
        let Some((patch, parts)) = detail.draft_patch() else {
            return;
        };
        let id = detail.id;
        let fields = [&patch.description, &patch.requirements, &patch.acceptance];
        if fields
            .iter()
            .any(|field| field.as_deref().is_some_and(has_section_heading))
        {
            if let Some(detail) = &mut self.detail {
                detail.problem = Some(Problem::Error(HEADING_MESSAGE.to_owned()));
            }
            return;
        }
        if parts.is_empty() {
            self.send_draft(id, patch);
        } else {
            self.dialog = Some(Dialog::Reason(ReasonForm {
                id,
                patch,
                parts,
                reason: String::new(),
                error: None,
            }));
        }
    }

    fn save_with_secret(&mut self) {
        let Some(detail) = &mut self.detail else {
            return;
        };
        let Some(Problem::Secret { patch, .. }) = detail.problem.take() else {
            return;
        };
        let id = detail.id;
        let mut patch = patch;
        patch.secret_policy = SecretPolicy::PersonOverride;
        patch.expected_version = detail.base_version;
        self.send_draft(id, patch);
    }

    /// Sends the drafts. A stale version or a secret keeps the text, and shows a message.
    fn send_draft(&mut self, id: TaskId, patch: Box<TaskPatch>) {
        let op = TaskOp::Update {
            id,
            patch: patch.clone(),
            from_draft: true,
        };
        if let Err(error) = self.submit(op) {
            self.note_draft_problem(&error, &patch);
        }
    }

    fn note_draft_problem(&mut self, error: &LenkaError, patch: &TaskPatch) {
        let problem = match error.code() {
            ErrorCode::StaleVersion => Problem::Stale(error.message().to_owned()),
            ErrorCode::SecretDetected => Problem::Secret {
                message: error.message().to_owned(),
                patch: Box::new(patch.clone()),
            },
            _ => Problem::Error(format!("{} ({})", error.message(), error.code().as_str())),
        };
        if let Some(detail) = &mut self.detail {
            detail.problem = Some(problem);
        }
    }

    fn add_entry(
        &mut self,
        id: TaskId,
        kind: lenka_tasks::EntryKind,
        text: String,
        policy: SecretPolicy,
    ) {
        let Some(session) = &self.session else {
            return;
        };
        let new = NewEntry {
            kind,
            text,
            replaces: None,
            secret_policy: policy,
        };
        match session.tasks.add_entry(&self.caller, id, new) {
            Ok(_) => {
                if let Some(detail) = &mut self.detail {
                    detail.entry_text.clear();
                    detail.entry_error = None;
                }
                self.reload_all(Reload::OwnField);
            }
            Err(error) => {
                if let Some(detail) = &mut self.detail {
                    detail.entry_error = Some(FormError::of(&error));
                }
            }
        }
    }

    fn add_file(&mut self, id: TaskId, path: &str) {
        let Some(session) = &self.session else {
            return;
        };
        let source = Source::File(std::path::Path::new(path));
        let result = session.artifacts.create(&self.caller, source, None, None);
        match result {
            Ok(record) => self.link_artifact(id, record.id),
            Err(error) => self.report(&error),
        }
    }

    fn link_artifact(&mut self, id: TaskId, artifact: ArtifactId) {
        let Some(session) = &self.session else {
            return;
        };
        let version = session
            .model
            .row(id)
            .map_or(0, |row| row.meta.state.version);
        let patch = TaskPatch {
            expected_version: version,
            artifacts_add: vec![artifact],
            ..TaskPatch::default()
        };
        if let Err(error) = self.submit(TaskOp::Update {
            id,
            patch: Box::new(patch),
            from_draft: false,
        }) {
            self.report(&error);
        }
    }

    fn open_drawing(&mut self, artifact: ArtifactId) {
        let Some(session) = &self.session else {
            return;
        };
        if self.drawing.as_ref().is_some_and(DrawingBoard::is_dirty) {
            self.banners
                .push("Save or close the open drawing first.".to_owned());
            self.view = View::Drawing;
            return;
        }
        let task = self.detail.as_ref().map(|detail| detail.id);
        let loaded = session.artifacts.record(artifact).and_then(|record| {
            session
                .artifacts
                .read_bytes(artifact, None)
                .map(|bytes| (record, bytes))
        });
        match loaded {
            Ok((record, bytes)) => match DrawingBoard::open(&record, &bytes, task) {
                Ok(board) => {
                    self.drawing = Some(board);
                    self.view = View::Drawing;
                }
                Err(error) => self
                    .banners
                    .push(format!("lenka cannot open this drawing. {error}")),
            },
            Err(error) => self.report(&error),
        }
    }

    /// Opens a drawing, as the button of the artifact list does.
    #[cfg(test)]
    pub(crate) fn open_drawing_for_test(&mut self, artifact: ArtifactId) {
        self.open_drawing(artifact);
    }

    fn drawing_events(&mut self, events: &[DrawingEvent]) {
        for event in events {
            match event {
                DrawingEvent::Save => self.save_drawing(),
                DrawingEvent::Close => {
                    if self.drawing.as_ref().is_some_and(DrawingBoard::is_dirty) {
                        self.dialog = Some(Dialog::CloseDrawing);
                    } else {
                        self.drawing = None;
                        self.view = View::Board;
                    }
                }
                DrawingEvent::ReloadRemote => self.reload_drawing(),
            }
        }
    }

    fn save_drawing(&mut self) {
        let Some(board) = &self.drawing else {
            return;
        };
        if board.artifact.is_some() {
            if board.editor.is_dirty() {
                self.dialog = Some(Dialog::SaveDrawing {
                    reason: String::new(),
                    error: None,
                });
            }
            return;
        }
        if let Err(error) = self.create_drawing() {
            self.report(&error);
        }
    }

    fn create_drawing(&mut self) -> Result<()> {
        let (Some(session), Some(board)) = (&self.session, &mut self.drawing) else {
            return Ok(());
        };
        let text = board.editor.doc().to_text();
        let file = file_name_of(&board.name);
        let source = Source::Bytes {
            name: &file,
            media_type: DRAWING_MEDIA_TYPE,
            bytes: text.as_bytes(),
        };
        let record = session.artifacts.create(
            &self.caller,
            source,
            Some(&board.name),
            Some(DRAWING_MEDIA_TYPE),
        )?;
        board.artifact = Some(record.id);
        board.version = record.version;
        board.editor.mark_saved();
        if let Some(task) = board.task {
            self.link_artifact(task, record.id);
        }
        Ok(())
    }

    fn save_drawing_version(&mut self, reason: &str) -> Result<()> {
        let (Some(session), Some(board)) = (&self.session, &mut self.drawing) else {
            return Ok(());
        };
        let Some(artifact) = board.artifact else {
            return Ok(());
        };
        let text = board.editor.doc().to_text();
        let file = file_name_of(&board.name);
        let source = Source::Bytes {
            name: &file,
            media_type: DRAWING_MEDIA_TYPE,
            bytes: text.as_bytes(),
        };
        let request = NewVersion {
            expected_version: board.version,
            source,
            reason,
            mode: UpdateMode::Keep,
            claim: ClaimRef::Implicit,
        };
        let record = session.artifacts.update(&self.caller, artifact, &request)?;
        board.version = record.version;
        board.remote_version = None;
        board.editor.mark_saved();
        self.reload_all(Reload::OwnField);
        Ok(())
    }

    fn reload_drawing(&mut self) {
        let (Some(session), Some(board)) = (&self.session, &mut self.drawing) else {
            return;
        };
        let Some(artifact) = board.artifact else {
            return;
        };
        let loaded = session.artifacts.record(artifact).and_then(|record| {
            session
                .artifacts
                .read_bytes(artifact, None)
                .map(|bytes| (record, bytes))
        });
        match loaded {
            Ok((record, bytes)) => {
                if let Err(error) = board.load_version(&record, &bytes) {
                    self.banners.push(format!(
                        "lenka cannot read the new version of this drawing. {error}"
                    ));
                }
            }
            Err(error) => self.report(&error),
        }
    }

    fn show_version(&mut self, id: TaskId, version: u64) {
        let Some(session) = &self.session else {
            return;
        };
        match session.tasks.read_version(id, version) {
            Ok((task, plan)) => {
                if let Some(detail) = &mut self.detail {
                    detail.old_version = Some(crate::detail::OldVersion {
                        version,
                        task,
                        plan,
                    });
                }
            }
            Err(error) => self.report(&error),
        }
    }

    fn save_settings(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        match self.settings.validated(&session.model.rows) {
            Ok(config) => match session.project.save_config(config.clone()) {
                Ok(()) => {
                    self.settings.mark_saved(config);
                    self.visible_key.0 = 0;
                }
                Err(error) => self.settings.error = Some(error.message().to_owned()),
            },
            Err(message) => self.settings.error = Some(message),
        }
    }

    fn run_doctor(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        match session.tasks.doctor() {
            Ok(report) => self.settings.doctor = Some(report),
            Err(error) => self.report(&error),
        }
    }
}

fn sync_drawing(board: &mut DrawingBoard, session: &Session, errors: &mut Vec<LenkaError>) {
    let Some(artifact) = board.artifact else {
        return;
    };
    let record = match session.artifacts.record(artifact) {
        Ok(record) => record,
        Err(error) => {
            errors.push(error);
            return;
        }
    };
    if record.version == board.version {
        return;
    }
    if board.editor.is_dirty() {
        board.remote_version = Some(record.version);
        return;
    }
    match session.artifacts.read_bytes(artifact, None) {
        Ok(bytes) => {
            if let Err(error) = board.load_version(&record, &bytes) {
                errors.push(LenkaError::invalid_file(format!(
                    "lenka cannot read the new version of this drawing. {error}"
                )));
            }
        }
        Err(error) => errors.push(error),
    }
}

fn op_task(op: &TaskOp) -> TaskId {
    match op {
        TaskOp::SetStatus { id, .. }
        | TaskOp::Finish { id, .. }
        | TaskOp::CompleteStage { id, .. }
        | TaskOp::ReturnStage { id, .. }
        | TaskOp::Pause { id }
        | TaskOp::Update { id, .. } => *id,
    }
}

/// Tells if a person can override a stop with an explicit confirmation.
fn can_override(error: &LenkaError, op: &TaskOp) -> bool {
    match error.code() {
        ErrorCode::ClaimLost => true,
        ErrorCode::ActorDenied | ErrorCode::Blocked => matches!(op, TaskOp::CompleteStage { .. }),
        _ => false,
    }
}

fn option_combo<'a>(
    ui: &mut Ui,
    name: &str,
    any: &str,
    value: &mut Option<String>,
    items: impl Iterator<Item = &'a String>,
) {
    ComboBox::from_id_salt(name)
        .selected_text(value.clone().unwrap_or_else(|| any.to_owned()))
        .width(90.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, any);
            for item in items {
                ui.selectable_value(value, Some(item.clone()), item.as_str());
            }
        })
        .response
        .widget_info(|| combo_info(name));
}

fn empty_board(ui: &mut Ui, actions: &mut Vec<Action>) {
    let palette = Palette::of(ui);
    ui.vertical_centered(|ui| {
        ui.add_space(100.0);
        ui.label(
            RichText::new("This project has no tasks yet")
                .size(19.0)
                .color(palette.text),
        );
        ui.add_space(6.0);
        ui.label(
            RichText::new("Create the first task here. Agents can also create tasks through MCP.")
                .color(palette.text_muted),
        );
        ui.add_space(10.0);
        if ui
            .add(Button::new("Create the first task").min_size(Vec2::new(0.0, 28.0)))
            .clicked()
        {
            actions.push(Action::OpenNewTask { parent: None });
        }
    });
}

impl eframe::App for DesktopApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(session) = &self.session {
            // The snapshot only makes the next start faster, so an error here has no effect.
            let _ignored = session.tasks.save_snapshot();
        }
    }
}
