// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The state of the task detail panel: the loaded task, the text drafts, and the save problems.

mod view;

pub(crate) use view::{dates_form_of, stage_place};

use std::collections::HashMap;

use lenka_plugin_api::{Result, TaskId};
use lenka_tasks::markdown::criteria_text_differs;
use lenka_tasks::{EntryKind, RelatedView, TaskDetail, TaskPatch};

use crate::dialogs::FormError;
use crate::session::Session;
use crate::widgets::MarkdownState;

/// All parts that the panel shows.
const PARTS: [&str; 4] = ["plan", "entries", "versions", "artifacts"];

/// The tabs of the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Details,
    Plan,
    Entries,
    Relations,
    Artifacts,
    History,
}

/// The texts that a person edits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Draft {
    pub title: String,
    pub description: String,
    pub requirements: String,
    pub acceptance: String,
    pub plan: String,
}

impl Draft {
    fn of(data: &TaskDetail) -> Self {
        Self {
            title: data.meta.front.title.clone(),
            description: data.description.clone(),
            requirements: data.requirements.clone(),
            acceptance: data.acceptance.clone(),
            plan: data.plan.clone().unwrap_or_default(),
        }
    }
}

/// A problem from the last save. The drafts stay as the person wrote them.
#[derive(Debug, Clone)]
pub(crate) enum Problem {
    /// Another writer saved a newer version.
    Stale(String),
    /// The text matches a secret pattern. A person can save it with an explicit choice.
    Secret {
        message: String,
        patch: Box<TaskPatch>,
        /// The text that the check read. A person accepts only this text.
        checked: Draft,
    },
    /// Another error.
    Error(String),
}

/// How a new load affects the drafts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reload {
    /// Another writer can have changed the task. Edited texts stay, and they stay on the old base.
    External,
    /// The person changed a field in this panel. Edited texts stay on the new base.
    OwnField,
    /// The save of the drafts worked.
    Saved,
    /// The person asked for the new version. Edited texts stay on the new base.
    Rebase,
    /// The person threw away the drafts.
    Discard,
}

/// An old version that the History tab shows.
#[derive(Debug, Clone)]
pub(crate) struct OldVersion {
    pub version: u64,
    pub task: String,
    pub plan: Option<String>,
}

/// The kind of a new link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LinkKind {
    BlockedBy,
    Relates,
    Parent,
}

/// The state of the panel for one task.
#[derive(Debug)]
pub(crate) struct DetailState {
    pub id: TaskId,
    pub data: TaskDetail,
    pub related: Option<RelatedView>,
    /// The full IDs of the short IDs that the panel shows.
    known_ids: HashMap<String, TaskId>,
    pub tab: Tab,
    pub draft: Draft,
    /// The version that the drafts started from. A save sends it as the expected version.
    pub base_version: u64,
    pub problem: Option<Problem>,
    pub old_version: Option<OldVersion>,
    pub entry_kind: EntryKind,
    pub entry_text: String,
    pub entry_error: Option<FormError>,
    label_input: String,
    start_input: String,
    due_input: String,
    field_error: Option<String>,
    link_kind: LinkKind,
    link_search: String,
    artifact_path: String,
    editors: [MarkdownState; 4],
}

impl DetailState {
    /// Reads a task, and creates its panel.
    pub(crate) fn open(session: &Session, id: TaskId) -> Result<Self> {
        let data = session.tasks.detail(id, &PARTS)?;
        let draft = Draft::of(&data);
        let base_version = data.meta.state.version;
        let mut state = Self {
            id,
            related: None,
            known_ids: HashMap::new(),
            tab: Tab::Details,
            draft,
            base_version,
            problem: None,
            old_version: None,
            entry_kind: EntryKind::Comment,
            entry_text: String::new(),
            entry_error: None,
            label_input: String::new(),
            start_input: String::new(),
            due_input: String::new(),
            field_error: None,
            link_kind: LinkKind::BlockedBy,
            link_search: String::new(),
            artifact_path: String::new(),
            editors: Default::default(),
            data,
        };
        state.fill_date_inputs();
        state.resolve_blockers(session);
        Ok(state)
    }

    fn resolve_blockers(&mut self, session: &Session) {
        self.known_ids.clear();
        for blocker in &self.data.blockers {
            if let Ok(id) = session.tasks.resolve(&blocker.id) {
                self.known_ids.insert(blocker.id.clone(), id);
            }
        }
    }

