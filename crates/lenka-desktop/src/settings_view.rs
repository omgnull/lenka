// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The project settings, the plugin list, and the `doctor` report.

use egui::{Button, ComboBox, DragValue, RichText, ScrollArea, Stroke, TextEdit, Ui};
use lenka_core::{Host, ProjectConfig, Stage, TypeDef};
use lenka_plugin_api::Actor;
use lenka_tasks::{BoardRow, DoctorReport};

use crate::actions::Action;
use crate::session::Session;
use crate::theme::Palette;
use crate::widgets::{caption, chip, combo_info, danger_button, inline_field, section};

/// The state of the settings view.
#[derive(Debug, Default)]
pub(crate) struct SettingsState {
    /// The draft and the settings that the draft started from.
    pub draft: Option<ProjectConfig>,
    base: Option<ProjectConfig>,
    link_to: String,
    accept_from: String,
    new_type: String,
    pub error: Option<String>,
    pub saved: bool,
    /// Tells if another writer changed the file after the draft loaded.
    pub conflict: bool,
    pub doctor: Option<DoctorReport>,
}

impl SettingsState {
    /// Loads the settings of the project into the draft, unless the person edits a draft.
    pub(crate) fn load(&mut self, session: &Session) {
        let current = (*session.project.config()).clone();
        let edited = self.draft.is_some() && self.draft != self.base;
        if edited {
            return;
        }
        self.link_to = current.links.link_to.join("\n");
        self.accept_from = current.links.accept_from.join("\n");
        self.draft = Some(current.clone());
        self.base = Some(current);
    }

    /// Returns the settings that the draft started from.
    pub(crate) fn base(&self) -> Option<&ProjectConfig> {
        self.base.as_ref()
    }

    /// Tells if the draft holds changes that are not saved.
    pub(crate) fn is_dirty(&self) -> bool {
        self.draft != self.base
            || self.base.as_ref().is_some_and(|base| {
                lines_of(&self.link_to) != base.links.link_to
                    || lines_of(&self.accept_from) != base.links.accept_from
            })
    }

    /// Forgets the state of another project.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Returns the settings to save, or a message about a problem.
    pub(crate) fn validated(&mut self, rows: &[BoardRow]) -> Result<ProjectConfig, String> {
        let mut draft = self
            .draft
            .clone()
            .ok_or_else(|| "The settings did not load.".to_owned())?;
        draft.links.link_to = lines_of(&self.link_to);
        draft.links.accept_from = lines_of(&self.accept_from);
        check_config(&draft, rows)?;
        Ok(draft)
    }

    /// Records a successful save.
    pub(crate) fn mark_saved(&mut self, saved: ProjectConfig) {
        self.draft = Some(saved.clone());
        self.base = Some(saved);
        self.error = None;
        self.conflict = false;
        self.saved = true;
    }

    /// Draws the view.
    pub(crate) fn show(&mut self, ui: &mut Ui, host: &Host, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        ScrollArea::vertical().id_salt("settings").auto_shrink([false, false]).show(ui, |ui| {
            let width = ui.available_width().min(860.0);
            ui.set_max_width(width);
            ui.label(RichText::new("Project settings").size(19.0).color(palette.text));
            ui.add_space(6.0);
            self.save_bar(ui, actions);
            if let Some(draft) = &mut self.draft {
                card(ui, |ui| limits_section(ui, draft));
                card(ui, |ui| {
                    section(ui, "Links between projects");
                    caption(ui, "Both projects must allow a link. Write one project ID on each line. * means any.");
                    ui.columns(2, |columns| {
                        area(&mut columns[0], "Can link to", &mut self.link_to);
                        area(&mut columns[1], "Accepts links from", &mut self.accept_from);
                    });
                });
                card(ui, |ui| types_section(ui, draft, &mut self.new_type));
            }
            card(ui, |ui| plugins_section(ui, host));
            card(ui, |ui| self.doctor_section(ui, actions));
        });
    }

