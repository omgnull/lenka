// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Small shared widgets.

use egui::accesskit::Role;
use egui::text::LayoutJob;
use egui::{
    Align2, Button, Color32, FontId, Galley, Painter, Pos2, Rect, Response, RichText, Sense,
    Stroke, StrokeKind, TextEdit, TextWrapMode, Ui, Vec2, WidgetInfo, WidgetText,
};
use std::sync::Arc;

use crate::markdown_view::Preview;
use crate::theme::Palette;

/// A button with the accent color, for the main action of a view.
pub(crate) fn primary_button(ui: &mut Ui, text: &str) -> Response {
    let palette = Palette::of(ui);
    ui.add(
        Button::new(RichText::new(text).color(palette.on_accent))
            .fill(palette.accent)
            .stroke(Stroke::NONE),
    )
}

/// A button with the danger color, for an action that removes something.
pub(crate) fn danger_button(ui: &mut Ui, text: &str) -> Response {
    let palette = Palette::of(ui);
    ui.add(
        Button::new(RichText::new(text).color(palette.danger))
            .fill(palette.danger_soft)
            .stroke(Stroke::NONE),
    )
}

/// A small text pill.
pub(crate) fn chip(ui: &mut Ui, text: &str, color: Color32) {
    let palette = Palette::of(ui);
    let font = FontId::proportional(11.5);
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let size = galley.size() + Vec2::new(12.0, 5.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect(
        rect,
        size.y / 2.0,
        palette.surface_alt,
        Stroke::new(1.0, palette.border),
        StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.min + Vec2::new(6.0, 2.5), galley, color);
}

/// Paints a small text pill, and returns its width.
pub(crate) fn paint_chip(
    painter: &Painter,
    pos: Pos2,
    text: &str,
    color: Color32,
    fill: Color32,
) -> f32 {
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::proportional(11.0), color);
    let size = galley.size() + Vec2::new(10.0, 3.0);
    let rect = Rect::from_min_size(pos, size);
    painter.rect_filled(rect, 4.0, fill);
    painter.galley(rect.min + Vec2::new(5.0, 1.5), galley, color);
    size.x
}

/// Lays out text in at most some lines, and ends a longer text with an ellipsis.
pub(crate) fn clamp_text(
    painter: &Painter,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
    rows: usize,
) -> Arc<Galley> {
    let mut job = LayoutJob::simple(text.to_owned(), font, color, width);
    job.wrap.max_rows = rows;
    job.wrap.break_anywhere = false;
    job.wrap.overflow_character = Some('\u{2026}');
    painter.layout_job(job)
}

/// Returns the accessible description of a combo box with this name.
pub(crate) fn combo_info(name: &str) -> WidgetInfo {
    WidgetInfo::labeled(Role::ComboBox, true, name)
}

/// A muted caption above a field or a group.
pub(crate) fn caption(ui: &mut Ui, text: &str) -> Response {
    let palette = Palette::of(ui);
    ui.label(RichText::new(text).small().color(palette.text_muted))
}

/// A section title.
pub(crate) fn section(ui: &mut Ui, text: &str) {
    let palette = Palette::of(ui);
    ui.add_space(6.0);
    ui.label(RichText::new(text).size(14.5).color(palette.text));
    ui.add_space(1.0);
}

/// A one-line text field with a caption above it. The caption is also its accessible name.
pub(crate) fn line_field(ui: &mut Ui, label: &str, text: &mut String, hint: &str) -> Response {
    let caption = caption(ui, label);
    ui.add(
        TextEdit::singleline(text)
            .hint_text(hint.to_owned())
            .desired_width(f32::INFINITY),
    )
    .labelled_by(caption.id)
}

/// A one-line text field with its name on the left side, in a horizontal row.
pub(crate) fn inline_field(
    ui: &mut Ui,
    label: &str,
    text: &mut String,
    width: f32,
    hint: &str,
) -> Response {
    let palette = Palette::of(ui);
    let caption = ui.label(RichText::new(label).color(palette.text_muted));
    ui.add(
        TextEdit::singleline(text)
            .hint_text(hint.to_owned())
            .desired_width(width),
    )
    .labelled_by(caption.id)
}

