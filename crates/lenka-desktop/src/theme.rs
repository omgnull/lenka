// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The look of the app: one accent color, consistent spacing, and clear status colors.

use egui::style::{ScrollStyle, Selection, TextCursorStyle, WidgetVisuals, Widgets};
use egui::{
    Color32, Context, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, Style, TextStyle,
    Theme, Ui, Vec2, Visuals,
};
use lenka_tasks::{Priority, Status};

/// The colors of one theme.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub surface: Color32,
    pub surface_alt: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_soft: Color32,
    pub text_muted: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub danger: Color32,
    pub danger_soft: Color32,
    pub warning: Color32,
    pub success: Color32,
}

const fn rgb(value: u32) -> Color32 {
    Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

const LIGHT: Palette = Palette {
    dark: false,
    bg: rgb(0xf4f5f7),
    surface: rgb(0xffffff),
    surface_alt: rgb(0xeceef2),
    border: rgb(0xdfe2e8),
    border_strong: rgb(0xc3c8d1),
    text: rgb(0x14171c),
    text_soft: rgb(0x2b3038),
    text_muted: rgb(0x5a6372),
    accent: rgb(0x4338ca),
    accent_soft: rgb(0xe6e4fa),
    on_accent: rgb(0xffffff),
    danger: rgb(0xb91c1c),
    danger_soft: rgb(0xfbe6e6),
    warning: rgb(0x92400e),
    success: rgb(0x166534),
};

const DARK: Palette = Palette {
    dark: true,
    bg: rgb(0x121419),
    surface: rgb(0x1b1e24),
    surface_alt: rgb(0x262a32),
    border: rgb(0x2e323b),
    border_strong: rgb(0x454b57),
    text: rgb(0xf0f2f5),
    text_soft: rgb(0xd6d9df),
    text_muted: rgb(0x9aa3b1),
    accent: rgb(0x9d97ff),
    accent_soft: rgb(0x2c2a52),
    on_accent: rgb(0x14131f),
    danger: rgb(0xf87171),
    danger_soft: rgb(0x3b1d20),
    warning: rgb(0xfbbf24),
    success: rgb(0x4ade80),
};

impl Palette {
    /// Returns the palette of the theme of a `Ui`.
    pub(crate) fn of(ui: &Ui) -> Self {
        Self::for_dark(ui.visuals().dark_mode)
    }

    pub(crate) fn for_dark(dark: bool) -> Self {
        if dark { DARK } else { LIGHT }
    }

    /// Returns the background of a board column.
    pub(crate) fn column_fill(&self) -> Color32 {
        if self.dark {
            rgb(0x171a1f)
        } else {
            rgb(0xeaecf0)
        }
    }

    /// Returns the color of a status. The UI also shows the status name.
    pub(crate) fn status(&self, status: Status) -> Color32 {
        match (status, self.dark) {
            (Status::Backlog, false) => rgb(0x64748b),
            (Status::Backlog, true) => rgb(0x94a3b8),
            (Status::Todo, false) => rgb(0x1d4ed8),
            (Status::Todo, true) => rgb(0x60a5fa),
            (Status::InProgress, false) => rgb(0xb45309),
            (Status::InProgress, true) => rgb(0xfbbf24),
            (Status::Paused, false) => rgb(0xa21caf),
            (Status::Paused, true) => rgb(0xe879f9),
            (Status::Done, false) => rgb(0x15803d),
            (Status::Done, true) => rgb(0x4ade80),
            (Status::Cancelled, false) => rgb(0x6b7280),
            (Status::Cancelled, true) => rgb(0x8b93a1),
        }
    }

    /// Returns the color of a priority. The UI also shows the priority name.
    pub(crate) fn priority(&self, priority: Priority) -> Color32 {
        match priority {
            Priority::Low => self.text_muted,
            Priority::Normal => self.text_soft,
            Priority::High => self.warning,
            Priority::Urgent => self.danger,
        }
    }
}

/// Returns the display name of a status.
pub(crate) fn status_name(status: Status) -> &'static str {
    match status {
        Status::Backlog => "Backlog",
        Status::Todo => "Todo",
        Status::InProgress => "In progress",
        Status::Paused => "Paused",
        Status::Done => "Done",
        Status::Cancelled => "Cancelled",
    }
}