    fn save_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        let edited = self.is_dirty();
        if edited {
            self.saved = false;
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(edited, primary_action_button(ui)).clicked() {
                actions.push(Action::SaveSettings);
            }
            if edited {
                ui.label(RichText::new("Unsaved changes").color(palette.warning));
            } else if self.saved {
                ui.label(RichText::new("Saved").color(palette.success));
            }
        });
        if let Some(error) = &self.error {
            ui.label(RichText::new(error).color(palette.danger));
        }
        if self.conflict && ui.button("Load the file and lose my changes").clicked() {
            actions.push(Action::ReloadSettings);
        }
        caption(
            ui,
            "A save writes lenka.toml again. lenka keeps other tables in the file, but it removes comments.",
        );
        ui.add_space(4.0);
    }

    fn doctor_section(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        section(ui, "Project check");
        caption(
            ui,
            "The check reports invalid files, broken links, cycles, and missing stages.",
        );
        if ui.button("Run check").clicked() {
            actions.push(Action::RunDoctor);
        }
        let Some(report) = &self.doctor else {
            return;
        };
        if report.is_clean() {
            ui.label(RichText::new("The check found no problems.").color(palette.success));
            return;
        }
        let cycles: Vec<String> = report
            .cycles
            .iter()
            .map(|cycle| cycle.join(" > "))
            .collect();
        let unfinished: Vec<String> = report.unfinished_write.iter().cloned().collect();
        for (title, items) in [
            ("An unfinished write stops all writes", &unfinished),
            ("Invalid files", &report.invalid_files),
            ("Directories with a bad name", &report.bad_names),
            ("Relation cycles", &cycles),
            ("Broken links", &report.broken_links),
            ("Unavailable links", &report.unavailable_links),
            ("Missing types or stages", &report.missing_stages),
        ] {
            if !items.is_empty() {
                ui.label(RichText::new(format!("{title} ({})", items.len())).color(palette.danger));
                for item in items {
                    ui.label(RichText::new(item).small().color(palette.text_soft));
                }
            }
        }
    }
}

fn primary_action_button(ui: &Ui) -> Button<'static> {
    let palette = Palette::of(ui);
    Button::new(RichText::new("Save settings").color(palette.on_accent)).fill(palette.accent)
}

fn lines_of(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn card(ui: &mut Ui, content: impl FnOnce(&mut Ui)) {
    let palette = Palette::of(ui);
    egui::Frame::new()
        .fill(palette.surface)
        .stroke(Stroke::new(1.0, palette.border))
        .corner_radius(8)
        .inner_margin(14)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            content(ui);
        });
    ui.add_space(8.0);
}

fn area(ui: &mut Ui, label: &str, text: &mut String) {
    let response = caption(ui, label);
    ui.add(
        TextEdit::multiline(text)
            .desired_rows(2)
            .desired_width(f32::INFINITY)
            .font(egui::TextStyle::Monospace),
    )
    .labelled_by(response.id);
}

fn limits_section(ui: &mut Ui, draft: &mut ProjectConfig) {
    section(ui, "Limits");
    ui.horizontal(|ui| {
        let label = ui.label("Claim timeout, minutes");
        ui.add(DragValue::new(&mut draft.limits.claim_timeout_minutes).range(1..=1440))
            .labelled_by(label.id);
    });
    ui.horizontal(|ui| {
        let label = ui.label("Loop limit, material agent changes in one hour");
        ui.add(DragValue::new(&mut draft.limits.loop_limit).range(1..=1000))
            .labelled_by(label.id);
    });
    ui.checkbox(
        &mut draft.limits.agent_delete,
        "Agents can use the delete tools",
    );
}

fn actor_label(actor: Actor) -> &'static str {
    match actor {
        Actor::Agent => "Agent",
        Actor::Human => "Person",
        Actor::Any => "Any",
    }
}

