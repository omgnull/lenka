// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The Gantt chart: all dated tasks as a virtual list of bars on a day timeline.

use egui::accesskit::Role;
use egui::{
    Align2, Color32, CursorIcon, FontId, Id, Painter, Panel, Pos2, Rect, RichText, ScrollArea,
    Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo,
};
use lenka_plugin_api::{Day, TaskId};
use lenka_tasks::TaskPatch;

use crate::actions::{Action, TaskOp};
use crate::chart::{AXIS_HEIGHT, AxisKind, draw_axis, draw_today, zoom_and_pan, zoom_buttons};
use crate::data::BoardModel;
use crate::detail::dates_form_of;
use crate::dialogs::Dialog;
use crate::theme::{Palette, status_name};
use crate::timeline::{BarDrag, DaySpan, GanttLayout, GanttRow, Scale, dragged_span, gantt_layout};
use crate::widgets::clamp_text;

const ROW_HEIGHT: f32 = 30.0;
const TITLE_WIDTH: f32 = 260.0;
const HANDLE_WIDTH: f32 = 7.0;
/// The largest count of arrows in one frame. A larger count is not readable.
const MAX_ARROWS: usize = 1_000;

/// A bar that the person drags.
#[derive(Debug, Clone, Copy)]
struct ActiveDrag {
    position: usize,
    mode: BarDrag,
    /// The drag distance in pixels.
    distance: f32,
}

/// The state of the Gantt chart.
#[derive(Debug, Default)]
pub(crate) struct GanttState {
    layout: GanttLayout,
    token: u64,
    scale: Option<Scale>,
    drag: Option<ActiveDrag>,
    show_undated: bool,
}

fn overall_span(rows: &[GanttRow]) -> DaySpan {
    rows.iter()
        .map(|row| row.span)
        .reduce(DaySpan::union)
        .unwrap_or_else(|| {
            let today = Day::today().number();
            DaySpan {
                start: today - 7,
                end: today + 30,
            }
        })
}

#[expect(clippy::cast_possible_truncation, reason = "a drag covers few days")]
fn days_of(distance: f32, scale: &Scale) -> i32 {
    (distance / scale.px_per_day).round() as i32
}

