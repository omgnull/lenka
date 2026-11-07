// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The modal dialogs. Each dialog holds its form, and the app runs its result.

use std::path::{Path, PathBuf};

use egui::{Button, ComboBox, RichText, ScrollArea, TextEdit, Ui};
use lenka_core::{Host, Paths};
use lenka_plugin_api::{ArtifactId, Day, ErrorCode, LenkaError, TaskId};
use lenka_tasks::{Priority, Status, TaskPatch};

use crate::actions::TaskOp;
use crate::folder_pick::{self, FolderPick, PendingFolder};
use crate::theme::{Palette, priority_name, status_name};
use crate::timeline::parse_day_field;
use crate::widgets::{area_field, caption, combo_info, danger_button, line_field, primary_button};

/// What the person did in a dialog during one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Open,
    Cancel,
    Submit,
    /// Submit, and let a person override the secret check.
    SubmitWithSecret,
}

/// An error that a form shows.
#[derive(Debug, Clone)]
pub(crate) struct FormError {
    pub message: String,
    pub secret: bool,
}

impl FormError {
    pub(crate) fn of(error: &LenkaError) -> Self {
        Self {
            message: error.message().to_owned(),
            secret: error.code() == ErrorCode::SecretDetected,
        }
    }

    pub(crate) fn text(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            secret: false,
        }
    }
}

/// The fields of a new task.
#[derive(Debug, Clone)]
pub(crate) struct NewTaskForm {
    pub title: String,
    pub kind: String,
    pub kinds: Vec<String>,
    pub priority: Priority,
    pub status: Status,
    pub parent: String,
    pub description: String,
    pub requirements: String,
    pub acceptance: String,
    pub labels: String,
    pub start: String,
    pub due: String,
    pub error: Option<FormError>,
}

/// The only driver that can create a project from the desktop app.
pub(crate) const LOCAL_DRIVER: &str = "local";

/// The fields of a new project.
#[derive(Debug, Clone)]
pub(crate) struct NewProjectForm {
    pub name: String,
    /// The drivers of the running build.
    pub drivers: Vec<&'static str>,
    pub driver: &'static str,
    /// The data directory of the local driver. An empty value selects the user data directory.
    pub directory: String,
    /// The native folder dialog, while it is open.
    pub pending: Option<PendingFolder>,
    /// The directory in which the app started.
    pub start_directory: PathBuf,
    /// Tells if agents in the start directory use the new project by default.
    pub attach: bool,
    paths: Paths,
    pub error: Option<FormError>,
}

impl NewProjectForm {
    /// Creates an empty form. The local driver is the first selection.
    pub(crate) fn new(host: &Host, start_directory: &Path) -> Self {
        let drivers = host.driver_names();
        let driver = drivers
            .iter()
            .copied()
            .find(|name| *name == LOCAL_DRIVER)
            .or_else(|| drivers.first().copied())
            .unwrap_or_default();
        Self {
            name: String::new(),
            drivers,
            driver,
            directory: String::new(),
            pending: None,
            start_directory: start_directory.to_path_buf(),
            // A Git clone is one project. A plain directory can hold many unrelated directories.
            attach: lenka_core::init::in_git_clone(start_directory),
            paths: host.paths().clone(),
            error: None,
        }
    }
}

/// The evidence for a finish or a stage completion.
#[derive(Debug, Clone)]
pub(crate) struct EvidenceForm {
    pub id: TaskId,
    pub title: String,
    pub stage: String,
    /// Tells if the form finishes the task, and not one stage.
    pub finish: bool,
    pub evidence: String,
    pub unchecked: usize,
    pub unchecked_reason: String,
    pub error: Option<FormError>,
}

impl EvidenceForm {
    /// Returns the operation of this form.
    pub(crate) fn op(&self) -> TaskOp {
        if self.finish {
            let reason = self.unchecked_reason.trim();
            TaskOp::Finish {
                id: self.id,
                evidence: self.evidence.clone(),
                unchecked_reason: (!reason.is_empty()).then(|| reason.to_owned()),
            }
        } else {
            TaskOp::CompleteStage {
                id: self.id,
                evidence: self.evidence.clone(),
            }
        }
    }
}

/// A return to an earlier stage.
#[derive(Debug, Clone)]
pub(crate) struct ReturnForm {
    pub id: TaskId,
    pub title: String,
    pub stages: Vec<String>,
    pub target: String,
    pub reason: String,
    pub error: Option<FormError>,
}

/// The reason for a material change.
#[derive(Debug, Clone)]
pub(crate) struct ReasonForm {
    pub id: TaskId,
    pub patch: Box<TaskPatch>,
    pub parts: Vec<&'static str>,
    pub reason: String,
    pub error: Option<FormError>,
}