/// A text area with a caption above it. The caption is also its accessible name.
pub(crate) fn area_field(ui: &mut Ui, label: &str, text: &mut String, rows: usize) -> Response {
    let caption = caption(ui, label);
    ui.add(
        TextEdit::multiline(text)
            .desired_rows(rows)
            .desired_width(f32::INFINITY),
    )
    .labelled_by(caption.id)
}

/// A plain Markdown editor with a preview. The state tells which side shows.
pub(crate) fn markdown_editor(
    ui: &mut Ui,
    label: &str,
    text: &mut String,
    preview: &mut MarkdownState,
    rows: usize,
) {
    let palette = Palette::of(ui);
    let caption_response = ui
        .horizontal(|ui| {
            let caption = ui.label(RichText::new(label).size(13.0).color(palette.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let name = format!("Preview {label}");
                if tab_button(ui, "Preview", preview.showing, &name).clicked() {
                    preview.showing = true;
                }
                let name = format!("Edit {label}");
                if tab_button(ui, "Edit", !preview.showing, &name).clicked() {
                    preview.showing = false;
                }
            });
            caption
        })
        .inner;
    if preview.showing {
        egui::Frame::new()
            .fill(palette.surface)
            .stroke(Stroke::new(1.0, palette.border))
            .corner_radius(6)
            .inner_margin(10)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                preview.preview.show(ui, text);
            });
    } else {
        ui.add(
            TextEdit::multiline(text)
                .desired_rows(rows)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        )
        .labelled_by(caption_response.id);
    }
}

/// The state of one Markdown editor.
#[derive(Debug, Default)]
pub(crate) struct MarkdownState {
    /// Tells if the preview shows, and not the editor.
    pub showing: bool,
    pub preview: Preview,
}

fn tab_button(ui: &mut Ui, text: &str, selected: bool, name: &str) -> Response {
    let palette = Palette::of(ui);
    let color = if selected {
        palette.accent
    } else {
        palette.text_muted
    };
    let fill = if selected {
        palette.accent_soft
    } else {
        Color32::TRANSPARENT
    };
    let response = ui.add(
        Button::new(RichText::new(text).small().color(color))
            .fill(fill)
            .stroke(Stroke::NONE)
            .small(),
    );
    let name = name.to_owned();
    response.widget_info(move || WidgetInfo::selected(Role::Button, true, selected, &name));
    response
}

/// A flat tab with an underline when it is selected.
pub(crate) fn underline_tab(ui: &mut Ui, text: &str, selected: bool) -> Response {
    let palette = Palette::of(ui);
    let color = if selected {
        palette.text
    } else {
        palette.text_muted
    };
    let galley = WidgetText::from(RichText::new(text).color(color)).into_galley(
        ui,
        Some(TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let size = galley.size() + Vec2::new(16.0, 14.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let visible_text = text.to_owned();
    response.widget_info(move || WidgetInfo::selected(Role::Tab, true, selected, &visible_text));
    if response.hovered() && !selected {
        ui.painter()
            .rect_filled(rect.shrink2(Vec2::new(0.0, 3.0)), 5.0, palette.surface_alt);
    }
    ui.painter().galley(
        Align2::CENTER_CENTER
            .align_size_within_rect(galley.size(), rect)
            .min,
        galley,
        color,
    );
    if selected {
        let line = Rect::from_min_max(
            Pos2::new(rect.left() + 6.0, rect.bottom() - 2.0),
            Pos2::new(rect.right() - 6.0, rect.bottom()),
        );
        ui.painter().rect_filled(line, 1.0, palette.accent);
    }
    response
}

/// Formats a size in bytes for people.
pub(crate) fn format_bytes(size: u64) -> String {
    const KIB: u64 = 1024;
    #[expect(
        clippy::cast_precision_loss,
        reason = "the text shows one decimal only"
    )]
    let value = size as f64;
    if size < KIB {
        format!("{size} B")
    } else if size < KIB * KIB {
        format!("{:.1} KiB", value / 1024.0)
    } else if size < KIB * KIB * KIB {
        format!("{:.1} MiB", value / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GiB", value / (1024.0 * 1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::format_bytes;

    #[test]
    fn sizes_use_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(3 * 1024 * 1024), "3.0 MiB");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }
}