impl GanttState {
    /// Draws the chart.
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        model: &BoardModel,
        visible: &[usize],
        token: u64,
        selected: Option<TaskId>,
        actions: &mut Vec<Action>,
    ) {
        if self.token != token {
            self.layout = gantt_layout(&model.rows, visible);
            self.token = token;
            self.drag = None;
        }
        let palette = Palette::of(ui);
        if !self.layout.undated.is_empty() {
            self.undated_panel(ui, model, actions);
        }
        let mut zoom = 1.0;
        let mut fit = false;
        ui.horizontal(|ui| {
            (zoom, fit) = zoom_buttons(ui);
            let conflicts = self.layout.rows.iter().filter(|row| row.conflict).count();
            ui.label(
                RichText::new(format!("{} tasks with dates", self.layout.rows.len()))
                    .color(palette.text_muted),
            );
            if conflicts > 0 {
                ui.label(
                    RichText::new(format!("{conflicts} tasks start before a blocker ends"))
                        .color(palette.danger),
                );
            }
        });
        ui.add_space(4.0);
        let area = ui.available_rect_before_wrap().shrink2(Vec2::new(0.0, 1.0));
        ui.painter().rect(
            area,
            8.0,
            palette.surface,
            Stroke::new(1.0, palette.border),
            StrokeKind::Inside,
        );
        let chart_width = (area.width() - TITLE_WIDTH).max(100.0);
        let scale = self
            .scale
            .get_or_insert_with(|| Scale::fit(overall_span(&self.layout.rows), chart_width));
        if fit {
            *scale = Scale::fit(overall_span(&self.layout.rows), chart_width);
        }
        if (zoom - 1.0).abs() > f32::EPSILON {
            scale.zoom_at(chart_width / 2.0, zoom);
        }
        let chart = Rect::from_min_max(
            Pos2::new(area.left() + TITLE_WIDTH, area.top() + AXIS_HEIGHT),
            area.max,
        );
        let axis = Rect::from_min_max(
            Pos2::new(chart.left(), area.top()),
            Pos2::new(chart.right(), chart.top()),
        );
        let background = ui.interact(chart, Id::unique("gantt_chart"), Sense::click_and_drag());
        zoom_and_pan(ui, chart, &background, scale);
        let scale = *scale;
        draw_axis(ui, axis, chart.bottom(), AxisKind::Days, &scale);
        let header = Rect::from_min_max(area.min, Pos2::new(chart.left(), chart.top()));
        ui.painter().text(
            Pos2::new(header.left() + 12.0, header.center().y),
            Align2::LEFT_CENTER,
            "Task",
            FontId::proportional(11.5),
            palette.text_muted,
        );
        ui.painter().hline(
            header.x_range(),
            header.bottom(),
            Stroke::new(1.0, palette.border),
        );

        let list = Rect::from_min_max(Pos2::new(area.left(), chart.top()), area.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list).id_salt("gantt_list"));
        ScrollArea::vertical()
            .id_salt("gantt_rows")
            .auto_shrink([false, false])
            .show_viewport(&mut child, |ui, viewport| {
                self.rows(ui, model, viewport, &scale, selected, actions);
            });
        draw_today(ui, Rect::from_min_max(axis.min, chart.max), &scale);
    }

    fn rows(
        &mut self,
        ui: &mut Ui,
        model: &BoardModel,
        viewport: Rect,
        scale: &Scale,
        selected: Option<TaskId>,
        actions: &mut Vec<Action>,
    ) {
        #[expect(clippy::cast_precision_loss, reason = "row counts are small")]
        let total = self.layout.rows.len() as f32 * ROW_HEIGHT;
        let (_, content) = ui.allocate_space(Vec2::new(ui.available_width(), total));
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the viewport is not negative"
        )]
        let first = (viewport.top() / ROW_HEIGHT).floor().max(0.0) as usize;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the viewport is not negative"
        )]
        let last =
            ((viewport.bottom() / ROW_HEIGHT).ceil().max(0.0) as usize).min(self.layout.rows.len());

        for position in first..last {
            #[expect(clippy::cast_precision_loss, reason = "row counts are small")]
            let top = content.top() + position as f32 * ROW_HEIGHT;
            let rect = Rect::from_min_size(
                Pos2::new(content.left(), top),
                Vec2::new(content.width(), ROW_HEIGHT),
            );
            self.row(ui, model, position, rect, scale, selected, actions);
        }
        self.arrows(ui, content, first, last, scale);
    }

    fn shown_span(&self, position: usize, scale: &Scale) -> DaySpan {
        let row = &self.layout.rows[position];
        match self.drag {
            Some(drag) if drag.position == position => {
                dragged_span(row.span, drag.mode, days_of(drag.distance, scale))
            }
            _ => row.span,
        }
    }

    #[expect(clippy::too_many_arguments, reason = "these values describe one row")]
    fn row(
        &mut self,
        ui: &mut Ui,
        model: &BoardModel,
        position: usize,
        rect: Rect,
        scale: &Scale,
        selected: Option<TaskId>,
        actions: &mut Vec<Action>,
    ) {
        let palette = Palette::of(ui);
        let layout_row = self.layout.rows[position];
        let row = &model.rows[layout_row.index];
        let id = row.meta.id();
        let title_text = row.meta.front.title.as_str();
        let title_rect = Rect::from_min_size(rect.min, Vec2::new(TITLE_WIDTH, ROW_HEIGHT));
        let title_response =
            ui.interact(title_rect, Id::unique(("gantt_title", id)), Sense::click());
        title_response.widget_info(|| {
            WidgetInfo::selected(Role::Button, true, selected == Some(id), title_text)
        });
        if title_response.clicked() {
            actions.push(Action::Select(id));
        }
        let painter = ui.painter();
        if selected == Some(id) {
            painter.rect_filled(title_rect.shrink(1.0), 4.0, palette.accent_soft);
        } else if title_response.hovered() {
            painter.rect_filled(title_rect.shrink(1.0), 4.0, palette.surface_alt);
        }
        painter.hline(
            rect.x_range(),
            rect.bottom(),
            Stroke::new(1.0, palette.border.gamma_multiply(0.6)),
        );
        let status = row.meta.state.status;
        painter.circle_filled(
            Pos2::new(rect.left() + 13.0, rect.center().y),
            3.5,
            palette.status(status),
        );
        let title = clamp_text(
            painter,
            title_text,
            FontId::proportional(12.5),
            palette.text,
            TITLE_WIDTH - 30.0,
            1,
        );
        let at = Pos2::new(rect.left() + 23.0, rect.center().y - title.size().y / 2.0);
        painter.galley(at, title, palette.text);

        let chart_left = rect.left() + TITLE_WIDTH;
        let clip = Rect::from_min_max(Pos2::new(chart_left, rect.top()), rect.max);
        let span = self.shown_span(position, scale);
        let bar = bar_rect(span, rect, scale);
        if !bar.intersects(clip) {
            return;
        }
        let painter = ui.painter_at(clip);
        let color = palette.status(status);
        let dragged = self.drag.is_some_and(|drag| drag.position == position);
        let border = if layout_row.conflict {
            Stroke::new(1.5, palette.danger)
        } else if dragged {
            Stroke::new(1.5, palette.accent)
        } else {
            Stroke::new(1.0, color)
        };
        painter.rect(
            bar,
            4.0,
            color.gamma_multiply(0.3),
            border,
            StrokeKind::Inside,
        );
        if layout_row.conflict {
            let width = paint_conflict_mark(
                &painter,
                Pos2::new(bar.right() + 5.0, bar.center().y),
                &palette,
            );
            let mark = Rect::from_min_size(
                Pos2::new(bar.right() + 5.0, bar.top()),
                Vec2::new(width, bar.height()),
            );
            ui.interact(
                mark.intersect(clip),
                Id::unique(("gantt_conflict", id)),
                Sense::hover(),
            )
            .on_hover_text("This task starts before its blocker ends.");
        }
        if dragged {
            let text = format!(
                "{} to {}",
                Day::from_number(span.start),
                Day::from_number(span.end)
            );
            painter.text(
                Pos2::new(bar.right() + 8.0, bar.center().y),
                Align2::LEFT_CENTER,
                text,
                FontId::proportional(11.0),
                palette.text,
            );
        }
        self.bar_input(
            ui,
            position,
            id,
            title_text,
            bar.intersect(clip),
            layout_row.span,
            row.meta.state.version,
            scale,
            actions,
        );
    }

    #[expect(clippy::too_many_arguments, reason = "these values describe one bar")]
    fn bar_input(
        &mut self,
        ui: &Ui,
        position: usize,
        id: TaskId,
        title: &str,
        bar: Rect,
        span: DaySpan,
        version: u64,
        scale: &Scale,
        actions: &mut Vec<Action>,
    ) {
        let body = ui.interact(bar, Id::unique(("gantt_bar", id)), Sense::click_and_drag());
        let name = format!("Bar of {title}");
        body.widget_info(move || WidgetInfo::labeled(Role::Button, true, &name));
        if body.clicked() {
            actions.push(Action::Select(id));
        }
        let start_rect =
            Rect::from_min_max(bar.min, Pos2::new(bar.left() + HANDLE_WIDTH, bar.bottom()));
        let end_rect =
            Rect::from_min_max(Pos2::new(bar.right() - HANDLE_WIDTH, bar.top()), bar.max);
        let start = ui.interact(start_rect, Id::unique(("gantt_start", id)), Sense::drag());
        let label = format!("Start of {title}");
        start.widget_info(move || WidgetInfo::labeled(Role::Button, true, &label));
        let end = ui.interact(end_rect, Id::unique(("gantt_end", id)), Sense::drag());
        let label = format!("End of {title}");
        end.widget_info(move || WidgetInfo::labeled(Role::Button, true, &label));
        for (response, mode, cursor) in [
            (&body, BarDrag::Move, CursorIcon::Grab),
            (&start, BarDrag::Start, CursorIcon::ResizeHorizontal),
            (&end, BarDrag::End, CursorIcon::ResizeHorizontal),
        ] {
            if response.hovered() {
                ui.ctx().set_cursor_icon(cursor);
            }
            if response.drag_started() {
                self.drag = Some(ActiveDrag {
                    position,
                    mode,
                    distance: 0.0,
                });
            }
            if response.dragged()
                && let Some(drag) = &mut self.drag
                && drag.position == position
            {
                drag.distance += response.drag_delta().x;
            }
            if response.drag_stopped()
                && let Some(drag) = self.drag.take()
            {
                let new = dragged_span(span, drag.mode, days_of(drag.distance, scale));
                if new != span {
                    let patch = TaskPatch {
                        expected_version: version,
                        start: Some(Some(Day::from_number(new.start))),
                        due: Some(Some(Day::from_number(new.end))),
                        ..TaskPatch::default()
                    };
                    actions.push(Action::Task(TaskOp::Update {
                        id,
                        patch: Box::new(patch),
                        from_draft: false,
                    }));
                }
            }
        }
    }

    fn arrows(&self, ui: &Ui, content: Rect, first: usize, last: usize, scale: &Scale) {
        let palette = Palette::of(ui);
        let chart_left = content.left() + TITLE_WIDTH;
        let clip = Rect::from_min_max(
            Pos2::new(chart_left, ui.clip_rect().top()),
            Pos2::new(content.right(), ui.clip_rect().bottom()),
        );
        let painter = ui.painter_at(clip);
        let mut drawn = 0;
        for arrow in &self.layout.arrows {
            let (low, high) = (arrow.from.min(arrow.to), arrow.from.max(arrow.to));
            if high < first || low >= last {
                continue;
            }
            drawn += 1;
            if drawn > MAX_ARROWS {
                break;
            }
            let row_top = |position: usize| {
                #[expect(clippy::cast_precision_loss, reason = "row counts are small")]
                let top = content.top() + position as f32 * ROW_HEIGHT;
                Rect::from_min_size(
                    Pos2::new(content.left(), top),
                    Vec2::new(content.width(), ROW_HEIGHT),
                )
            };
            let from = bar_rect(
                self.shown_span(arrow.from, scale),
                row_top(arrow.from),
                scale,
            );
            let to = bar_rect(self.shown_span(arrow.to, scale), row_top(arrow.to), scale);
            let late = self.layout.rows[arrow.to].conflict;
            let color = if late {
                palette.danger
            } else {
                palette.text_muted
            };
            draw_arrow(&painter, from, to, color);
        }
    }

    fn undated_panel(&mut self, ui: &mut Ui, model: &BoardModel, actions: &mut Vec<Action>) {
        let palette = Palette::of(ui);
        let count = self.layout.undated.len();
        Panel::bottom("gantt_undated")
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                left: 0,
                right: 0,
                top: 8,
                bottom: 0,
            }))
            .show_separator_line(false)
            .resizable(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let text = format!("Tasks with no dates ({count})");
                    let action = if self.show_undated {
                        "Hide tasks with no dates"
                    } else {
                        "Show tasks with no dates"
                    };
                    ui.label(RichText::new(text).color(palette.text));
                    if ui.small_button(action).clicked() {
                        self.show_undated = !self.show_undated;
                    }
                });
                if self.show_undated {
                    egui::Frame::new()
                        .fill(palette.surface)
                        .stroke(Stroke::new(1.0, palette.border))
                        .corner_radius(8)
                        .inner_margin(6)
                        .show(ui, |ui| {
                            ScrollArea::vertical()
                                .id_salt("gantt_undated_rows")
                                .max_height(150.0)
                                .auto_shrink([false, true])
                                .show_rows(ui, 24.0, count, |ui, range| {
                                    for position in range {
                                        let row = &model.rows[self.layout.undated[position]];
                                        ui.horizontal(|ui| {
                                            let status = row.meta.state.status;
                                            ui.label(
                                                RichText::new(status_name(status))
                                                    .small()
                                                    .color(palette.status(status)),
                                            );
                                            ui.label(
                                                RichText::new(&row.meta.front.title)
                                                    .color(palette.text_soft),
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    let response = ui.small_button("Set dates");
                                                    let name = format!(
                                                        "Set dates of {}",
                                                        row.meta.front.title
                                                    );
                                                    response.widget_info(|| {
                                                        WidgetInfo::labeled(
                                                            Role::Button,
                                                            true,
                                                            &name,
                                                        )
                                                    });
                                                    if response.clicked() {
                                                        actions.push(Action::Open(Dialog::Dates(
                                                            dates_form_of(row),
                                                        )));
                                                    }
                                                },
                                            );
                                        });
                                    }
                                });
                        });
                }
            });
    }
}