/// The dates of a task.
#[derive(Debug, Clone)]
pub(crate) struct DatesForm {
    pub id: TaskId,
    pub title: String,
    pub version: u64,
    pub start: String,
    pub due: String,
    pub error: Option<FormError>,
}

impl DatesForm {
    /// Returns the two dates, or a message about a bad date.
    pub(crate) fn parsed(&self) -> Result<(Option<Day>, Option<Day>), String> {
        Ok((parse_day_field(&self.start)?, parse_day_field(&self.due)?))
    }
}

/// The step that waits for a person to throw away unsaved work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AfterUnsaved {
    CloseDetail,
    OpenProject(String),
    CloseWindow,
}

/// A modal dialog.
#[derive(Debug, Clone)]
pub(crate) enum Dialog {
    NewTask(Box<NewTaskForm>),
    NewProject(NewProjectForm),
    Evidence(EvidenceForm),
    Return(ReturnForm),
    Reason(ReasonForm),
    Dates(DatesForm),
    /// Asks a person to override a claim, an actor rule, or a similar stop.
    Confirm {
        message: String,
        op: TaskOp,
        /// A new artifact that the change links. A stop of the change removes the artifact.
        created: Option<ArtifactId>,
    },
    /// Asks a person to throw away unsaved work before a next step.
    Unsaved(AfterUnsaved),
    Delete {
        id: TaskId,
        title: String,
        error: Option<FormError>,
    },
    NewDrawing {
        task: TaskId,
        name: String,
        error: Option<FormError>,
    },
    SaveDrawing {
        reason: String,
        error: Option<FormError>,
    },
    CloseDrawing,
}

fn error_line(ui: &mut Ui, error: Option<&FormError>) {
    if let Some(error) = error {
        let palette = Palette::of(ui);
        egui::Frame::new()
            .fill(palette.danger_soft)
            .corner_radius(6)
            .inner_margin(8)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(RichText::new(&error.message).color(palette.danger));
            });
    }
}

/// Draws the row of buttons at the end of a form.
fn buttons(ui: &mut Ui, submit: &str, error: Option<&FormError>) -> Outcome {
    let mut outcome = Outcome::Open;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if primary_button(ui, submit).clicked() {
            outcome = Outcome::Submit;
        }
        if ui.button("Cancel").clicked() {
            outcome = Outcome::Cancel;
        }
        if error.is_some_and(|error| error.secret)
            && danger_button(ui, "Save with this value").clicked()
        {
            outcome = Outcome::SubmitWithSecret;
        }
    });
    outcome
}

fn heading(ui: &mut Ui, text: &str) {
    let palette = Palette::of(ui);
    ui.label(RichText::new(text).size(17.0).color(palette.text));
    ui.add_space(4.0);
}

fn task_line(ui: &mut Ui, title: &str) {
    let palette = Palette::of(ui);
    ui.label(RichText::new(title).color(palette.text_muted));
    ui.add_space(2.0);
}

impl Dialog {
    /// Draws the dialog.
    pub(crate) fn show(&mut self, ui: &mut Ui) -> Outcome {
        ui.set_width(if matches!(self, Self::NewTask(_)) {
            560.0
        } else {
            440.0
        });
        match self {
            Self::NewTask(form) => show_new_task(ui, form),
            Self::NewProject(form) => show_new_project(ui, form),
            Self::Evidence(form) => show_evidence(ui, form),
            Self::Return(form) => show_return(ui, form),
            Self::Reason(form) => show_reason(ui, form),
            Self::Dates(form) => show_dates(ui, form),
            Self::Confirm { message, .. } => {
                heading(ui, "Override this stop?");
                ui.label(message.as_str());
                caption(
                    ui,
                    "Only a person can do this. lenka records your name on the change.",
                );
                buttons(ui, "Override and continue", None)
            }
            Self::Unsaved(_) => {
                heading(ui, "Discard the unsaved work?");
                ui.label("You have text, a drawing, or settings that lenka did not save, or a file that lenka still adds.");
                buttons(ui, "Discard and continue", None)
            }
            Self::Delete { title, error, .. } => {
                heading(ui, "Delete this task?");
                task_line(ui, title);
                ui.label("lenka deletes the task and its versions. Committed data stays in the Git history.");
                error_line(ui, error.as_ref());
                let mut outcome = Outcome::Open;
                ui.horizontal(|ui| {
                    if danger_button(ui, "Delete").clicked() {
                        outcome = Outcome::Submit;
                    }
                    if ui.button("Cancel").clicked() {
                        outcome = Outcome::Cancel;
                    }
                });
                outcome
            }
            Self::NewDrawing { name, error, .. } => {
                heading(ui, "New drawing");
                line_field(ui, "Drawing name", name, "Architecture sketch");
                error_line(ui, error.as_ref());
                buttons(ui, "Create drawing", error.as_ref())
            }
            Self::SaveDrawing { reason, error } => {
                heading(ui, "Save a new version");
                ui.label("A new version is a material change, so it needs a reason.");
                line_field(ui, "Reason", reason, "What changed, and why");
                error_line(ui, error.as_ref());
                buttons(ui, "Save version", None)
            }
            Self::CloseDrawing => {
                heading(ui, "Close this drawing?");
                ui.label("The drawing has edits that are not saved. A close loses them.");
                let mut outcome = Outcome::Open;
                ui.horizontal(|ui| {
                    if danger_button(ui, "Close and lose edits").clicked() {
                        outcome = Outcome::Submit;
                    }
                    if ui.button("Keep editing").clicked() {
                        outcome = Outcome::Cancel;
                    }
                });
                outcome
            }
        }
    }
}

