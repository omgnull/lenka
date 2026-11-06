// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The drawing code of the task detail panel.

use std::collections::HashMap;

use egui::{Button, Color32, ComboBox, Panel, RichText, ScrollArea, Stroke, TextEdit, Ui};
use lenka_artifacts::DRAWING_MEDIA_TYPE;
use lenka_core::{ProjectConfig, SecretPolicy};
use lenka_plugin_api::{Actor, ArtifactId, Day, TaskId, Timestamp};
use lenka_tasks::{CheckItem, EntryKind, Link, LinkState, Priority, Status, TaskPatch, TaskRow};

use super::{DetailState, LinkKind, Problem, Tab};
use crate::actions::{Action, TaskOp};
use crate::data::BoardModel;
use crate::dialogs::{DatesForm, Dialog};
use crate::session::Session;
use crate::theme::{Palette, priority_name, status_name};
use crate::timeline::parse_day_field;
use crate::widgets::{
    caption, chip, combo_info, danger_button, format_bytes, inline_field, markdown_editor,
    primary_button, section, underline_tab,
};

/// Formats a time to the minute, in UTC.
pub(crate) fn short_time(at: Timestamp) -> String {
    let text = at.to_string();
    format!(
        "{} {} UTC",
        text.get(..10).unwrap_or(""),
        text.get(11..16).unwrap_or("")
    )
}

fn actor_name(actor: Actor) -> &'static str {
    match actor {
        Actor::Agent => "agent",
        Actor::Human => "person",
        Actor::Any => "any actor",
    }
}

/// The position of a task on the stages of its type.
#[derive(Debug, Clone)]
pub(crate) struct StagePlace {
    pub position: usize,
    pub stages: Vec<String>,
    pub actor: Actor,
}

/// Finds the position of a stage. A missing stage gives `None`.
pub(crate) fn stage_place(config: &ProjectConfig, kind: &str, stage: &str) -> Option<StagePlace> {
    let def = config.types.get(kind)?;
    let position = def.stages.iter().position(|item| item.name == stage)?;
    Some(StagePlace {
        position,
        stages: def.stages.iter().map(|item| item.name.clone()).collect(),
        actor: def.stages[position].actor,
    })
}

impl DetailState {
    fn field_patch(&self) -> TaskPatch {
        TaskPatch {
            expected_version: self.data.meta.state.version,
            ..TaskPatch::default()
        }
    }

    fn send(&self, actions: &mut Vec<Action>, patch: TaskPatch) {
        actions.push(Action::Task(TaskOp::Update {
            id: self.id,
            patch: Box::new(patch),
            from_draft: false,
        }));
    }