/// Returns the display name of a priority.
pub(crate) fn priority_name(priority: Priority) -> &'static str {
    match priority {
        Priority::Low => "Low",
        Priority::Normal => "Normal",
        Priority::High => "High",
        Priority::Urgent => "Urgent",
    }
}

fn widget(
    fill: Color32,
    weak: Color32,
    border: Stroke,
    text: Color32,
    radius: u8,
) -> WidgetVisuals {
    WidgetVisuals {
        bg_fill: fill,
        weak_bg_fill: weak,
        bg_stroke: border,
        corner_radius: CornerRadius::same(radius),
        fg_stroke: Stroke::new(1.0, text),
        expansion: 0.0,
    }
}

fn visuals(p: &Palette) -> Visuals {
    let mut visuals = if p.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let border = Stroke::new(1.0, p.border);
    let strong = Stroke::new(1.0, p.border_strong);
    visuals.widgets = Widgets {
        noninteractive: widget(p.surface, p.surface, border, p.text_soft, 6),
        inactive: widget(p.surface, p.surface_alt, border, p.text_soft, 6),
        hovered: widget(p.surface_alt, p.border, strong, p.text, 6),
        active: widget(
            p.accent_soft,
            p.accent_soft,
            Stroke::new(1.0, p.accent),
            p.text,
            6,
        ),
        open: widget(p.surface_alt, p.surface_alt, strong, p.text, 6),
    };
    visuals.selection = Selection {
        bg_fill: p.accent_soft,
        stroke: Stroke::new(1.0, p.accent),
    };
    visuals.hyperlink_color = p.accent;
    visuals.faint_bg_color = p.surface_alt;
    visuals.extreme_bg_color = p.surface;
    visuals.code_bg_color = p.surface_alt;
    visuals.warn_fg_color = p.warning;
    visuals.error_fg_color = p.danger;
    visuals.window_corner_radius = CornerRadius::same(10);
    visuals.window_fill = p.surface;
    visuals.window_stroke = Stroke::new(1.0, p.border_strong);
    visuals.window_shadow = Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(60),
    };
    visuals.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(45),
    };
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.panel_fill = p.bg;
    visuals.window_highlight_topmost = false;
    visuals.indent_has_left_vline = false;
    visuals.text_cursor = TextCursorStyle {
        stroke: Stroke::new(1.5, p.accent),
        blink: false,
        ..TextCursorStyle::default()
    };
    visuals
}

fn style_of(palette: &Palette) -> Style {
    let mut style = Style {
        visuals: visuals(palette),
        ..Style::default()
    };
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(11.5, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(13.5, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(13.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Heading,
            FontId::new(19.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(12.5, FontFamily::Monospace),
        ),
    ]
    .into();
    let spacing = &mut style.spacing;
    spacing.item_spacing = Vec2::new(8.0, 6.0);
    spacing.button_padding = Vec2::new(10.0, 4.0);
    spacing.interact_size = Vec2::new(36.0, 24.0);
    spacing.window_margin = Margin::same(16);
    spacing.menu_margin = Margin::same(6);
    spacing.indent = 16.0;
    spacing.scroll = ScrollStyle::solid();
    style.animation_time = 0.0;
    style
}

/// Sets the styles of both themes on a context. The system theme selects one of them.
pub(crate) fn install(ctx: &Context) {
    ctx.set_style_of(Theme::Light, style_of(&LIGHT));
    ctx.set_style_of(Theme::Dark, style_of(&DARK));
}