fn show_new_task(ui: &mut Ui, form: &mut NewTaskForm) -> Outcome {
    heading(ui, "New task");
    let response = line_field(ui, "Title", &mut form.title, "What must happen");
    if ui.memory(|memory| memory.focused().is_none()) {
        response.request_focus();
    }
    ui.add_space(2.0);
    ui.columns(4, |columns| {
        caption(&mut columns[0], "Type");
        ComboBox::from_id_salt("new_task_kind")
            .selected_text(form.kind.clone())
            .width(110.0)
            .show_ui(&mut columns[0], |ui| {
                for kind in &form.kinds {
                    ui.selectable_value(&mut form.kind, kind.clone(), kind.as_str());
                }
            })
            .response
            .widget_info(|| combo_info("Type"));
        caption(&mut columns[1], "Priority");
        ComboBox::from_id_salt("new_task_priority")
            .selected_text(priority_name(form.priority))
            .width(110.0)
            .show_ui(&mut columns[1], |ui| {
                for priority in Priority::ALL {
                    ui.selectable_value(&mut form.priority, priority, priority_name(priority));
                }
            })
            .response
            .widget_info(|| combo_info("Priority"));
        caption(&mut columns[2], "Status");
        ComboBox::from_id_salt("new_task_status")
            .selected_text(status_name(form.status))
            .width(110.0)
            .show_ui(&mut columns[2], |ui| {
                for status in [Status::Backlog, Status::Todo] {
                    ui.selectable_value(&mut form.status, status, status_name(status));
                }
            })
            .response
            .widget_info(|| combo_info("Status"));
        line_field(&mut columns[3], "Parent ID", &mut form.parent, "Optional");
    });
    ScrollArea::vertical()
        .max_height(320.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            area_field(ui, "Description", &mut form.description, 3);
            area_field(ui, "Requirements", &mut form.requirements, 2);
            area_field(ui, "Acceptance criteria", &mut form.acceptance, 2);
            caption(
                ui,
                "Write each criterion as a line like: - [ ] The parser rejects bad input.",
            );
        });
    ui.columns(3, |columns| {
        line_field(&mut columns[0], "Labels", &mut form.labels, "api, parser");
        line_field(&mut columns[1], "Start date", &mut form.start, "YYYY-MM-DD");
        line_field(&mut columns[2], "Due date", &mut form.due, "YYYY-MM-DD");
    });
    error_line(ui, form.error.as_ref());
    buttons(ui, "Create task", form.error.as_ref())
}

fn show_new_project(ui: &mut Ui, form: &mut NewProjectForm) -> Outcome {
    heading(ui, "New project");
    let name = line_field(ui, "Project name", &mut form.name, "api");
    if ui.memory(|memory| memory.focused().is_none()) {
        name.request_focus();
    }
    ui.add_space(2.0);
    caption(ui, "Storage driver");
    ComboBox::from_id_salt("new_project_driver")
        .selected_text(form.driver)
        .width(160.0)
        .show_ui(ui, |ui| {
            for driver in &form.drivers {
                ui.selectable_value(&mut form.driver, *driver, *driver);
            }
        })
        .response
        .widget_info(|| combo_info("Storage driver"));
    ui.add_space(2.0);
    if form.driver == LOCAL_DRIVER {
        show_local_driver_fields(ui, form);
        ui.checkbox(
            &mut form.attach,
            format!("Connect {} to this project", form.start_directory.display()),
        );
        caption(
            ui,
            "Agents in this directory and in directories below it use the project by default.",
        );
    } else {
        ui.label("The desktop app can create a project only with the local driver.");
    }
    error_line(ui, form.error.as_ref());
    buttons(ui, "Create project", None)
}

