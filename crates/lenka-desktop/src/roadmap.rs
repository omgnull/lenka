// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The roadmap: top-level tasks as rows on a month timeline.

use egui::accesskit::Role;
use egui::{
    Align2, FontId, Id, Pos2, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, Ui, Vec2,
    WidgetInfo,
};
use lenka_plugin_api::{Day, TaskId};

use crate::actions::Action;
use crate::chart::{AXIS_HEIGHT, AxisKind, draw_axis, draw_today, zoom_and_pan, zoom_buttons};
use crate::data::BoardModel;
use crate::theme::{Palette, status_name};
use crate::timeline::{DaySpan, Rollup, Scale, roadmap_rows};
use crate::widgets::clamp_text;

const ROW_HEIGHT: f32 = 42.0;
const TITLE_WIDTH: f32 = 280.0;

/// The state of the roadmap.
#[derive(Debug, Default)]
pub(crate) struct RoadmapState {
    rows: Vec<Rollup>,
    generation: u64,
    scale: Option<Scale>,
}

fn overall_span(rows: &[Rollup]) -> DaySpan {
    rows.iter()
        .filter_map(|row| row.span)
        .reduce(DaySpan::union)
        .unwrap_or_else(|| {
            let today = Day::today().number();
            DaySpan {
                start: today - 30,
                end: today + 120,
            }
        })
}

impl RoadmapState {
    /// Draws the roadmap.
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        model: &BoardModel,
        generation: u64,
        selected: Option<TaskId>,
        actions: &mut Vec<Action>,
    ) {
        if self.generation != generation {
            self.rows = roadmap_rows(model);
            self.generation = generation;
        }
        let palette = Palette::of(ui);
        let mut zoom = 1.0;
        let mut fit = false;
        ui.horizontal(|ui| {
            (zoom, fit) = zoom_buttons(ui);
            let dated = self.rows.iter().filter(|row| row.span.is_some()).count();
            ui.label(
                RichText::new(format!(
                    "{} top-level tasks, {dated} with dates",
                    self.rows.len()
                ))
                .color(palette.text_muted),
            );
        });
        ui.add_space(4.0);
        let area = ui.available_rect_before_wrap();
        let frame_rect = area.shrink2(Vec2::new(0.0, 1.0));
        ui.painter().rect(
            frame_rect,
            8.0,
            palette.surface,
            Stroke::new(1.0, palette.border),
            StrokeKind::Inside,
        );
        let chart_width = (frame_rect.width() - TITLE_WIDTH).max(100.0);
        let scale = self
            .scale
            .get_or_insert_with(|| Scale::fit(overall_span(&self.rows), chart_width));
        if fit {
            *scale = Scale::fit(overall_span(&self.rows), chart_width);
        }
        if (zoom - 1.0).abs() > f32::EPSILON {
            scale.zoom_at(chart_width / 2.0, zoom);
        }
        let chart = Rect::from_min_max(
            Pos2::new(
                frame_rect.left() + TITLE_WIDTH,
                frame_rect.top() + AXIS_HEIGHT,
            ),
            frame_rect.max,
        );
        let axis = Rect::from_min_max(
            Pos2::new(chart.left(), frame_rect.top()),
            Pos2::new(chart.right(), chart.top()),
        );
        let background = ui.interact(chart, Id::unique("roadmap_chart"), Sense::click_and_drag());
        zoom_and_pan(ui, chart, &background, scale);
        let scale = *scale;
        draw_axis(ui, axis, chart.bottom(), AxisKind::Months, &scale);
        let header = Rect::from_min_max(frame_rect.min, Pos2::new(chart.left(), chart.top()));
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

        let list = Rect::from_min_max(Pos2::new(frame_rect.left(), chart.top()), frame_rect.max);
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(list)
                .id_salt("roadmap_list"),
        );
        ScrollArea::vertical()
            .id_salt("roadmap_rows")
            .auto_shrink([false, false])
            .show_viewport(&mut child, |ui, viewport| {
                #[expect(clippy::cast_precision_loss, reason = "row counts are small")]
                let total = self.rows.len() as f32 * ROW_HEIGHT;
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
                let last = ((viewport.bottom() / ROW_HEIGHT).ceil().max(0.0) as usize)
                    .min(self.rows.len());
                for position in first..last {
                    #[expect(clippy::cast_precision_loss, reason = "row counts are small")]
                    let top = content.top() + position as f32 * ROW_HEIGHT;
                    let row_rect = Rect::from_min_size(
                        Pos2::new(content.left(), top),
                        Vec2::new(content.width(), ROW_HEIGHT),
                    );
                    draw_row(
                        ui,
                        model,
                        self.rows[position],
                        row_rect,
                        &scale,
                        selected,
                        actions,
                    );
                }
            });
        draw_today(ui, Rect::from_min_max(axis.min, chart.max), &scale);
    }
}

