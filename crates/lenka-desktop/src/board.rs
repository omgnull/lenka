// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The board: one virtual list of cards for each status.

use egui::accesskit::Role;
use egui::{
    Align2, Color32, CursorIcon, FontId, Id, LayerId, Order, Painter, Pos2, Rect, Response,
    ScrollArea, Sense, Stroke, StrokeKind, TextStyle, Ui, UiBuilder, Vec2, WidgetInfo,
};
use lenka_plugin_api::TaskId;
use lenka_tasks::{BoardRow, Status};

use crate::actions::Action;
use crate::data::BoardModel;
use crate::theme::{Palette, priority_name, status_name};
use crate::widgets::{clamp_text, paint_chip};

const CARD_HEIGHT: f32 = 112.0;
const COLUMN_GAP: f32 = 10.0;
const MIN_COLUMN_WIDTH: f32 = 215.0;
const COLUMNS: [Status; 6] = Status::ALL;

fn column_of(status: Status) -> usize {
    COLUMNS.iter().position(|item| *item == status).unwrap_or(0)
}

/// A card that the person drags.
#[derive(Debug, Clone, Copy)]
struct CardDrag {
    id: TaskId,
    index: usize,
    from: Status,
    pos: Pos2,
}

/// The state of the board.
#[derive(Debug, Default)]
pub(crate) struct BoardState {
    columns: [Vec<usize>; 6],
    token: u64,
    drag: Option<CardDrag>,
}

impl BoardState {
    fn update_columns(&mut self, model: &BoardModel, visible: &[usize], token: u64) {
        if self.token == token {
            return;
        }
        self.token = token;
        for column in &mut self.columns {
            column.clear();
        }
        for index in visible {
            self.columns[column_of(model.rows[*index].meta.state.status)].push(*index);
        }
    }