fn bar_rect(span: DaySpan, row: Rect, scale: &Scale) -> Rect {
    let chart_left = row.left() + TITLE_WIDTH;
    let x0 = chart_left + scale.x_of(f64::from(span.start));
    let x1 = chart_left + scale.x_of(f64::from(span.end + 1));
    Rect::from_min_max(
        Pos2::new(x0, row.top() + 6.0),
        Pos2::new(x1.max(x0 + 3.0), row.bottom() - 6.0),
    )
}

fn paint_conflict_mark(painter: &Painter, left_center: Pos2, palette: &Palette) -> f32 {
    let galley = painter.layout_no_wrap(
        "Too early".to_owned(),
        FontId::proportional(10.5),
        palette.danger,
    );
    let size = galley.size();
    let rect = Rect::from_min_size(
        left_center - Vec2::new(0.0, size.y / 2.0 + 1.0),
        size + Vec2::new(8.0, 2.0),
    );
    painter.rect_filled(rect, 3.0, palette.danger_soft);
    painter.galley(rect.min + Vec2::new(4.0, 1.0), galley, palette.danger);
    rect.width()
}

fn draw_arrow(painter: &Painter, from: Rect, to: Rect, color: Color32) {
    let stroke = Stroke::new(1.0, color);
    let start = Pos2::new(from.right(), from.center().y);
    let end = Pos2::new(to.left(), to.center().y);
    let bend = 7.0;
    let points = if end.x - start.x >= bend * 2.0 {
        let mid = start.x + bend;
        vec![start, Pos2::new(mid, start.y), Pos2::new(mid, end.y), end]
    } else {
        // The target starts too early, so the path goes around the end of the source bar.
        let out = start.x + bend;
        let back = end.x - bend;
        let between = if end.y >= start.y {
            from.bottom() + 3.0
        } else {
            from.top() - 3.0
        };
        vec![
            start,
            Pos2::new(out, start.y),
            Pos2::new(out, between),
            Pos2::new(back, between),
            Pos2::new(back, end.y),
            end,
        ]
    };
    painter.add(egui::Shape::line(points, stroke));
    let head = vec![end, end + Vec2::new(-5.0, -3.0), end + Vec2::new(-5.0, 3.0)];
    painter.add(egui::Shape::convex_polygon(head, color, Stroke::NONE));
}