fn draw_row(
    ui: &Ui,
    model: &BoardModel,
    roll: Rollup,
    rect: Rect,
    scale: &Scale,
    selected: Option<TaskId>,
    actions: &mut Vec<Action>,
) {
    let palette = Palette::of(ui);
    let row = &model.rows[roll.index];
    let id = row.meta.id();
    let title_rect = Rect::from_min_size(rect.min, Vec2::new(TITLE_WIDTH, ROW_HEIGHT));
    let response = ui.interact(
        title_rect,
        Id::unique(("roadmap_title", id)),
        Sense::click(),
    );
    let title = row.meta.front.title.as_str();
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected == Some(id), title));
    if response.clicked() {
        actions.push(Action::Select(id));
    }
    let painter = ui.painter();
    if selected == Some(id) {
        painter.rect_filled(title_rect.shrink(2.0), 5.0, palette.accent_soft);
    } else if response.hovered() {
        painter.rect_filled(title_rect.shrink(2.0), 5.0, palette.surface_alt);
    }
    painter.hline(
        rect.x_range(),
        rect.bottom(),
        Stroke::new(1.0, palette.border),
    );
    let name = clamp_text(
        painter,
        title,
        FontId::proportional(13.0),
        palette.text,
        TITLE_WIDTH - 24.0,
        1,
    );
    painter.galley(
        Pos2::new(rect.left() + 12.0, rect.top() + 6.0),
        name,
        palette.text,
    );
    let status = row.meta.state.status;
    let facts = format!(
        "{}  {}  {}/{} closed",
        status_name(status),
        row.meta.front.kind,
        roll.closed,
        roll.total
    );
    painter.text(
        Pos2::new(rect.left() + 12.0, rect.bottom() - 11.0),
        Align2::LEFT_CENTER,
        facts,
        FontId::proportional(11.0),
        palette.text_muted,
    );
    let Some(span) = roll.span else {
        let chart_rect =
            Rect::from_min_max(Pos2::new(rect.left() + TITLE_WIDTH, rect.top()), rect.max);
        ui.painter_at(chart_rect).text(
            Pos2::new(chart_rect.left() + 10.0, rect.center().y),
            Align2::LEFT_CENTER,
            "No dates",
            FontId::proportional(11.5),
            palette.text_muted,
        );
        return;
    };
    let chart_left = rect.left() + TITLE_WIDTH;
    let x0 = chart_left + scale.x_of(f64::from(span.start));
    let x1 = chart_left + scale.x_of(f64::from(span.end + 1));
    let bar = Rect::from_min_max(
        Pos2::new(x0, rect.top() + 11.0),
        Pos2::new(x1.max(x0 + 4.0), rect.bottom() - 11.0),
    );
    let clip = Rect::from_min_max(Pos2::new(chart_left, rect.top()), rect.max);
    let painter = ui.painter_at(clip);
    let color = palette.status(status);
    painter.rect(
        bar,
        5.0,
        color.gamma_multiply(0.22),
        Stroke::new(1.0, color),
        StrokeKind::Inside,
    );
    let done = bar.width() * roll.progress();
    if done > 0.5 {
        let fill = Rect::from_min_size(bar.min, Vec2::new(done, bar.height()));
        painter.rect_filled(fill, 5.0, color.gamma_multiply(0.75));
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the part is from 0 to 1"
    )]
    let percent = (roll.progress() * 100.0).round() as u32;
    painter.text(
        Pos2::new(bar.right() + 6.0, bar.center().y),
        Align2::LEFT_CENTER,
        format!("{percent}%"),
        FontId::proportional(11.0),
        palette.text_soft,
    );
    let bar_response = ui.interact(
        bar.intersect(clip),
        Id::unique(("roadmap_bar", id)),
        Sense::click(),
    );
    let name = format!("Bar of {title}");
    bar_response.widget_info(move || WidgetInfo::labeled(Role::Button, true, &name));
    if bar_response.clicked() {
        actions.push(Action::Select(id));
    }
}