    /// Draws the board.
    #[expect(
        clippy::too_many_arguments,
        reason = "these values are the inputs of one view"
    )]
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        model: &BoardModel,
        visible: &[usize],
        token: u64,
        selected: Option<TaskId>,
        show_cancelled: bool,
        actions: &mut Vec<Action>,
    ) {
        self.update_columns(model, visible, token);
        let statuses: &[Status] = if show_cancelled {
            &COLUMNS
        } else {
            &COLUMNS[..5]
        };
        #[expect(clippy::cast_precision_loss, reason = "the count of columns is small")]
        let count = statuses.len() as f32;
        let available = ui.available_size();
        let width = ((available.x - COLUMN_GAP * (count - 1.0)) / count)
            .max(MIN_COLUMN_WIDTH)
            .floor();
        let height = (available.y - 14.0).max(120.0);
        let mut targets = Vec::with_capacity(statuses.len());
        ScrollArea::horizontal()
            .id_salt("board_columns")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = COLUMN_GAP;
                ui.horizontal_top(|ui| {
                    for status in statuses {
                        let rect = self.column(
                            ui,
                            model,
                            *status,
                            Vec2::new(width, height),
                            selected,
                            actions,
                        );
                        targets.push((*status, rect));
                    }
                });
            });
        self.finish_drag(ui, model, &targets, actions);
    }

    fn column(
        &mut self,
        ui: &mut Ui,
        model: &BoardModel,
        status: Status,
        size: Vec2,
        selected: Option<TaskId>,
        actions: &mut Vec<Action>,
    ) -> Rect {
        let palette = Palette::of(ui);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let hovered_by_drag = self
            .drag
            .is_some_and(|drag| drag.from != status && rect.contains(drag.pos));
        let border = if hovered_by_drag {
            Stroke::new(2.0, palette.accent)
        } else {
            Stroke::new(1.0, palette.border)
        };
        let fill = if hovered_by_drag {
            palette.accent_soft
        } else {
            palette.column_fill()
        };
        ui.painter()
            .rect(rect, 8.0, fill, border, StrokeKind::Inside);

        let inner = rect.shrink2(Vec2::new(8.0, 8.0));
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .id_salt(("column", status.as_str()))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let column_index = column_of(status);
        let items = std::mem::take(&mut self.columns[column_index]);

        let header = Rect::from_min_size(inner.min, Vec2::new(inner.width(), 22.0));
        let center = Pos2::new(header.left() + 5.0, header.center().y);
        child
            .painter()
            .circle_filled(center, 4.0, palette.status(status));
        let title = child.painter().text(
            Pos2::new(header.left() + 15.0, header.center().y),
            Align2::LEFT_CENTER,
            status_name(status),
            FontId::proportional(13.5),
            palette.text,
        );
        child.painter().text(
            Pos2::new(title.right() + 7.0, header.center().y),
            Align2::LEFT_CENTER,
            items.len().to_string(),
            FontId::proportional(12.0),
            palette.text_muted,
        );
        let label = format!("{} column, {} tasks", status_name(status), items.len());
        let header_response = child.interact(
            header,
            Id::unique(("column_header", status.as_str())),
            Sense::hover(),
        );
        header_response.widget_info(move || WidgetInfo::labeled(Role::Label, true, &label));
        child.advance_cursor_after_rect(header);
        child.add_space(4.0);

        let spacing_y = child.spacing().item_spacing.y;
        child.spacing_mut().item_spacing.y = spacing_y.max(7.0);
        ScrollArea::vertical()
            .id_salt(("column_cards", status.as_str()))
            .auto_shrink([false, false])
            .show_rows(&mut child, CARD_HEIGHT, items.len(), |ui, range| {
                for position in range {
                    let index = items[position];
                    self.card(ui, model, index, selected, actions);
                }
            });
        self.columns[column_index] = items;
        rect
    }

    fn card(
        &mut self,
        ui: &mut Ui,
        model: &BoardModel,
        index: usize,
        selected: Option<TaskId>,
        actions: &mut Vec<Action>,
    ) {
        let palette = Palette::of(ui);
        let row = &model.rows[index];
        let id = row.meta.id();
        let width = ui.available_width();
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(width, CARD_HEIGHT), Sense::click_and_drag());
        let is_dragged = self.drag.is_some_and(|drag| drag.index == index);
        response.widget_info(|| {
            WidgetInfo::selected(
                Role::Button,
                true,
                selected == Some(id),
                row.meta.front.title.as_str(),
            )
        });
        if ui.is_rect_visible(rect) {
            let dim = if is_dragged { 0.35 } else { 1.0 };
            paint_card(
                ui.painter(),
                rect,
                row,
                selected == Some(id),
                response.hovered(),
                dim,
                &palette,
            );
        }
        handle_card_input(
            &response,
            ui,
            id,
            index,
            row.meta.state.status,
            &mut self.drag,
            actions,
        );
    }

    fn finish_drag(
        &mut self,
        ui: &mut Ui,
        model: &BoardModel,
        targets: &[(Status, Rect)],
        actions: &mut Vec<Action>,
    ) {
        let Some(mut drag) = self.drag else {
            return;
        };
        let (pointer, down, released) = ui.input(|input| {
            (
                input.pointer.latest_pos(),
                input.pointer.primary_down(),
                input.pointer.any_released(),
            )
        });
        if let Some(pointer) = pointer {
            drag.pos = pointer;
            self.drag = Some(drag);
        }
        if released || !down {
            self.drag = None;
            let target = targets.iter().find(|(_, rect)| rect.contains(drag.pos));
            if let Some((status, _)) = target
                && *status != drag.from
            {
                actions.push(Action::MoveTo {
                    id: drag.id,
                    status: *status,
                });
            }
            return;
        }
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        let palette = Palette::of(ui);
        let layer = LayerId::new(Order::Tooltip, Id::unique("dragged_card"));
        let painter = ui.ctx().layer_painter(layer);
        let width = targets
            .first()
            .map_or(220.0, |(_, rect)| rect.width() - 24.0);
        let ghost = Rect::from_min_size(
            drag.pos + Vec2::new(-width / 2.0, -14.0),
            Vec2::new(width, CARD_HEIGHT),
        );
        if let Some(row) = model.rows.get(drag.index) {
            paint_card(&painter, ghost, row, true, false, 0.96, &palette);
        }
    }
}

fn handle_card_input(
    response: &Response,
    ui: &Ui,
    id: TaskId,
    index: usize,
    from: Status,
    drag: &mut Option<CardDrag>,
    actions: &mut Vec<Action>,
) {
    if response.clicked() {
        actions.push(Action::Select(id));
    }
    if response.drag_started() {
        let pos = ui
            .input(|input| input.pointer.latest_pos())
            .unwrap_or(response.rect.center());
        *drag = Some(CardDrag {
            id,
            index,
            from,
            pos,
        });
    }
    if response.hovered() && drag.is_none() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
}