/// Draws the fields of the local driver: a path, and a button for the native folder dialog.
fn show_local_driver_fields(ui: &mut Ui, form: &mut NewProjectForm) {
    apply_folder_pick(form);
    let label = caption(ui, "Data directory");
    ui.horizontal(|ui| {
        let field = ui
            .add(
                TextEdit::singleline(&mut form.directory)
                    .hint_text("An absolute path, or empty for the default")
                    .desired_width(ui.available_width() - 80.0),
            )
            .labelled_by(label.id);
        if field.changed() {
            form.error = None;
        }
        let browse = ui.add_enabled(form.pending.is_none(), Button::new("Browse"));
        if browse.clicked() {
            form.pending = Some(folder_pick::start(ui.ctx(), &form.directory));
        }
    });
    if form.pending.is_some() {
        caption(ui, "Select a directory in the folder dialog.");
    } else if form.directory.trim().is_empty() {
        let default = form.paths.shared_project_dir(form.name.trim());
        caption(
            ui,
            &format!("With no path, lenka uses {}", default.display()),
        );
    } else {
        caption(
            ui,
            "Select an empty directory or a new one. A directory with a lenka project connects to that project.",
        );
    }
}

/// Moves the result of the native folder dialog into the form.
fn apply_folder_pick(form: &mut NewProjectForm) {
    let Some(pending) = &form.pending else {
        return;
    };
    match pending.state() {
        FolderPick::Waiting => {}
        FolderPick::Cancelled => form.pending = None,
        FolderPick::Selected(path) => {
            form.pending = None;
            match path.into_os_string().into_string() {
                Ok(text) => {
                    form.directory = text;
                    form.error = None;
                }
                Err(_) => {
                    form.error = Some(FormError::text(
                        "The path of this directory is not valid text. Select another directory.",
                    ));
                }
            }
        }
    }
}

fn show_evidence(ui: &mut Ui, form: &mut EvidenceForm) -> Outcome {
    if form.finish {
        heading(ui, "Finish the task");
    } else {
        heading(ui, &format!("Complete the stage {}", form.stage));
    }
    task_line(ui, &form.title);
    area_field(ui, "Evidence", &mut form.evidence, 4);
    caption(
        ui,
        "Give the proof of the work, such as a command and its result.",
    );
    if form.finish && form.unchecked > 0 {
        let palette = Palette::of(ui);
        ui.add_space(4.0);
        ui.label(
            RichText::new(format!(
                "{} acceptance criteria are not checked.",
                form.unchecked
            ))
            .color(palette.warning),
        );
        line_field(
            ui,
            "Reason to finish with unchecked criteria",
            &mut form.unchecked_reason,
            "Why this is acceptable",
        );
    }
    error_line(ui, form.error.as_ref());
    buttons(
        ui,
        if form.finish {
            "Finish task"
        } else {
            "Complete the stage"
        },
        None,
    )
}

fn show_return(ui: &mut Ui, form: &mut ReturnForm) -> Outcome {
    heading(ui, "Return to an earlier stage");
    task_line(ui, &form.title);
    caption(ui, "Stage");
    ComboBox::from_id_salt("return_stage")
        .selected_text(form.target.clone())
        .show_ui(ui, |ui| {
            for stage in &form.stages {
                ui.selectable_value(&mut form.target, stage.clone(), stage.as_str());
            }
        })
        .response
        .widget_info(|| combo_info("Stage"));
    area_field(ui, "Reason", &mut form.reason, 3);
    error_line(ui, form.error.as_ref());
    buttons(ui, "Return the task", None)
}

fn show_reason(ui: &mut Ui, form: &mut ReasonForm) -> Outcome {
    heading(ui, "Why did this change?");
    ui.label(format!(
        "A change to the {} is a material change. lenka tells the holders of related claims.",
        form.parts.join(", ")
    ));
    let response = line_field(ui, "Reason", &mut form.reason, "What changed, and why");
    if ui.memory(|memory| memory.focused().is_none()) {
        response.request_focus();
    }
    error_line(ui, form.error.as_ref());
    buttons(ui, "Save with reason", None)
}

fn show_dates(ui: &mut Ui, form: &mut DatesForm) -> Outcome {
    heading(ui, "Set the dates");
    task_line(ui, &form.title);
    ui.columns(2, |columns| {
        line_field(&mut columns[0], "Start date", &mut form.start, "YYYY-MM-DD");
        line_field(&mut columns[1], "Due date", &mut form.due, "YYYY-MM-DD");
    });
    error_line(ui, form.error.as_ref());
    buttons(ui, "Set dates", None)
}
