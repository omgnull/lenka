// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The parts that the roadmap and the Gantt chart share.

use egui::{Align2, FontId, Modifiers, Pos2, Rect, Response, Stroke, Ui, Vec2};
use lenka_plugin_api::Day;

use crate::theme::Palette;
use crate::timeline::{Scale, Tick, day_ticks, month_ticks};

/// The height of the time axis.
pub(crate) const AXIS_HEIGHT: f32 = 34.0;

/// The kind of time axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AxisKind {
    Months,
    Days,
}

#[expect(clippy::cast_possible_truncation, reason = "the chart shows real days")]
fn visible_days(scale: &Scale, width: f32) -> (i32, i32) {
    (
        scale.day_at(0.0).floor() as i32,
        scale.day_at(width).ceil() as i32,
    )
}

fn ticks_for(kind: AxisKind, scale: &Scale, width: f32) -> Vec<Tick> {
    let (from, to) = visible_days(scale, width);
    match kind {
        AxisKind::Months => month_ticks(from, to),
        AxisKind::Days => day_ticks(from, to, scale.px_per_day),
    }
}

/// Draws the time axis on top of a chart, and the vertical grid lines below it.
pub(crate) fn draw_axis(ui: &Ui, axis: Rect, grid_bottom: f32, kind: AxisKind, scale: &Scale) {
    let palette = Palette::of(ui);
    let painter = ui.painter_at(Rect::from_min_max(
        axis.min,
        Pos2::new(axis.right(), grid_bottom),
    ));
    // Keep the frame line of the chart visible on the top and the right.
    let fill = Rect::from_min_max(
        axis.min + Vec2::new(0.0, 1.0),
        axis.max - Vec2::new(1.0, 0.0),
    );
    painter.rect_filled(fill, 0.0, palette.surface);
    let ticks = ticks_for(kind, scale, axis.width());
    let mut last_right = f32::NEG_INFINITY;
    for tick in &ticks {
        let x = axis.left() + scale.x_of(f64::from(tick.day));
        let color = if tick.major {
            palette.border_strong
        } else {
            palette.border
        };
        painter.vline(
            x,
            (axis.top() + 1.0)..=grid_bottom,
            Stroke::new(1.0, color.gamma_multiply(0.7)),
        );
        // A label that starts before the axis would show only its end.
        if x >= axis.left() && x + 4.0 > last_right {
            let text_color = if tick.major {
                palette.text
            } else {
                palette.text_muted
            };
            let rect = painter.text(
                Pos2::new(x + 4.0, axis.bottom() - 8.0),
                Align2::LEFT_CENTER,
                &tick.label,
                FontId::proportional(11.5),
                text_color,
            );
            last_right = rect.right() + 4.0;
        }
    }
    painter.hline(
        axis.x_range(),
        axis.bottom(),
        Stroke::new(1.0, palette.border),
    );
}

/// Draws the marker of today over a chart.
pub(crate) fn draw_today(ui: &Ui, chart: Rect, scale: &Scale) {
    let palette = Palette::of(ui);
    let x = chart.left() + scale.x_of(f64::from(Day::today().number()));
    if x < chart.left() || x > chart.right() {
        return;
    }
    let painter = ui.painter_at(chart);
    painter.vline(x, chart.y_range(), Stroke::new(1.5, palette.accent));
    let galley = painter.layout_no_wrap(
        "Today".to_owned(),
        FontId::proportional(10.5),
        palette.on_accent,
    );
    let rect = Rect::from_min_size(
        Pos2::new(x - 1.0, chart.top() + 2.0),
        galley.size() + Vec2::new(8.0, 3.0),
    );
    painter.rect_filled(rect, 3.0, palette.accent);
    painter.galley(rect.min + Vec2::new(4.0, 1.5), galley, palette.on_accent);
}

/// Applies a pan with a drag or a sideways scroll, and a zoom with Ctrl and the wheel.
pub(crate) fn zoom_and_pan(ui: &Ui, chart: Rect, background: &Response, scale: &mut Scale) {
    if background.dragged() {
        scale.pan(background.drag_delta().x);
    }
    let pointer = ui.input(|input| input.pointer.hover_pos());
    let Some(pointer) = pointer.filter(|pos| chart.contains(*pos)) else {
        return;
    };
    let (zoom, sideways, modifiers) = ui.input(|input| {
        (
            input.zoom_delta(),
            input.smooth_scroll_delta.x,
            input.modifiers,
        )
    });
    if (zoom - 1.0).abs() > f32::EPSILON {
        scale.zoom_at(pointer.x - chart.left(), zoom);
    }
    if sideways.abs() > 0.0 && modifiers == Modifiers::NONE {
        scale.pan(sideways);
    }
}

/// Draws the zoom buttons, and returns the factor or the request to fit.
pub(crate) fn zoom_buttons(ui: &mut Ui) -> (f32, bool) {
    let mut factor = 1.0;
    let mut fit = false;
    if ui.button("Zoom out").clicked() {
        factor = 0.8;
    }
    if ui.button("Zoom in").clicked() {
        factor = 1.25;
    }
    if ui.button("Fit").clicked() {
        fit = true;
    }
    (factor, fit)
}