fn types_section(ui: &mut Ui, draft: &mut ProjectConfig, new_type: &mut String) {
    let palette = Palette::of(ui);
    section(ui, "Task types and stages");
    caption(
        ui,
        "lenka refuses to remove a type or a stage that tasks use.",
    );
    let mut remove_type = None;
    for (name, def) in &mut draft.types {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(name).size(14.0).color(palette.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = ui.small_button("Remove type");
                let label = format!("Remove the type {name}");
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::accesskit::Role::Button, true, &label)
                });
                if response.clicked() {
                    remove_type = Some(name.clone());
                }
            });
        });
        stages_table(ui, name, def);
    }
    if let Some(name) = remove_type {
        draft.types.remove(&name);
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        inline_field(ui, "New type", new_type, 140.0, "Name");
        let name = new_type.trim().to_owned();
        let usable = !name.is_empty() && !draft.types.contains_key(&name);
        if ui.add_enabled(usable, Button::new("Add type")).clicked() {
            let stage = Stage {
                name: "work".to_owned(),
                actor: Actor::Any,
                unblocks: false,
            };
            draft.types.insert(
                name,
                TypeDef {
                    stages: vec![stage],
                },
            );
            new_type.clear();
        }
    });
}

fn stages_table(ui: &mut Ui, type_name: &str, def: &mut TypeDef) {
    let mut remove = None;
    let mut swap = None;
    let count = def.stages.len();
    for (index, stage) in def.stages.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let number = ui.label(RichText::new(format!("{}.", index + 1)).small());
            let name = format!("Stage {} name of {type_name}", index + 1);
            let edit = ui
                .add(TextEdit::singleline(&mut stage.name).desired_width(140.0))
                .labelled_by(number.id);
            edit.widget_info(|| {
                egui::WidgetInfo::labeled(egui::accesskit::Role::TextInput, true, &name)
            });
            ComboBox::from_id_salt(("stage_actor", type_name, index))
                .selected_text(actor_label(stage.actor))
                .width(80.0)
                .show_ui(ui, |ui| {
                    for actor in [Actor::Any, Actor::Agent, Actor::Human] {
                        ui.selectable_value(&mut stage.actor, actor, actor_label(actor));
                    }
                })
                .response
                .widget_info(|| combo_info("Actor"));
            ui.checkbox(&mut stage.unblocks, "Unblocks");
            if ui
                .add_enabled(index > 0, Button::new("Up").small())
                .clicked()
            {
                swap = Some((index, index - 1));
            }
            if ui
                .add_enabled(index + 1 < count, Button::new("Down").small())
                .clicked()
            {
                swap = Some((index, index + 1));
            }
            if danger_button_small(ui, count > 1).clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some((a, b)) = swap {
        def.stages.swap(a, b);
    }
    if let Some(index) = remove {
        def.stages.remove(index);
    }
    if ui.small_button("Add stage").clicked() {
        let mut number = def.stages.len() + 1;
        let mut name = format!("stage{number}");
        while def.stages.iter().any(|stage| stage.name == name) {
            number += 1;
            name = format!("stage{number}");
        }
        def.stages.push(Stage {
            name,
            actor: Actor::Any,
            unblocks: false,
        });
    }
}

fn danger_button_small(ui: &mut Ui, enabled: bool) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| danger_button(ui, "Remove"))
        .inner
}

fn plugins_section(ui: &mut Ui, host: &Host) {
    let palette = Palette::of(ui);
    section(ui, "Plugins");
    for plugin in host.plugins() {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(plugin.info.name).color(palette.text));
            let (state, color) = if plugin.enabled {
                ("On", palette.success)
            } else {
                ("Off", palette.text_muted)
            };
            chip(ui, state, color);
            chip(ui, "fully trusted, no sandbox", palette.warning);
            ui.label(
                RichText::new(plugin.info.description)
                    .small()
                    .color(palette.text_muted),
            );
        });
    }
    if host.plugins().is_empty() {
        ui.label(RichText::new("This build has no plugins.").color(palette.text_muted));
    }
}