    /// Draws the panel.
    pub(crate) fn show(&mut self, ui: &mut Ui, session: &Session, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        if self.is_dirty() {
            Panel::bottom("detail_save_bar")
                .frame(egui::Frame::new().fill(palette.surface).inner_margin(10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if primary_button(ui, "Save").clicked() {
                            actions.push(Action::SaveDraft);
                        }
                        if ui.button("Discard").clicked() {
                            actions.push(Action::DiscardDraft);
                        }
                        ui.label(RichText::new("Unsaved changes").color(palette.warning));
                    });
                });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(12))
            .show(ui, |ui| {
                self.header(ui, session, actions);
                self.problem_banner(ui, actions);
                self.stage_bar(ui, session, actions);
                ui.add_space(4.0);
                self.tabs(ui);
                ui.add_space(6.0);
                ScrollArea::vertical()
                    .id_salt("detail_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.tab {
                        Tab::Details => self.details_tab(ui, actions),
                        Tab::Plan => self.plan_tab(ui),
                        Tab::Entries => self.entries_tab(ui, actions),
                        Tab::Relations => self.relations_tab(ui, session, actions),
                        Tab::Artifacts => self.artifacts_tab(ui, actions),
                        Tab::History => self.history_tab(ui, actions),
                    });
            });
    }

    fn header(&mut self, ui: &mut Ui, session: &Session, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        let front = &self.data.meta.front;
        ui.horizontal(|ui| {
            chip(ui, &front.kind, palette.text_soft);
            ui.label(
                RichText::new(front.id.short())
                    .monospace()
                    .color(palette.text_muted),
            );
            if self.data.meta.state.version > 0 {
                ui.label(
                    RichText::new(format!("Version {}", self.data.meta.state.version))
                        .small()
                        .color(palette.text_muted),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    actions.push(Action::CloseDetail);
                }
            });
        });
        let title_caption = caption(ui, "Title");
        ui.add(
            TextEdit::singleline(&mut self.draft.title)
                .font(egui::FontId::proportional(16.5))
                .desired_width(f32::INFINITY),
        )
        .labelled_by(title_caption.id);

        ui.add_space(2.0);
        let status = self.data.meta.state.status;
        let priority = self.data.meta.front.priority;
        let kind = self.data.meta.front.kind.clone();
        let config = session.project.config();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                caption(ui, "Status");
                ComboBox::from_id_salt("detail_status")
                    .selected_text(RichText::new(status_name(status)).color(palette.status(status)))
                    .width(116.0)
                    .show_ui(ui, |ui| {
                        for item in Status::ALL {
                            if ui
                                .selectable_label(item == status, status_name(item))
                                .clicked()
                                && item != status
                            {
                                actions.push(Action::MoveTo {
                                    id: self.id,
                                    status: item,
                                });
                            }
                        }
                    })
                    .response
                    .widget_info(|| combo_info("Status"));
            });
            ui.vertical(|ui| {
                caption(ui, "Priority");
                ComboBox::from_id_salt("detail_priority")
                    .selected_text(priority_name(priority))
                    .width(100.0)
                    .show_ui(ui, |ui| {
                        for item in Priority::ALL {
                            if ui
                                .selectable_label(item == priority, priority_name(item))
                                .clicked()
                                && item != priority
                            {
                                let patch = TaskPatch {
                                    priority: Some(item),
                                    ..self.field_patch()
                                };
                                self.send(actions, patch);
                            }
                        }
                    })
                    .response
                    .widget_info(|| combo_info("Priority"));
            });
            ui.vertical(|ui| {
                caption(ui, "Type");
                ComboBox::from_id_salt("detail_kind")
                    .selected_text(kind.clone())
                    .width(100.0)
                    .show_ui(ui, |ui| {
                        for item in config.types.keys() {
                            if ui.selectable_label(*item == kind, item.as_str()).clicked()
                                && *item != kind
                            {
                                let patch = TaskPatch {
                                    kind: Some(item.clone()),
                                    ..self.field_patch()
                                };
                                self.send(actions, patch);
                            }
                        }
                    })
                    .response
                    .widget_info(|| combo_info("Type"));
            });
        });
    }

    fn problem_banner(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        let newer = self.has_newer_version() && self.problem.is_none();
        if newer {
            notice(ui, palette.warning, palette.surface_alt, |ui| {
                ui.label(
                    RichText::new(format!(
                        "Another writer saved version {}. Your edits start from version {}.",
                        self.data.meta.state.version, self.base_version
                    ))
                    .color(palette.warning),
                );
                ui.horizontal(|ui| {
                    if ui.button("Use the new version and keep my text").clicked() {
                        actions.push(Action::ReloadDetail);
                    }
                });
            });
        }
        let Some(problem) = &self.problem else {
            return;
        };
        match problem {
            Problem::Stale(message) => {
                let message = message.clone();
                notice(ui, palette.danger, palette.danger_soft, |ui| {
                    ui.label(
                        RichText::new("Another writer saved this task first.")
                            .color(palette.danger),
                    );
                    ui.label(RichText::new(message).color(palette.danger));
                    ui.label(
                        "Your text is still here. Reload to save it on top of the new version.",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Reload").clicked() {
                            actions.push(Action::ReloadDetail);
                        }
                        if ui.button("Discard my text").clicked() {
                            actions.push(Action::DiscardDraft);
                        }
                    });
                });
            }
            Problem::Secret { message, .. } => {
                let message = message.clone();
                notice(ui, palette.danger, palette.danger_soft, |ui| {
                    ui.label(RichText::new("lenka did not save this text.").color(palette.danger));
                    ui.label(RichText::new(message).color(palette.danger));
                    ui.label("Remove the value, or save it with an explicit choice. Rotate a real secret.");
                    if danger_button(ui, "Save with this value").clicked() {
                        actions.push(Action::SaveWithSecret);
                    }
                });
            }
            Problem::Error(message) => {
                let message = message.clone();
                notice(ui, palette.danger, palette.danger_soft, |ui| {
                    ui.label(RichText::new(message).color(palette.danger));
                });
            }
        }
    }

    fn stage_bar(&mut self, ui: &mut Ui, session: &Session, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        let state = &self.data.meta.state;
        let closed = state.status.is_closed();
        let place = stage_place(
            &session.project.config(),
            &self.data.meta.front.kind,
            &state.stage,
        );
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(palette.surface_alt)
            .corner_radius(7)
            .inner_margin(8)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let text = match &place {
                    Some(place) => format!(
                        "Stage {} of {}: {} (for {})",
                        place.position + 1,
                        place.stages.len(),
                        state.stage,
                        actor_name(place.actor)
                    ),
                    None => format!("Stage: {} (not in the settings)", state.stage),
                };
                ui.label(RichText::new(text).color(palette.text));
                ui.horizontal_wrapped(|ui| {
                    let last = place
                        .as_ref()
                        .is_none_or(|place| place.position + 1 >= place.stages.len());
                    let first = place.as_ref().is_none_or(|place| place.position == 0);
                    let complete = ui
                        .add_enabled(!closed && !last, Button::new("Complete stage"))
                        .on_disabled_hover_text("This is the last stage. Finish the task.");
                    if complete.clicked() {
                        actions.push(Action::OpenCompleteStage(self.id));
                    }
                    let back = ui.add_enabled(!closed && !first, Button::new("Return to stage"));
                    if back.clicked() {
                        actions.push(Action::OpenReturnStage(self.id));
                    }
                    if ui.add_enabled(!closed, Button::new("Finish")).clicked() {
                        actions.push(Action::OpenFinish(self.id));
                    }
                    if ui
                        .add_enabled(
                            !closed && state.status != Status::Paused,
                            Button::new("Pause"),
                        )
                        .clicked()
                    {
                        actions.push(Action::Task(TaskOp::Pause { id: self.id }));
                    }
                });
            });
    }

    fn tabs(&mut self, ui: &mut Ui) {
        let palette = Palette::of(ui);
        let rect = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                for (tab, name) in [
                    (Tab::Details, "Details"),
                    (Tab::Plan, "Plan"),
                    (Tab::Entries, "Entries"),
                    (Tab::Relations, "Relations"),
                    (Tab::Artifacts, "Artifacts"),
                    (Tab::History, "History"),
                ] {
                    if underline_tab(ui, name, self.tab == tab).clicked() {
                        self.tab = tab;
                    }
                }
            })
            .response
            .rect;
        ui.painter().hline(
            rect.x_range()
                .expand(200.0)
                .intersection(ui.max_rect().x_range()),
            rect.bottom(),
            Stroke::new(1.0, palette.border),
        );
    }

    fn details_tab(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        for warning in &self.data.warnings {
            notice(ui, palette.warning, palette.surface_alt, |ui| {
                ui.label(RichText::new(warning).color(palette.warning));
            });
        }
        self.claim_and_blockers(ui, &palette, actions);
        self.labels_and_dates(ui, &palette, actions);
        ui.add_space(6.0);
        let [description, requirements, acceptance, _] = &mut self.editors;
        markdown_editor(
            ui,
            "Description",
            &mut self.draft.description,
            description,
            4,
        );
        ui.add_space(4.0);
        markdown_editor(
            ui,
            "Requirements",
            &mut self.draft.requirements,
            requirements,
            3,
        );
        ui.add_space(4.0);
        markdown_editor(
            ui,
            "Acceptance criteria",
            &mut self.draft.acceptance,
            acceptance,
            3,
        );
        self.criteria(ui, actions);
        for (heading, text) in &self.data.other_sections {
            ui.add_space(4.0);
            section(ui, heading);
            ui.label(text.as_str());
        }
        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            caption(
                ui,
                &format!(
                    "Created by {} at {}",
                    self.data.meta.front.created_by,
                    short_time(self.data.meta.front.created_at)
                ),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if danger_button(ui, "Delete task").clicked() {
                    actions.push(Action::Open(Dialog::Delete {
                        id: self.id,
                        title: self.data.meta.front.title.clone(),
                        error: None,
                    }));
                }
            });
        });
    }

    fn claim_and_blockers(&self, ui: &mut Ui, palette: &Palette, actions: &mut Vec<Action>) {
        match &self.data.claim {
            Some(claim) => {
                ui.label(
                    RichText::new(format!(
                        "Claim: {} holds the stage {} until {}",
                        claim.holder,
                        claim.stage,
                        short_time(claim.expires_at)
                    ))
                    .color(palette.warning),
                );
            }
            None => {
                ui.label(RichText::new("Claim: nobody holds this stage").color(palette.text_muted));
            }
        }
        if !self.data.blockers.is_empty() {
            ui.label(
                RichText::new(format!("Blocked by {} tasks", self.data.blockers.len()))
                    .color(palette.danger),
            );
            for blocker in &self.data.blockers {
                let status = blocker.status.map_or("unknown", status_name);
                let title = blocker.title.as_deref().unwrap_or("Title not visible");
                let mut text = format!("{} ({status}) {title}", blocker.id);
                if let Some(parent) = &blocker.via_parent {
                    text.push_str(&format!(" through the parent {parent}"));
                }
                match self.known_ids.get(&blocker.id) {
                    Some(id) => {
                        if ui.selectable_label(false, text).clicked() {
                            actions.push(Action::Select(*id));
                        }
                    }
                    None => {
                        ui.label(text);
                    }
                }
            }
        }
    }

    fn labels_and_dates(&mut self, ui: &mut Ui, palette: &Palette, actions: &mut Vec<Action>) {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            caption(ui, "Labels");
            let labels = self.data.meta.front.labels.clone();
            if labels.is_empty() {
                ui.label(RichText::new("None").small().color(palette.text_muted));
            }
            for label in labels {
                chip(ui, &label, palette.accent);
                let name = format!("Remove the label {label}");
                let response = ui
                    .add(Button::new("x").small().frame(false))
                    .on_hover_text(name.clone());
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::accesskit::Role::Button, true, &name)
                });
                if response.clicked() {
                    let patch = TaskPatch {
                        labels_remove: vec![label.clone()],
                        ..self.field_patch()
                    };
                    self.send(actions, patch);
                }
            }
        });
        ui.horizontal(|ui| {
            let response = inline_field(ui, "Add label", &mut self.label_input, 110.0, "Name");
            if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                let label = self.label_input.trim().to_owned();
                if !label.is_empty() {
                    let patch = TaskPatch {
                        labels_add: vec![label],
                        ..self.field_patch()
                    };
                    self.send(actions, patch);
                    self.label_input.clear();
                }
            }
        });
        ui.horizontal(|ui| {
            let start = inline_field(ui, "Start", &mut self.start_input, 92.0, "YYYY-MM-DD");
            let due = inline_field(ui, "Due", &mut self.due_input, 92.0, "YYYY-MM-DD");
            if start.lost_focus() || due.lost_focus() {
                self.apply_dates(actions);
            }
        });
        if let Some(error) = &self.field_error {
            ui.label(RichText::new(error).color(palette.danger));
        }
    }

    fn apply_dates(&mut self, actions: &mut Vec<Action>) {
        let front = &self.data.meta.front;
        match (
            parse_day_field(&self.start_input),
            parse_day_field(&self.due_input),
        ) {
            (Ok(start), Ok(due)) => {
                self.field_error = None;
                let mut patch = self.field_patch();
                let change = |old: Option<Day>, new: Option<Day>| (old != new).then_some(new);
                patch.start = change(front.start, start);
                patch.due = change(front.due, due);
                if patch.start.is_some() || patch.due.is_some() {
                    self.send(actions, patch);
                }
            }
            (Err(message), _) | (_, Err(message)) => self.field_error = Some(message),
        }
    }

    fn criteria(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        if self.data.criteria.is_empty() {
            return;
        }
        let palette = Palette::of(ui);
        ui.add_space(4.0);
        let edited = self.draft.acceptance != self.data.acceptance;
        let done = self.data.criteria.iter().filter(|item| item.done).count();
        caption(
            ui,
            &format!(
                "Check marks: {done} of {} complete",
                self.data.criteria.len()
            ),
        );
        ui.add_enabled_ui(!edited, |ui| {
            for (index, item) in self.data.criteria.iter().enumerate() {
                let mut checked = item.done;
                let color = if item.done {
                    palette.text_muted
                } else {
                    palette.text_soft
                };
                let text = RichText::new(&item.text).color(color);
                if ui.checkbox(&mut checked, text).changed() {
                    let patch = TaskPatch {
                        check: vec![CheckItem {
                            index,
                            done: checked,
                        }],
                        ..self.field_patch()
                    };
                    self.send(actions, patch);
                }
            }
        });
        if edited {
            caption(ui, "Save the text of the criteria first.");
        }
    }

    fn plan_tab(&mut self, ui: &mut Ui) {
        let [_, _, _, plan] = &mut self.editors;
        markdown_editor(ui, "Plan", &mut self.draft.plan, plan, 18);
        caption(ui, "A plan is optional. An empty text removes the plan.");
    }

    fn entries_tab(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        section(ui, "Add an entry");
        ui.horizontal(|ui| {
            for (kind, name) in [
                (EntryKind::Comment, "Comment"),
                (EntryKind::Decision, "Decision"),
            ] {
                ui.selectable_value(&mut self.entry_kind, kind, name);
            }
        });
        let label = caption(ui, "Entry text");
        ui.add(
            TextEdit::multiline(&mut self.entry_text)
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        )
        .labelled_by(label.id);
        if let Some(error) = &self.entry_error {
            ui.label(RichText::new(&error.message).color(palette.danger));
        }
        ui.horizontal(|ui| {
            let mut policy = None;
            if primary_button(ui, "Add entry").clicked() {
                policy = Some(SecretPolicy::Enforce);
            }
            if self.entry_error.as_ref().is_some_and(|error| error.secret)
                && danger_button(ui, "Add with this value").clicked()
            {
                policy = Some(SecretPolicy::PersonOverride);
            }
            if let Some(policy) = policy {
                actions.push(Action::AddEntry {
                    id: self.id,
                    kind: self.entry_kind,
                    text: self.entry_text.clone(),
                    policy,
                });
            }
        });
        ui.add_space(8.0);
        section(ui, &format!("Entries ({})", self.data.entries.len()));
        if self.data.entries.is_empty() {
            ui.label(RichText::new("No entries yet.").color(palette.text_muted));
        }
        for entry in self.data.entries.iter().rev() {
            egui::Frame::new()
                .fill(palette.surface)
                .stroke(Stroke::new(1.0, palette.border))
                .corner_radius(7)
                .inner_margin(9)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        let color = match entry.kind {
                            EntryKind::Decision => palette.accent,
                            EntryKind::Evidence => palette.success,
                            EntryKind::Proposal => palette.warning,
                            EntryKind::Comment => palette.text_soft,
                        };
                        chip(ui, entry.kind.as_str(), color);
                        ui.label(RichText::new(&entry.author).color(palette.text_soft));
                        ui.label(
                            RichText::new(short_time(entry.at))
                                .small()
                                .color(palette.text_muted),
                        );
                        if let Some(stage) = &entry.stage {
                            ui.label(
                                RichText::new(format!("Stage {stage}"))
                                    .small()
                                    .color(palette.text_muted),
                            );
                        }
                    });
                    ui.label(entry.text.as_str());
                });
            ui.add_space(2.0);
        }
    }

    fn relations_tab(&mut self, ui: &mut Ui, session: &Session, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        if self.related.is_none() {
            match session.tasks.related(self.id) {
                Ok(view) => {
                    for row in view
                        .blocks
                        .iter()
                        .chain(&view.children)
                        .chain(&view.similar)
                    {
                        if let Ok(id) = session.tasks.resolve(&row.id) {
                            self.known_ids.insert(row.id.clone(), id);
                        }
                    }
                    self.related = Some(view);
                }
                Err(error) => {
                    ui.label(RichText::new(error.message()).color(palette.danger));
                    return;
                }
            }
        }
        let model = &session.model;
        let front = self.data.meta.front.clone();
        let mut remove = None;

        section(ui, "Parent");
        match front.parent {
            Some(parent) => {
                task_link(ui, model, parent, actions);
                if ui.small_button("Remove the parent").clicked() {
                    remove = Some(TaskPatch {
                        parent: Some(None),
                        ..self.field_patch()
                    });
                }
            }
            None => {
                ui.label(RichText::new("No parent").color(palette.text_muted));
            }
        }

        section(ui, "Blocked by");
        let local_blockers = front
            .blocked_by
            .iter()
            .filter_map(|link| link.local())
            .collect::<Vec<_>>();
        if local_blockers.is_empty() {
            ui.label(RichText::new("No local blockers").color(palette.text_muted));
        }
        for target in &local_blockers {
            ui.horizontal(|ui| {
                task_link(ui, model, *target, actions);
                if remove_button(ui, *target).clicked() {
                    remove = Some(TaskPatch {
                        blocked_by_remove: vec![Link::Local(*target)],
                        ..self.field_patch()
                    });
                }
            });
        }

        section(ui, "Relates to");
        let local_relates = front
            .relates
            .iter()
            .filter_map(|link| link.local())
            .collect::<Vec<_>>();
        if local_relates.is_empty() {
            ui.label(RichText::new("No related tasks").color(palette.text_muted));
        }
        for target in &local_relates {
            ui.horizontal(|ui| {
                task_link(ui, model, *target, actions);
                if remove_button(ui, *target).clicked() {
                    remove = Some(TaskPatch {
                        relates_remove: vec![Link::Local(*target)],
                        ..self.field_patch()
                    });
                }
            });
        }

        if let Some(related) = &self.related {
            let known = &self.known_ids;
            rows_section(
                ui,
                known,
                "Blocks",
                &related.blocks,
                "No open task waits for this task",
                actions,
            );
            rows_section(
                ui,
                known,
                "Children",
                &related.children,
                "No children",
                actions,
            );
            if !related.soft_links.is_empty() {
                section(ui, "Links to other projects");
                for view in &related.soft_links {
                    let (state, color) = match view.state {
                        LinkState::Ok => ("Available", palette.success),
                        LinkState::Unavailable => ("Unavailable", palette.warning),
                        LinkState::Broken => ("Broken", palette.danger),
                    };
                    ui.horizontal_wrapped(|ui| {
                        chip(ui, state, color);
                        let title = view.title.as_deref().unwrap_or("Title not visible");
                        let status = view.status.map_or("", status_name);
                        ui.label(format!("{title} {status}"));
                        if ui.small_button("Remove").clicked()
                            && let Ok(link) = Link::parse(&view.link)
                        {
                            let is_blocker = front.blocked_by.contains(&link);
                            remove = Some(if is_blocker {
                                TaskPatch {
                                    blocked_by_remove: vec![link],
                                    ..self.field_patch()
                                }
                            } else {
                                TaskPatch {
                                    relates_remove: vec![link],
                                    ..self.field_patch()
                                }
                            });
                        }
                    });
                    caption(ui, &view.link);
                }
            }
            rows_section(
                ui,
                known,
                "Similar tasks",
                &related.similar,
                "No similar tasks",
                actions,
            );
        }
        if let Some(patch) = remove {
            self.send(actions, patch);
        }

        ui.add_space(6.0);
        section(ui, "Add a link");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.link_kind, LinkKind::BlockedBy, "Blocked by");
            ui.selectable_value(&mut self.link_kind, LinkKind::Relates, "Relates to");
            ui.selectable_value(&mut self.link_kind, LinkKind::Parent, "Parent");
        });
        ui.horizontal(|ui| {
            inline_field(
                ui,
                "Search",
                &mut self.link_search,
                220.0,
                "Title, short ID, or project-id/task-id",
            );
            if self.link_search.contains('/') && ui.button("Add link").clicked() {
                match session.tasks.resolve_link(self.link_search.trim()) {
                    Ok(link) => {
                        let patch = self.link_patch(link);
                        self.send(actions, patch);
                        self.link_search.clear();
                    }
                    Err(error) => self.field_error = Some(error.message().to_owned()),
                }
            }
        });
        if let Some(error) = &self.field_error {
            ui.label(RichText::new(error).color(palette.danger));
        }
        let query = self.link_search.trim().to_lowercase();
        if !query.is_empty() && !query.contains('/') {
            let matches: Vec<_> = model
                .rows
                .iter()
                .filter(|row| row.meta.id() != self.id)
                .filter(|row| {
                    row.meta.front.title.to_lowercase().contains(&query)
                        || row.meta.id().matches_short(&query)
                })
                .take(8)
                .collect();
            if matches.is_empty() {
                ui.label(RichText::new("No task matches.").color(palette.text_muted));
            }
            let mut chosen = None;
            for row in matches {
                let text = format!("{}  {}", row.meta.id().short(), row.meta.front.title);
                if ui.selectable_label(false, text).clicked() {
                    chosen = Some(row.meta.id());
                }
            }
            if let Some(target) = chosen {
                let patch = self.link_patch(Link::Local(target));
                self.send(actions, patch);
                self.link_search.clear();
            }
        }
        ui.add_space(8.0);
        if ui.button("Add a child task").clicked() {
            actions.push(Action::OpenNewTask {
                parent: Some(self.id),
            });
        }
    }

    fn link_patch(&self, link: Link) -> TaskPatch {
        let mut patch = self.field_patch();
        match self.link_kind {
            LinkKind::BlockedBy => patch.blocked_by_add = vec![link],
            LinkKind::Relates => patch.relates_add = vec![link],
            LinkKind::Parent => patch.parent = Some(link.local()),
        }
        patch
    }

    fn artifacts_tab(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        section(
            ui,
            &format!("Linked artifacts ({})", self.data.artifacts.len()),
        );
        if self.data.artifacts.is_empty() {
            ui.label(RichText::new("No artifacts yet.").color(palette.text_muted));
        }
        let mut unlink = None;
        for artifact in &self.data.artifacts {
            egui::Frame::new()
                .fill(palette.surface)
                .stroke(Stroke::new(1.0, palette.border))
                .corner_radius(7)
                .inner_margin(9)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(RichText::new(&artifact.name).color(palette.text));
                    ui.horizontal_wrapped(|ui| {
                        let is_drawing = artifact.media_type == DRAWING_MEDIA_TYPE;
                        let kind = if is_drawing {
                            "Drawing"
                        } else {
                            artifact.media_type.as_str()
                        };
                        caption(
                            ui,
                            &format!(
                                "{kind}, version {}, {}",
                                artifact.version,
                                format_bytes(artifact.size)
                            ),
                        );
                        if !artifact.downloaded {
                            chip(ui, "Not downloaded", palette.warning);
                        }
                    });
                    ui.horizontal(|ui| {
                        let parsed = ArtifactId::parse(&artifact.full_id).ok();
                        if artifact.media_type == DRAWING_MEDIA_TYPE
                            && ui
                                .add_enabled(artifact.downloaded, Button::new("Open drawing"))
                                .clicked()
                            && let Some(id) = parsed
                        {
                            actions.push(Action::OpenDrawing { artifact: id });
                        }
                        if ui.button("Unlink").clicked() {
                            unlink = parsed;
                        }
                    });
                });
            ui.add_space(2.0);
        }
        if let Some(id) = unlink {
            let patch = TaskPatch {
                artifacts_remove: vec![id],
                ..self.field_patch()
            };
            self.send(actions, patch);
        }
        ui.add_space(6.0);
        section(ui, "Add");
        if ui.button("New drawing").clicked() {
            actions.push(Action::Open(Dialog::NewDrawing {
                task: self.id,
                name: String::new(),
                error: None,
            }));
        }
        ui.horizontal(|ui| {
            inline_field(
                ui,
                "File path",
                &mut self.artifact_path,
                230.0,
                "/path/to/file",
            );
            if ui.button("Add file").clicked() && !self.artifact_path.trim().is_empty() {
                actions.push(Action::AddFile {
                    id: self.id,
                    path: self.artifact_path.trim().to_owned(),
                });
                self.artifact_path.clear();
            }
        });
        caption(ui, "You can also drop a file onto the window.");
    }

    fn history_tab(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        if let Some(old) = &self.old_version {
            let mut close = false;
            egui::Frame::new()
                .fill(palette.surface_alt)
                .corner_radius(7)
                .inner_margin(9)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("The text of version {}", old.version))
                                .color(palette.text),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close = ui.button("Close text").clicked();
                        });
                    });
                    ScrollArea::vertical()
                        .id_salt("old_version")
                        .max_height(280.0)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(&old.task)
                                    .monospace()
                                    .color(palette.text_soft),
                            );
                            if let Some(plan) = &old.plan {
                                ui.separator();
                                ui.label(RichText::new("Plan").color(palette.text));
                                ui.label(RichText::new(plan).monospace().color(palette.text_soft));
                            }
                        });
                });
            if close {
                self.old_version = None;
            }
            ui.add_space(6.0);
        }
        section(ui, &format!("Versions ({})", self.data.versions.len()));
        for version in self.data.versions.iter().rev() {
            egui::Frame::new()
                .fill(palette.surface)
                .stroke(Stroke::new(1.0, palette.border))
                .corner_radius(7)
                .inner_margin(9)
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(format!("Version {}", version.version))
                                .color(palette.text),
                        );
                        ui.label(RichText::new(&version.author).color(palette.text_soft));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let name = format!("Show the text of version {}", version.version);
                            let response = ui.small_button("Show text");
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::accesskit::Role::Button,
                                    true,
                                    &name,
                                )
                            });
                            if response.clicked() {
                                actions.push(Action::ShowVersion {
                                    id: self.id,
                                    version: version.version,
                                });
                            }
                        });
                    });
                    caption(ui, &short_time(version.at));
                    if !version.reason.is_empty() {
                        ui.label(version.reason.as_str());
                    }
                    if !version.changed.is_empty() {
                        caption(ui, &format!("Changed: {}", version.changed.join(", ")));
                    }
                });
            ui.add_space(2.0);
        }
    }
}