fn scaled(color: Color32, opacity: f32) -> Color32 {
    if opacity >= 1.0 {
        color
    } else {
        color.gamma_multiply(opacity)
    }
}

/// Paints one card. The card shows each signal as text, so that color is never the only signal.
fn paint_card(
    painter: &Painter,
    rect: Rect,
    row: &BoardRow,
    selected: bool,
    hovered: bool,
    opacity: f32,
    palette: &Palette,
) {
    let front = &row.meta.front;
    let border = if selected {
        Stroke::new(1.5, palette.accent)
    } else if hovered {
        Stroke::new(1.0, palette.border_strong)
    } else {
        Stroke::new(1.0, palette.border)
    };
    painter.rect(
        rect,
        7.0,
        scaled(palette.surface, opacity),
        border,
        StrokeKind::Inside,
    );
    if row.blocked {
        let strip = Rect::from_min_max(
            rect.min + Vec2::new(1.0, 7.0),
            Pos2::new(rect.left() + 3.5, rect.bottom() - 7.0),
        );
        painter.rect_filled(strip, 1.5, palette.danger);
    }
    let inner = rect.shrink2(Vec2::new(11.0, 9.0));
    let muted = scaled(palette.text_muted, opacity);

    // The first line: type, short ID, and priority.
    let type_width = paint_chip(
        painter,
        inner.min,
        &front.kind,
        scaled(palette.text_soft, opacity),
        palette.surface_alt,
    );
    painter.text(
        Pos2::new(inner.left() + type_width + 7.0, inner.top() + 8.5),
        Align2::LEFT_CENTER,
        front.id.short(),
        FontId::monospace(11.0),
        muted,
    );
    painter.text(
        Pos2::new(inner.right(), inner.top() + 8.5),
        Align2::RIGHT_CENTER,
        priority_name(front.priority),
        FontId::proportional(11.5),
        scaled(palette.priority(front.priority), opacity),
    );

    // The title, in at most two lines.
    let title = clamp_text(
        painter,
        &front.title,
        FontId::proportional(13.5),
        scaled(palette.text, opacity),
        inner.width(),
        2,
    );
    painter.galley(
        Pos2::new(inner.left(), inner.top() + 22.0),
        title,
        palette.text,
    );

    // The labels and the blocked mark.
    let line_y = inner.top() + 60.0;
    let mut x = inner.left();
    let mut right = inner.right();
    if row.blocked {
        let width = paint_chip(
            painter,
            Pos2::new(right - 53.0, line_y),
            "Blocked",
            scaled(palette.danger, opacity),
            palette.danger_soft,
        );
        right -= width + 4.0;
    }
    if row.invalid {
        let width = paint_chip(
            painter,
            Pos2::new(right - 48.0, line_y),
            "Invalid",
            palette.danger,
            palette.danger_soft,
        );
        right -= width + 4.0;
    }
    for label in &front.labels {
        let estimate = 12.0 + 6.2 * label.chars().count() as f32;
        if x + estimate > right {
            break;
        }
        x += paint_chip(
            painter,
            Pos2::new(x, line_y),
            label,
            palette.accent,
            palette.accent_soft,
        ) + 4.0;
    }

    // The last line: stage, child progress, and the claim holder.
    let mut facts = format!("Stage: {}", row.meta.state.stage);
    if row.child_count > 0 {
        facts.push_str(&format!("   {}/{}", row.closed_children, row.child_count));
    }
    let bottom = Pos2::new(inner.left(), inner.bottom() - 7.0);
    let drawn = painter.text(
        bottom,
        Align2::LEFT_CENTER,
        &facts,
        TextStyle::Small.resolve(&egui::Style::default()),
        muted,
    );
    if let Some(holder) = &row.claimed_by {
        let room = (inner.right() - drawn.right() - 8.0).max(20.0);
        let name = clamp_text(
            painter,
            holder,
            FontId::proportional(11.5),
            scaled(palette.warning, opacity),
            room,
            1,
        );
        let at = Pos2::new(
            inner.right() - name.size().x,
            bottom.y - name.size().y / 2.0,
        );
        painter.galley(at, name, palette.warning);
    }
}