    fn fill_date_inputs(&mut self) {
        let text = |day: Option<lenka_plugin_api::Day>| {
            day.map_or_else(String::new, |day| day.to_string())
        };
        self.start_input = text(self.data.meta.front.start);
        self.due_input = text(self.data.meta.front.due);
    }

    /// Reads the task again, and merges the new data with the drafts.
    pub(crate) fn reload(&mut self, session: &Session, mode: Reload) -> Result<()> {
        let data = session.tasks.detail(self.id, &PARTS)?;
        let old = Draft::of(&self.data);
        let new = Draft::of(&data);
        let old_version = self.data.meta.state.version;
        let new_version = data.meta.state.version;
        match mode {
            Reload::Saved | Reload::Discard => {
                self.draft = new;
                self.base_version = new_version;
                self.problem = None;
            }
            Reload::External | Reload::OwnField | Reload::Rebase => {
                merge_field(&mut self.draft.title, &old.title, &new.title);
                merge_field(
                    &mut self.draft.description,
                    &old.description,
                    &new.description,
                );
                merge_field(
                    &mut self.draft.requirements,
                    &old.requirements,
                    &new.requirements,
                );
                merge_field(&mut self.draft.acceptance, &old.acceptance, &new.acceptance);
                merge_field(&mut self.draft.plan, &old.plan, &new.plan);
                let clean = self.draft == new;
                let rebase = match mode {
                    Reload::OwnField => self.base_version == old_version,
                    Reload::Rebase => true,
                    _ => clean,
                };
                if rebase {
                    self.base_version = new_version;
                }
                if mode == Reload::Rebase {
                    self.problem = None;
                }
            }
        }
        self.data = data;
        self.related = None;
        self.fill_date_inputs();
        self.resolve_blockers(session);
        Ok(())
    }

    /// Tells if a draft differs from the loaded task.
    pub(crate) fn is_dirty(&self) -> bool {
        self.draft != Draft::of(&self.data)
    }

    /// Tells if a close of the panel would lose text of the person.
    pub(crate) fn has_unsaved_text(&self) -> bool {
        self.is_dirty() || !self.entry_text.trim().is_empty()
    }

    /// Tells if the task has a newer version than the base of the drafts.
    pub(crate) fn has_newer_version(&self) -> bool {
        self.data.meta.state.version != self.base_version
    }

    /// Returns the change from the drafts, and the names of the material parts.
    pub(crate) fn draft_patch(&self) -> Option<(Box<TaskPatch>, Vec<&'static str>)> {
        if !self.is_dirty() {
            return None;
        }
        let old = Draft::of(&self.data);
        let draft = &self.draft;
        let mut parts = Vec::new();
        let mut patch = TaskPatch {
            expected_version: self.base_version,
            ..TaskPatch::default()
        };
        if draft.title != old.title {
            patch.title = Some(draft.title.clone());
        }
        if draft.description != old.description {
            patch.description = Some(draft.description.clone());
            if draft.description.trim() != old.description.trim() {
                parts.push("description");
            }
        }
        if draft.requirements != old.requirements {
            patch.requirements = Some(draft.requirements.clone());
            if draft.requirements.trim() != old.requirements.trim() {
                parts.push("requirements");
            }
        }
        if draft.acceptance != old.acceptance {
            patch.acceptance = Some(draft.acceptance.clone());
            if criteria_text_differs(Some(&old.acceptance), Some(&draft.acceptance)) {
                parts.push("acceptance criteria");
            }
        }
        if draft.plan != old.plan {
            patch.plan = Some(draft.plan.clone());
            if draft.plan.trim() != old.plan.trim() {
                parts.push("plan");
            }
        }
        Some((Box::new(patch), parts))
    }
}

/// Tells if a text has a line that the task file reads as the start of a new section.
///
/// The service moves such a line and the text after it out of the field. A code fence protects it.
pub(crate) fn has_section_heading(text: &str) -> bool {
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence && trimmed.starts_with("## ") {
            return true;
        }
    }
    false
}

/// Takes a new text unless the person edited the old text.
fn merge_field(draft: &mut String, old: &str, new: &str) {
    if draft == old {
        new.clone_into(draft);
    }
}

#[cfg(test)]
mod heading_tests {
    use super::has_section_heading;

    #[test]
    fn only_a_second_level_heading_outside_a_fence_counts() {
        assert!(has_section_heading("Text\n## Goal\nMore"));
        assert!(!has_section_heading("### Goal\n# Title\n##No space"));
        assert!(!has_section_heading("```\n## In a fence\n```\n"));
    }
}