/// Checks settings against the tasks of the project.
pub(crate) fn check_config(config: &ProjectConfig, rows: &[BoardRow]) -> Result<(), String> {
    config.validate().map_err(|err| err.message().to_owned())?;
    for (name, def) in &config.types {
        if name.trim().is_empty() {
            return Err("A type needs a name.".to_owned());
        }
        if def.stages.iter().any(|stage| stage.name.trim().is_empty()) {
            return Err(format!("The type {name} has a stage with no name."));
        }
    }
    let mut missing_type = None;
    let mut missing_stage = None;
    let mut type_uses = 0;
    let mut stage_uses = 0;
    for row in rows {
        let front = &row.meta.front;
        match config.types.get(&front.kind) {
            None => {
                type_uses += 1;
                missing_type.get_or_insert_with(|| front.kind.clone());
            }
            Some(def) => {
                if !def
                    .stages
                    .iter()
                    .any(|stage| stage.name == row.meta.state.stage)
                {
                    stage_uses += 1;
                    missing_stage
                        .get_or_insert_with(|| (front.kind.clone(), row.meta.state.stage.clone()));
                }
            }
        }
    }
    if let Some(kind) = missing_type {
        return Err(format!(
            "{type_uses} tasks use the type {kind}, and the new settings do not have it. Change those tasks first."
        ));
    }
    if let Some((kind, stage)) = missing_stage {
        return Err(format!(
            "{stage_uses} tasks are on the stage {stage} of the type {kind}, and the new settings do not have it. Move those tasks first."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use lenka_plugin_api::{TaskId, Timestamp};
    use lenka_tasks::{Priority, State, Status, TaskFront, TaskMeta};

    use super::*;

    fn row(kind: &str, stage: &str) -> BoardRow {
        BoardRow {
            meta: TaskMeta {
                front: TaskFront {
                    id: TaskId::generate(),
                    kind: kind.to_owned(),
                    title: "T".to_owned(),
                    parent: None,
                    blocked_by: Vec::new(),
                    relates: Vec::new(),
                    artifacts: Vec::new(),
                    priority: Priority::Normal,
                    labels: Vec::new(),
                    created_by: "t".to_owned(),
                    created_at: Timestamp::from_unix_ms(0),
                    start: None,
                    due: None,
                    coordinated: false,
                },
                state: State {
                    version: 1,
                    status: Status::Todo,
                    stage: stage.to_owned(),
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
        }
    }

    #[test]
    fn a_used_type_or_stage_cannot_go_away() {
        let mut config = ProjectConfig::new("t");
        let rows = vec![row("bug", "work")];
        assert!(check_config(&config, &rows).is_ok());
        config.types.remove("epic");
        assert!(
            check_config(&config, &rows).is_ok(),
            "no task uses this type"
        );
        let mut no_bug = config.clone();
        no_bug.types.remove("bug");
        let message = check_config(&no_bug, &rows).expect_err("a used type");
        assert!(message.contains("use the type bug"), "{message}");
        let mut renamed = config.clone();
        if let Some(def) = renamed.types.get_mut("bug") {
            def.stages[0].name = "fix".to_owned();
        }
        let message = check_config(&renamed, &rows).expect_err("a used stage");
        assert!(message.contains("stage work"), "{message}");
    }

    #[test]
    fn empty_names_and_duplicate_stages_are_errors() {
        let mut config = ProjectConfig::new("t");
        if let Some(def) = config.types.get_mut("bug") {
            def.stages[0].name = " ".to_owned();
        }
        assert!(check_config(&config, &[]).is_err());
        let mut config = ProjectConfig::new("t");
        if let Some(def) = config.types.get_mut("bug") {
            let copy = def.stages[0].clone();
            def.stages.push(copy);
        }
        assert!(check_config(&config, &[]).is_err());
    }

    #[test]
    fn lines_drop_blanks() {
        assert_eq!(
            lines_of(" a \n\n b\n"),
            vec!["a".to_owned(), "b".to_owned()]
        );
    }
}
