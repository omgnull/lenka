// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The task hierarchy with collapse, as a virtual list.

use std::collections::HashSet;

use egui::accesskit::Role;
use egui::{Align2, FontId, Id, Pos2, Rect, ScrollArea, Sense, Stroke, Ui, Vec2, WidgetInfo};
use lenka_plugin_api::TaskId;

use crate::actions::Action;
use crate::data::{BoardModel, TreeRow, flatten_tree};
use crate::theme::{Palette, priority_name, status_name};
use crate::widgets::{clamp_text, paint_chip};

const ROW_HEIGHT: f32 = 32.0;

/// The state of the tree.
#[derive(Debug, Default)]
pub(crate) struct TreeState {
    rows: Vec<TreeRow>,
    token: u64,
    collapsed: HashSet<TaskId>,
    dirty: bool,
}

impl TreeState {
    /// Draws the tree.
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        model: &BoardModel,
        visible: &[usize],
        token: u64,
        selected: Option<TaskId>,
        actions: &mut Vec<Action>,
    ) {
        if self.token != token || self.dirty {
            self.rows = flatten_tree(model, visible, &self.collapsed);
            self.token = token;
            self.dirty = false;
        }
        let palette = Palette::of(ui);
        egui::Frame::new()
            .fill(palette.surface)
            .stroke(Stroke::new(1.0, palette.border))
            .corner_radius(8)
            .inner_margin(egui::Margin::symmetric(4, 6))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ScrollArea::vertical()
                    .id_salt("tree_rows")
                    .auto_shrink([false, false])
                    .show_rows(ui, ROW_HEIGHT, self.rows.len(), |ui, range| {
                        for position in range {
                            let row = self.rows[position];
                            self.row(ui, model, row, selected, actions);
                        }
                    });
            });
    }

    fn row(
        &mut self,
        ui: &mut Ui,
        model: &BoardModel,
        tree_row: TreeRow,
        selected: Option<TaskId>,
        actions: &mut Vec<Action>,
    ) {
        let palette = Palette::of(ui);
        let row = &model.rows[tree_row.index];
        let front = &row.meta.front;
        let id = front.id;
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_HEIGHT), Sense::click());
        response.widget_info(|| {
            WidgetInfo::selected(
                Role::Button,
                true,
                selected == Some(id),
                front.title.as_str(),
            )
        });
        if response.clicked() {
            actions.push(Action::Select(id));
        }
        let painter = ui.painter();
        if selected == Some(id) {
            painter.rect_filled(rect.shrink(1.0), 5.0, palette.accent_soft);
        } else if response.hovered() {
            painter.rect_filled(rect.shrink(1.0), 5.0, palette.surface_alt);
        }
        #[expect(clippy::cast_precision_loss, reason = "tree depth is small")]
        let indent = rect.left() + 6.0 + tree_row.depth.min(40) as f32 * 18.0;
        let center_y = rect.center().y;
        if tree_row.has_children {
            let open = !self.collapsed.contains(&id);
            let toggle =
                Rect::from_center_size(Pos2::new(indent + 8.0, center_y), Vec2::splat(20.0));
            let toggle_response =
                ui.interact(toggle, Id::unique(("tree_toggle", id)), Sense::click());
            let name = format!(
                "{} {}",
                if open { "Collapse" } else { "Expand" },
                front.title
            );
            toggle_response.widget_info(move || WidgetInfo::labeled(Role::Button, true, &name));
            if toggle_response.clicked() {
                if open {
                    self.collapsed.insert(id);
                } else {
                    self.collapsed.remove(&id);
                }
                self.dirty = true;
            }
            let color = if toggle_response.hovered() {
                palette.text
            } else {
                palette.text_muted
            };
            let center = toggle.center();
            let points = if open {
                vec![
                    center + Vec2::new(-4.0, -2.5),
                    center + Vec2::new(4.0, -2.5),
                    center + Vec2::new(0.0, 3.5),
                ]
            } else {
                vec![
                    center + Vec2::new(-2.5, -4.0),
                    center + Vec2::new(3.5, 0.0),
                    center + Vec2::new(-2.5, 4.0),
                ]
            };
            painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
        }
        let mut x = indent + 22.0;
        let status = row.meta.state.status;
        painter.circle_filled(Pos2::new(x + 3.0, center_y), 3.5, palette.status(status));
        x += 12.0;
        let status_text = painter.text(
            Pos2::new(x, center_y),
            Align2::LEFT_CENTER,
            status_name(status),
            FontId::proportional(11.5),
            palette.status(status),
        );
        x = status_text.right().max(x + 62.0) + 8.0;
        let type_width = paint_chip(
            painter,
            Pos2::new(x, center_y - 8.5),
            &front.kind,
            palette.text_soft,
            palette.surface_alt,
        );
        x += type_width + 8.0;

        // The right part of the row: facts in a fixed order.
        let mut right = rect.right() - 10.0;
        let short = painter.text(
            Pos2::new(right, center_y),
            Align2::RIGHT_CENTER,
            front.id.short(),
            FontId::monospace(11.0),
            palette.text_muted,
        );
        right = short.left() - 12.0;
        let priority = painter.text(
            Pos2::new(right, center_y),
            Align2::RIGHT_CENTER,
            priority_name(front.priority),
            FontId::proportional(11.5),
            palette.priority(front.priority),
        );
        right = priority.left() - 12.0;
        if row.child_count > 0 {
            let progress = painter.text(
                Pos2::new(right, center_y),
                Align2::RIGHT_CENTER,
                format!("{}/{}", row.closed_children, row.child_count),
                FontId::proportional(11.5),
                palette.text_muted,
            );
            right = progress.left() - 12.0;
        }
        if row.blocked {
            let blocked = painter.text(
                Pos2::new(right, center_y),
                Align2::RIGHT_CENTER,
                "Blocked",
                FontId::proportional(11.5),
                palette.danger,
            );
            right = blocked.left() - 12.0;
        }
        let color = if tree_row.matched {
            palette.text
        } else {
            palette.text_muted
        };
        let title = clamp_text(
            painter,
            &front.title,
            FontId::proportional(13.5),
            color,
            (right - x).max(30.0),
            1,
        );
        let at = Pos2::new(x, center_y - title.size().y / 2.0);
        painter.galley(at, title, color);
    }
}