fn notice(ui: &mut Ui, border: Color32, fill: Color32, content: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, border.gamma_multiply(0.5)))
        .corner_radius(7)
        .inner_margin(8)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            content(ui);
        });
    ui.add_space(4.0);
}

fn remove_button(ui: &mut Ui, target: TaskId) -> egui::Response {
    let response = ui.small_button("Remove");
    let name = format!("Remove the link to {}", target.short());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::accesskit::Role::Button, true, &name));
    response
}

fn task_link(ui: &mut Ui, model: &BoardModel, target: TaskId, actions: &mut Vec<Action>) {
    let palette = Palette::of(ui);
    match model.row(target) {
        Some(row) => {
            let status = status_name(row.meta.state.status);
            let text = format!("{} ({status}) {}", target.short(), row.meta.front.title);
            if ui.selectable_label(false, text).clicked() {
                actions.push(Action::Select(target));
            }
        }
        None => {
            ui.label(RichText::new(format!("{} (missing)", target.short())).color(palette.danger));
        }
    }
}

fn rows_section(
    ui: &mut Ui,
    known: &HashMap<String, TaskId>,
    title: &str,
    rows: &[TaskRow],
    empty: &str,
    actions: &mut Vec<Action>,
) {
    let palette = Palette::of(ui);
    section(ui, title);
    if rows.is_empty() {
        ui.label(RichText::new(empty).color(palette.text_muted));
    }
    for row in rows {
        let text = format!("{} ({}) {}", row.id, status_name(row.status), row.title);
        match known.get(&row.id) {
            Some(id) => {
                if ui.selectable_label(false, text).clicked() {
                    actions.push(Action::Select(*id));
                }
            }
            None => {
                ui.label(text);
            }
        }
    }
}

/// Opens the dates dialog from a board row.
pub(crate) fn dates_form_of(row: &lenka_tasks::BoardRow) -> DatesForm {
    let front = &row.meta.front;
    DatesForm {
        id: front.id,
        title: front.title.clone(),
        version: row.meta.state.version,
        start: front.start.map_or_else(String::new, |day| day.to_string()),
        due: front.due.map_or_else(String::new, |day| day.to_string()),
        error: None,
    }
}
