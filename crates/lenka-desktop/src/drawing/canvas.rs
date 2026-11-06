// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The drawing board UI: tools, canvas, properties, and the text list of shapes.

use egui::accesskit::Role;
use egui::{
    Align2, Button, Color32, CursorIcon, FontId, Key, Modifiers, Painter, Panel, PointerButton,
    Pos2, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2, WidgetInfo,
};
use lenka_plugin_api::{ArtifactId, TaskId};
use lenka_tasks::ArtifactRecord;

use super::model::{
    Bounds, Corner, Document, DrawingError, Editor, Kind, Shape, Snapshot, TEXT_SIZE, format_color,
    parse_color, simplify,
};
use crate::theme::Palette;
use crate::widgets::{caption, primary_button, section};

/// The fixed colors of the tool panel.
const SWATCHES: [(&str, [u8; 3]); 8] = [
    ("slate", [0x1f, 0x29, 0x37]),
    ("gray", [0x6b, 0x72, 0x80]),
    ("red", [0xdc, 0x26, 0x26]),
    ("orange", [0xea, 0x58, 0x0c]),
    ("green", [0x16, 0xa3, 0x4a]),
    ("blue", [0x25, 0x63, 0xeb]),
    ("violet", [0x7c, 0x3a, 0xed]),
    ("white", [0xff, 0xff, 0xff]),
];
const WIDTHS: [f64; 4] = [1.0, 2.0, 4.0, 8.0];
const MIN_ZOOM: f32 = 0.02;
const MAX_ZOOM: f32 = 16.0;
const HANDLE_SIZE: f32 = 9.0;
const LIST_ROW_HEIGHT: f32 = 24.0;
/// The smallest count of points in the outline of one ellipse.
const MIN_ELLIPSE_POINTS: usize = 8;
/// The largest count of points in the outline of one ellipse.
const MAX_ELLIPSE_POINTS: usize = 256;
/// The count of ellipse points that one frame shares between all visible ellipses.
const ELLIPSE_POINTS_IN_FRAME: usize = 200_000;
/// The color of the paper. A drawing has fixed colors, so the paper stays light in a dark theme.
const PAPER: Color32 = Color32::from_rgb(0xf7, 0xf8, 0xfa);
const PAPER_DOT: Color32 = Color32::from_rgb(0xd9, 0xdd, 0xe3);

/// The drawing tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Select,
    Rect,
    Ellipse,
    Line,
    Arrow,
    Freehand,
    Text,
}

impl Tool {
    const ALL: [(Self, &'static str, &'static str, Key); 7] = [
        (Self::Select, "Select", "V", Key::V),
        (Self::Rect, "Rectangle", "R", Key::R),
        (Self::Ellipse, "Ellipse", "E", Key::E),
        (Self::Line, "Line", "L", Key::L),
        (Self::Arrow, "Arrow", "A", Key::A),
        (Self::Freehand, "Freehand", "P", Key::P),
        (Self::Text, "Text", "T", Key::T),
    ];

    fn kind(self) -> Option<Kind> {
        match self {
            Self::Select | Self::Text => None,
            Self::Rect => Some(Kind::Rect),
            Self::Ellipse => Some(Kind::Ellipse),
            Self::Line => Some(Kind::Line),
            Self::Arrow => Some(Kind::Arrow),
            Self::Freehand => Some(Kind::Freehand),
        }
    }
}

/// The active pointer gesture.
#[derive(Debug, Clone)]
enum Gesture {
    None,
    Create {
        kind: Kind,
        points: Vec<[f64; 2]>,
    },
    Move {
        before: Snapshot,
        start: [f64; 2],
    },
    Resize {
        before: Snapshot,
        from: Bounds,
        corner: Corner,
    },
    Marquee {
        start: [f64; 2],
        current: [f64; 2],
    },
}

/// The link between drawing coordinates and the screen.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Camera {
    /// The distance from the top left corner of the canvas to the drawing origin.
    pan: Vec2,
    zoom: f32,
}

impl Camera {
    fn to_screen(self, canvas: Rect, point: [f64; 2]) -> Pos2 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "screen positions fit in f32"
        )]
        let position = Vec2::new(point[0] as f32, point[1] as f32);
        canvas.min + self.pan + position * self.zoom
    }

    fn to_world(self, canvas: Rect, pos: Pos2) -> [f64; 2] {
        let local = (pos - canvas.min - self.pan) / self.zoom;
        [f64::from(local.x), f64::from(local.y)]
    }

    fn zoom_at(&mut self, canvas: Rect, pos: Pos2, factor: f32) {
        let world = self.to_world(canvas, pos);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "screen positions fit in f32"
        )]
        let scaled = Vec2::new(world[0] as f32, world[1] as f32) * self.zoom;
        self.pan = pos - canvas.min - scaled;
    }

    fn fit(canvas: Rect, bounds: Option<Bounds>) -> Self {
        let Some(bounds) = bounds else {
            return Self {
                pan: canvas.size() / 2.0,
                zoom: 1.0,
            };
        };
        let margin = 48.0;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the clamp limits the value"
        )]
        let (width, height) = (bounds.w.max(1.0) as f32, bounds.h.max(1.0) as f32);
        let zoom = ((canvas.width() - margin * 2.0).max(40.0) / width)
            .min((canvas.height() - margin * 2.0).max(40.0) / height)
            .clamp(MIN_ZOOM, 2.0);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "screen positions fit in f32"
        )]
        let center = Vec2::new(
            (bounds.x + bounds.w / 2.0) as f32,
            (bounds.y + bounds.h / 2.0) as f32,
        );
        Self {
            pan: canvas.size() / 2.0 - center * zoom,
            zoom,
        }
    }
}

/// A request from the board to the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrawingEvent {
    Save,
    Close,
    /// Load the version of the other writer, and lose local edits.
    ReloadRemote,
}

/// The open drawing.
#[derive(Debug)]
pub(crate) struct DrawingBoard {
    /// The artifact. A new drawing has none until its first save.
    pub artifact: Option<ArtifactId>,
    /// The task that a new drawing links to.
    pub task: Option<TaskId>,
    pub name: String,
    /// The version that the edits start from.
    pub version: u64,
    pub editor: Editor,
    pub tool: Tool,
    /// The newest version of another writer, while local edits exist.
    pub remote_version: Option<u64>,
    camera: Camera,
    needs_fit: bool,
    stroke: [u8; 3],
    fill: Option<[u8; 3]>,
    width: f64,
    gesture: Gesture,
    canvas_rect: Rect,
    focus_text: bool,
}

/// Returns the file name of a drawing.
pub(crate) fn file_name_of(name: &str) -> String {
    format!("{}.lenka-drawing.json", name.trim())
}

impl DrawingBoard {
    /// Creates an empty drawing that exists only in memory until its first save.
    pub(crate) fn new_unsaved(name: &str, task: TaskId) -> Self {
        Self::with_editor(
            None,
            Some(task),
            name.to_owned(),
            0,
            Editor::new(Document::default()),
        )
    }

    /// Opens a saved drawing.
    pub(crate) fn open(
        record: &ArtifactRecord,
        bytes: &[u8],
        task: Option<TaskId>,
    ) -> Result<Self, DrawingError> {
        let document = Document::from_bytes(bytes)?;
        Ok(Self::with_editor(
            Some(record.id),
            task,
            record.name.clone(),
            record.version,
            Editor::new(document),
        ))
    }

    fn with_editor(
        artifact: Option<ArtifactId>,
        task: Option<TaskId>,
        name: String,
        version: u64,
        editor: Editor,
    ) -> Self {
        Self {
            artifact,
            task,
            name,
            version,
            editor,
            tool: Tool::Select,
            remote_version: None,
            camera: Camera {
                pan: Vec2::ZERO,
                zoom: 1.0,
            },
            needs_fit: true,
            stroke: SWATCHES[0].1,
            fill: None,
            width: 2.0,
            gesture: Gesture::None,
            canvas_rect: Rect::ZERO,
            focus_text: false,
        }
    }

    /// Replaces the content with a new version from another writer.
    pub(crate) fn load_version(
        &mut self,
        record: &ArtifactRecord,
        bytes: &[u8],
    ) -> Result<(), DrawingError> {
        let document = Document::from_bytes(bytes)?;
        self.editor.replace(document);
        self.version = record.version;
        self.remote_version = None;
        self.gesture = Gesture::None;
        Ok(())
    }

    /// Tells if the drawing has edits that are not saved.
    pub(crate) fn is_dirty(&self) -> bool {
        self.editor.is_dirty() || self.artifact.is_none()
    }

    /// Draws the board. `keys` tells if the board can read keyboard shortcuts.
    pub(crate) fn show(&mut self, ui: &mut Ui, keys: bool, events: &mut Vec<DrawingEvent>) {
        if keys {
            self.shortcuts(ui, events);
        }
        self.toolbar(ui, events);
        self.remote_banner(ui, events);
        self.side_panel(ui);
        egui::CentralPanel::default()
            .frame(egui::Frame::new())
            .show(ui, |ui| self.canvas(ui));
        self.commit_text_if_unfocused(ui);
    }

    fn commit_text_if_unfocused(&mut self, ui: &Ui) {
        if !ui.ctx().egui_wants_keyboard_input() {
            self.editor.finish_typing();
        }
    }

    fn shortcuts(&mut self, ui: &Ui, events: &mut Vec<DrawingEvent>) {
        let command = Modifiers::COMMAND;
        let mut fit = false;
        // A gesture holds positions in the shape list, so the list stays the same until its end.
        let idle = matches!(self.gesture, Gesture::None);
        ui.input_mut(|input| {
            if idle {
                if input.consume_key(command | Modifiers::SHIFT, Key::Z)
                    || input.consume_key(command, Key::Y)
                {
                    self.editor.redo();
                }
                if input.consume_key(command, Key::Z) {
                    self.editor.undo();
                }
                if input.consume_key(command, Key::S) {
                    events.push(DrawingEvent::Save);
                }
                if input.consume_key(command, Key::D) {
                    self.editor.duplicate_selected();
                }
                if input.consume_key(command, Key::A) {
                    self.editor.select_all();
                }
                if input.consume_key(Modifiers::NONE, Key::Delete)
                    || input.consume_key(Modifiers::NONE, Key::Backspace)
                {
                    self.editor.delete_selected();
                }
            }
            if input.consume_key(command, Key::Num0) {
                fit = true;
            }
            if input.consume_key(Modifiers::NONE, Key::Escape) {
                self.cancel_gesture();
                self.editor.clear_selection();
                self.tool = Tool::Select;
            }
            for (tool, _, _, key) in Tool::ALL {
                if input.consume_key(Modifiers::NONE, key) {
                    self.tool = tool;
                }
            }
        });
        if fit {
            self.needs_fit = true;
        }
    }

    /// Ends a gesture, and puts the shapes back.
    fn cancel_gesture(&mut self) {
        if let Gesture::Move { before, .. } | Gesture::Resize { before, .. } =
            std::mem::replace(&mut self.gesture, Gesture::None)
        {
            self.editor.restore(&before);
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, events: &mut Vec<DrawingEvent>) {
        let palette = Palette::of(ui);
        Panel::top("drawing_tools")
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                left: 0,
                right: 0,
                top: 0,
                bottom: 8,
            }))
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&self.name).size(15.0).color(palette.text));
                    let state = match (self.artifact, self.editor.is_dirty()) {
                        (None, _) => "Not saved yet".to_owned(),
                        (Some(_), true) => format!("Version {}, unsaved edits", self.version),
                        (Some(_), false) => format!("Version {}", self.version),
                    };
                    let color = if self.is_dirty() {
                        palette.warning
                    } else {
                        palette.text_muted
                    };
                    ui.label(RichText::new(state).small().color(color));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close drawing").clicked() {
                            events.push(DrawingEvent::Close);
                        }
                        if primary_button(ui, "Save drawing").clicked() {
                            events.push(DrawingEvent::Save);
                        }
                    });
                });
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    for (tool, name, key, _) in Tool::ALL {
                        let response = ui
                            .add(Button::selectable(self.tool == tool, name))
                            .on_hover_text(format!("Shortcut: {key}"));
                        if response.clicked() {
                            self.tool = tool;
                        }
                    }
                    ui.add_space(12.0);
                    if ui
                        .add_enabled(self.editor.can_undo(), Button::new("Undo"))
                        .on_hover_text("Ctrl+Z")
                        .clicked()
                    {
                        self.editor.undo();
                    }
                    if ui
                        .add_enabled(self.editor.can_redo(), Button::new("Redo"))
                        .on_hover_text("Ctrl+Shift+Z")
                        .clicked()
                    {
                        self.editor.redo();
                    }
                    let has_selection = !self.editor.selection().is_empty();
                    if ui
                        .add_enabled(has_selection, Button::new("Duplicate"))
                        .on_hover_text("Ctrl+D")
                        .clicked()
                    {
                        self.editor.duplicate_selected();
                    }
                    if ui
                        .add_enabled(has_selection, Button::new("Delete"))
                        .on_hover_text("Delete")
                        .clicked()
                    {
                        self.editor.delete_selected();
                    }
                    ui.add_space(12.0);
                    if ui
                        .button("Fit to content")
                        .on_hover_text("Ctrl+0")
                        .clicked()
                    {
                        self.needs_fit = true;
                    }
                    ui.label(
                        RichText::new(format!("{:.0}%", self.camera.zoom * 100.0))
                            .small()
                            .color(palette.text_muted),
                    );
                });
            });
    }

    fn remote_banner(&mut self, ui: &mut Ui, events: &mut Vec<DrawingEvent>) {
        let Some(version) = self.remote_version else {
            return;
        };
        let palette = Palette::of(ui);
        Panel::top("drawing_remote")
            .frame(
                egui::Frame::new()
                    .fill(palette.danger_soft)
                    .inner_margin(8)
                    .corner_radius(7),
            )
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!(
                            "Another writer saved version {version}. You have local edits."
                        ))
                        .color(palette.danger),
                    );
                    if ui
                        .button("Load the new version and lose my edits")
                        .clicked()
                    {
                        events.push(DrawingEvent::ReloadRemote);
                    }
                    // The next save replaces that version, and the old version stays in the
                    // history of the artifact.
                    if ui.button("Keep my edits").clicked() {
                        self.version = version;
                        self.remote_version = None;
                    }
                });
            });
    }

    fn side_panel(&mut self, ui: &mut Ui) {
        let palette = Palette::of(ui);
        Panel::right("drawing_side")
            .frame(
                egui::Frame::new()
                    .fill(palette.surface)
                    .stroke(Stroke::new(1.0, palette.border))
                    .corner_radius(8)
                    .inner_margin(12),
            )
            .exact_size(304.0)
            .resizable(false)
            .show_separator_line(false)
            .show(ui, |ui| {
                self.properties(ui);
                ui.add_space(6.0);
                self.shape_list(ui);
            });
    }

    fn properties(&mut self, ui: &mut Ui) {
        section(ui, "Style");
        caption(ui, "Stroke color");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(4.0);
            for (name, color) in SWATCHES {
                if swatch(ui, color, self.stroke == color, &format!("Stroke {name}")).clicked() {
                    self.stroke = color;
                    let text = format_color(color);
                    self.editor
                        .change_selected(|shape| text.clone_into(&mut shape.stroke));
                }
            }
        });
        caption(ui, "Fill color");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(4.0);
            let none = ui.add(Button::selectable(self.fill.is_none(), "None").small());
            none.widget_info(|| WidgetInfo::labeled(Role::Button, true, "No fill"));
            if none.clicked() {
                self.fill = None;
                self.editor.change_selected(|shape| shape.fill = None);
            }
            for (name, color) in SWATCHES {
                if swatch(ui, color, self.fill == Some(color), &format!("Fill {name}")).clicked() {
                    self.fill = Some(color);
                    let text = format_color(color);
                    self.editor
                        .change_selected(|shape| shape.fill = Some(text.clone()));
                }
            }
        });
        caption(ui, "Stroke width");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            for width in WIDTHS {
                #[expect(clippy::float_cmp, reason = "the values come from one fixed list")]
                let active = self.width == width;
                let response = ui.add(Button::selectable(active, format!("{width:.0}")));
                let name = format!("Stroke width {width:.0}");
                response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
                if response.clicked() {
                    self.width = width;
                    self.editor.change_selected(|shape| shape.width = width);
                }
            }
        });
        let selected_text = self.selected_text_index();
        if let Some(index) = selected_text {
            caption(ui, "Text");
            let mut text = self.editor.doc().shapes[index]
                .text
                .clone()
                .unwrap_or_default();
            let label = ui.label(RichText::new("Shape text").small());
            let response = ui
                .add(
                    TextEdit::multiline(&mut text)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                )
                .labelled_by(label.id);
            if self.focus_text {
                response.request_focus();
                self.focus_text = false;
            }
            if response.changed() {
                self.editor.set_text_live(index, text);
            }
        }
        if !self.editor.selection().is_empty() {
            ui.horizontal(|ui| {
                if ui.small_button("To front").clicked() {
                    self.editor.bring_to_front();
                }
                if ui.small_button("To back").clicked() {
                    self.editor.send_to_back();
                }
            });
        }
    }

    fn selected_text_index(&self) -> Option<usize> {
        let mut selection = self.editor.selection().iter();
        match (selection.next(), selection.next()) {
            (Some(index), None) => self
                .editor
                .doc()
                .shapes
                .get(*index)
                .filter(|shape| shape.kind == Kind::Text)
                .map(|_| *index),
            _ => None,
        }
    }

    /// The text list of shapes, for a screen reader. It also selects shapes.
    fn shape_list(&mut self, ui: &mut Ui) {
        let count = self.editor.doc().shapes.len();
        section(ui, &format!("Shapes ({count})"));
        caption(ui, "A text list of the drawing, from back to front.");
        let mut chosen = None;
        ScrollArea::vertical()
            .id_salt("drawing_shapes")
            .auto_shrink([false, false])
            .show_rows(ui, LIST_ROW_HEIGHT, count, |ui, range| {
                ui.set_min_width(ui.available_width());
                for index in range {
                    let shape = &self.editor.doc().shapes[index];
                    let selected = self.editor.selection().contains(&index);
                    let text = shape.describe(index + 1);
                    let button = Button::selectable(selected, RichText::new(text.clone()).small())
                        .truncate();
                    let response = ui
                        .add_sized([ui.available_width(), LIST_ROW_HEIGHT], button)
                        .on_hover_text(text);
                    if response.clicked() {
                        chosen = Some(index);
                    }
                }
            });
        if let Some(index) = chosen {
            self.editor.select_only(index);
        }
    }

    fn canvas(&mut self, ui: &mut Ui) {
        let size = ui.available_size_before_wrap();
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        response.widget_info(|| WidgetInfo::labeled(Role::Canvas, true, "Drawing canvas"));
        self.canvas_rect = rect;
        if self.needs_fit && rect.width() > 50.0 {
            self.camera = Camera::fit(rect, self.editor.doc().bounds());
            self.needs_fit = false;
        }
        self.pointer_input(ui, rect, &response);
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 8.0, PAPER);
        self.paint_grid(&painter, rect);
        self.paint_shapes(&painter, rect);
        self.paint_gesture(&painter, rect);
        self.paint_selection(&painter, rect);
        let palette = Palette::of(ui);
        painter.rect_stroke(
            rect,
            8.0,
            Stroke::new(1.0, palette.border),
            StrokeKind::Inside,
        );
        if self.editor.doc().shapes.is_empty() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Select a tool, and drag on the canvas.",
                FontId::proportional(14.0),
                Color32::from_rgb(0x6b, 0x72, 0x80),
            );
        }
    }

    fn paint_grid(&self, painter: &Painter, rect: Rect) {
        let step = 40.0 * self.camera.zoom;
        if step < 14.0 {
            return;
        }
        let offset = rect.min
            + Vec2::new(
                self.camera.pan.x.rem_euclid(step),
                self.camera.pan.y.rem_euclid(step),
            );
        let mut y = offset.y;
        let mut count = 0;
        while y < rect.bottom() && count < 12_000 {
            let mut x = offset.x;
            while x < rect.right() {
                painter.circle_filled(Pos2::new(x, y), 1.0, PAPER_DOT);
                x += step;
                count += 1;
            }
            y += step;
        }
    }

    fn pointer_input(&mut self, ui: &Ui, rect: Rect, response: &egui::Response) {
        let (zoom, scroll, shift, space) = ui.input(|input| {
            (
                input.zoom_delta(),
                input.smooth_scroll_delta,
                input.modifiers.shift,
                input.key_down(Key::Space),
            )
        });
        if response.contains_pointer()
            && let Some(pointer) = response.hover_pos()
        {
            if (zoom - 1.0).abs() > f32::EPSILON {
                self.camera.zoom_at(rect, pointer, zoom);
            } else if scroll != Vec2::ZERO {
                self.camera.pan += scroll;
            }
        }
        if response.dragged_by(PointerButton::Middle)
            || response.dragged_by(PointerButton::Secondary)
            || (space && response.dragged_by(PointerButton::Primary))
        {
            self.camera.pan += response.drag_delta();
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            return;
        }
        if self.tool == Tool::Select {
            if let Some(pointer) = response.hover_pos()
                && self.corner_at(rect, pointer).is_some()
            {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeNwSe);
            }
        } else {
            ui.ctx().set_cursor_icon(if self.tool == Tool::Text {
                CursorIcon::Text
            } else {
                CursorIcon::Crosshair
            });
        }
        if response.clicked() {
            self.click(ui, rect, response, shift);
        }
        if response.drag_started_by(PointerButton::Primary) {
            let origin = ui.input(|input| input.pointer.press_origin());
            if let Some(origin) = origin.or(response.interact_pointer_pos()) {
                self.begin_gesture(rect, origin);
            }
        }
        if response.dragged_by(PointerButton::Primary)
            && let Some(pointer) = response.interact_pointer_pos()
        {
            self.update_gesture(rect, pointer);
        }
        if response.drag_stopped() {
            self.finish_gesture();
        }
    }

    fn tolerance(&self) -> f64 {
        f64::from(4.0 / self.camera.zoom)
    }

    fn click(&mut self, ui: &Ui, rect: Rect, response: &egui::Response, shift: bool) {
        let Some(pointer) = response.interact_pointer_pos() else {
            return;
        };
        let world = self.camera.to_world(rect, pointer);
        match self.tool {
            Tool::Select => match self.editor.doc().hit(world[0], world[1], self.tolerance()) {
                Some(index) if shift => self.editor.toggle_selected(index),
                Some(index) => self.editor.select_only(index),
                None => self.editor.clear_selection(),
            },
            Tool::Text => {
                let mut shape = Shape::new(Kind::Text, &format_color(self.stroke), None, 1.0);
                shape.x = world[0];
                shape.y = world[1];
                shape.text = Some("Text".to_owned());
                shape.fit_text();
                self.editor.add(shape);
                self.tool = Tool::Select;
                self.focus_text = true;
                ui.ctx().request_repaint();
            }
            _ => {}
        }
    }

    fn selection_screen_box(&self, rect: Rect) -> Option<Rect> {
        let bounds = self.editor.selection_bounds()?;
        Some(Rect::from_two_pos(
            self.camera.to_screen(rect, [bounds.x, bounds.y]),
            self.camera
                .to_screen(rect, [bounds.x + bounds.w, bounds.y + bounds.h]),
        ))
    }

    fn corner_at(&self, rect: Rect, pos: Pos2) -> Option<Corner> {
        let bounds = self.editor.selection_bounds()?;
        Corner::ALL.into_iter().find(|corner| {
            let at = self.camera.to_screen(rect, corner.on(bounds));
            Rect::from_center_size(at, Vec2::splat(HANDLE_SIZE + 4.0)).contains(pos)
        })
    }

    fn begin_gesture(&mut self, rect: Rect, origin: Pos2) {
        let world = self.camera.to_world(rect, origin);
        match self.tool {
            Tool::Select => {
                if let Some(corner) = self.corner_at(rect, origin)
                    && let Some(from) = self.editor.selection_bounds()
                {
                    self.gesture = Gesture::Resize {
                        before: self.editor.snapshot(),
                        from,
                        corner,
                    };
                    return;
                }
                let hit = self.editor.doc().hit(world[0], world[1], self.tolerance());
                match hit {
                    Some(index) => {
                        if !self.editor.selection().contains(&index) {
                            self.editor.select_only(index);
                        }
                        self.gesture = Gesture::Move {
                            before: self.editor.snapshot(),
                            start: world,
                        };
                    }
                    None => {
                        self.editor.clear_selection();
                        self.gesture = Gesture::Marquee {
                            start: world,
                            current: world,
                        };
                    }
                }
            }
            Tool::Text => {}
            tool => {
                if let Some(kind) = tool.kind() {
                    self.gesture = Gesture::Create {
                        kind,
                        points: vec![world],
                    };
                }
            }
        }
    }

    fn update_gesture(&mut self, rect: Rect, pointer: Pos2) {
        let world = self.camera.to_world(rect, pointer);
        match &mut self.gesture {
            Gesture::None => {}
            Gesture::Create { kind, points } => {
                if *kind == Kind::Freehand {
                    if points.last() != Some(&world) {
                        points.push(world);
                    }
                } else {
                    points.truncate(1);
                    points.push(world);
                }
            }
            Gesture::Move { before, start } => {
                let (dx, dy) = (world[0] - start[0], world[1] - start[1]);
                let before = std::mem::take(before);
                self.editor.drag_move(&before, dx, dy);
                if let Gesture::Move { before: slot, .. } = &mut self.gesture {
                    *slot = before;
                }
            }
            Gesture::Resize {
                before,
                from,
                corner,
            } => {
                let to = corner.resize(*from, world);
                let (from, before) = (*from, std::mem::take(before));
                self.editor.drag_resize(&before, from, to);
                if let Gesture::Resize { before: slot, .. } = &mut self.gesture {
                    *slot = before;
                }
            }
            Gesture::Marquee { current, .. } => *current = world,
        }
    }

    fn shape_of(&self, kind: Kind, points: &[[f64; 2]]) -> Option<Shape> {
        let (first, last) = (*points.first()?, *points.last()?);
        let stroke = format_color(self.stroke);
        let fill = self.fill.map(format_color);
        let mut shape = Shape::new(kind, &stroke, fill.as_deref(), self.width);
        if kind.has_points() {
            let simple = if kind == Kind::Freehand {
                simplify(points, 0.6 / f64::from(self.camera.zoom))
            } else {
                vec![first, last]
            };
            if simple.len() < 2 {
                return None;
            }
            shape.points = Some(simple);
            shape.fit_points();
        } else {
            let bounds = Bounds::of_points(first, last);
            shape.x = bounds.x;
            shape.y = bounds.y;
            shape.w = bounds.w;
            shape.h = bounds.h;
        }
        Some(shape)
    }

    fn finish_gesture(&mut self) {
        match std::mem::replace(&mut self.gesture, Gesture::None) {
            Gesture::None => {}
            Gesture::Create { kind, points } => {
                if let Some(shape) = self.shape_of(kind, &points)
                    && shape.w + shape.h >= 2.0
                {
                    self.editor.add(shape);
                    if kind != Kind::Freehand {
                        self.tool = Tool::Select;
                    }
                }
            }
            Gesture::Move { before, .. } | Gesture::Resize { before, .. } => {
                self.editor.commit(before)
            }
            Gesture::Marquee { start, current } => {
                self.editor.select_in(Bounds::of_points(start, current))
            }
        }
    }

    fn paint_gesture(&self, painter: &Painter, rect: Rect) {
        match &self.gesture {
            Gesture::Create { kind, points } => {
                if let Some(shape) = self.shape_of(*kind, points) {
                    paint_shape(painter, rect, self.camera, &shape, MAX_ELLIPSE_POINTS);
                }
            }
            Gesture::Marquee { start, current } => {
                let area = Rect::from_two_pos(
                    self.camera.to_screen(rect, *start),
                    self.camera.to_screen(rect, *current),
                );
                painter.rect(
                    area,
                    0.0,
                    Color32::from_rgba_unmultiplied(0x43, 0x38, 0xca, 24),
                    Stroke::new(1.0, Color32::from_rgb(0x43, 0x38, 0xca)),
                    StrokeKind::Middle,
                );
            }
            _ => {}
        }
    }

    fn paint_shapes(&self, painter: &Painter, rect: Rect) {
        let view_min = self.camera.to_world(rect, rect.min);
        let view_max = self.camera.to_world(rect, rect.max);
        let view = Bounds::of_points(view_min, view_max);
        let visible: Vec<&Shape> = self
            .editor
            .doc()
            .shapes
            .iter()
            .filter(|shape| shape.bounds().expand(shape.width + 2.0).intersects(view))
            .collect();
        let ellipses = visible
            .iter()
            .filter(|shape| shape.kind == Kind::Ellipse)
            .count();
        let ellipse_points = (ELLIPSE_POINTS_IN_FRAME / ellipses.max(1))
            .clamp(MIN_ELLIPSE_POINTS, MAX_ELLIPSE_POINTS);
        for shape in visible {
            paint_shape(painter, rect, self.camera, shape, ellipse_points);
        }
    }

    fn paint_selection(&self, painter: &Painter, rect: Rect) {
        let Some(area) = self.selection_screen_box(rect) else {
            return;
        };
        let color = Color32::from_rgb(0x43, 0x38, 0xca);
        painter.rect_stroke(
            area.expand(2.0),
            0.0,
            Stroke::new(1.0, color),
            StrokeKind::Middle,
        );
        if let Some(bounds) = self.editor.selection_bounds() {
            for corner in Corner::ALL {
                let center = self.camera.to_screen(rect, corner.on(bounds));
                let handle = Rect::from_center_size(center, Vec2::splat(HANDLE_SIZE));
                painter.rect(
                    handle,
                    2.0,
                    Color32::WHITE,
                    Stroke::new(1.5, color),
                    StrokeKind::Inside,
                );
            }
        }
    }
}

fn color_of(text: &str) -> Color32 {
    parse_color(text).map_or(Color32::BLACK, |[r, g, b]| Color32::from_rgb(r, g, b))
}

/// Returns the outline of an ellipse, with at most `limit` points.
///
/// The ellipse shape of `egui` adds points with the radius, so one large ellipse can cost millions
/// of vertices on each frame.
fn ellipse_outline(center: Pos2, radius: Vec2, limit: usize) -> Vec<Pos2> {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the clamp keeps the value small and positive"
    )]
    let wanted = radius.max_elem().clamp(0.0, 4096.0) as usize;
    let count = (MIN_ELLIPSE_POINTS + wanted).min(limit.max(MIN_ELLIPSE_POINTS));
    (0..count)
        .map(|index| {
            #[expect(clippy::cast_precision_loss, reason = "the count of points is small")]
            let angle = std::f32::consts::TAU * index as f32 / count as f32;
            center + Vec2::new(radius.x * angle.cos(), radius.y * angle.sin())
        })
        .collect()
}

/// Paints one shape.
fn paint_shape(
    painter: &Painter,
    canvas: Rect,
    camera: Camera,
    shape: &Shape,
    ellipse_points: usize,
) {
    #[expect(clippy::cast_possible_truncation, reason = "screen sizes fit in f32")]
    let width = (shape.width as f32 * camera.zoom).max(0.5);
    let stroke = Stroke::new(width, color_of(&shape.stroke));
    let fill = shape.fill.as_deref().map(color_of);
    let top_left = camera.to_screen(canvas, [shape.x, shape.y]);
    let bottom_right = camera.to_screen(canvas, [shape.x + shape.w, shape.y + shape.h]);
    let area = Rect::from_min_max(top_left, bottom_right);
    match shape.kind {
        Kind::Rect => {
            if let Some(fill) = fill {
                painter.rect_filled(area, 0.0, fill);
            }
            painter.rect_stroke(area, 0.0, stroke, StrokeKind::Middle);
        }
        Kind::Ellipse => {
            let outline = ellipse_outline(area.center(), area.size() / 2.0, ellipse_points);
            if let Some(fill) = fill {
                painter.add(egui::Shape::convex_polygon(
                    outline.clone(),
                    fill,
                    Stroke::NONE,
                ));
            }
            painter.add(egui::Shape::closed_line(outline, stroke));
        }
        Kind::Line | Kind::Arrow | Kind::Freehand => {
            let points: Vec<Pos2> = shape
                .points
                .iter()
                .flatten()
                .map(|point| camera.to_screen(canvas, *point))
                .collect();
            if shape.kind == Kind::Arrow
                && let [.., from, to] = points.as_slice()
            {
                paint_arrow_head(painter, *from, *to, stroke, width);
            }
            painter.add(egui::Shape::line(points, stroke));
        }
        Kind::Text => {
            #[expect(clippy::cast_possible_truncation, reason = "screen sizes fit in f32")]
            let size = (TEXT_SIZE as f32 * camera.zoom).clamp(1.0, 300.0);
            painter.text(
                top_left,
                Align2::LEFT_TOP,
                shape.text.as_deref().unwrap_or(""),
                FontId::proportional(size),
                stroke.color,
            );
        }
    }
}

fn paint_arrow_head(painter: &Painter, from: Pos2, to: Pos2, stroke: Stroke, width: f32) {
    let direction = (to - from).normalized();
    let size = (width * 4.0).max(8.0);
    let side = direction.rot90() * size * 0.5;
    let back = to - direction * size;
    painter.line_segment([back + side, to], stroke);
    painter.line_segment([back - side, to], stroke);
}

fn swatch(ui: &mut Ui, color: [u8; 3], selected: bool, name: &str) -> egui::Response {
    let palette = Palette::of(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
    let [r, g, b] = color;
    let border = if selected {
        Stroke::new(2.0, palette.accent)
    } else {
        Stroke::new(1.0, palette.border_strong)
    };
    ui.painter().rect(
        rect.shrink(1.0),
        5.0,
        Color32::from_rgb(r, g, b),
        border,
        StrokeKind::Middle,
    );
    let name = name.to_owned();
    response.widget_info(move || WidgetInfo::selected(Role::Button, true, selected, &name));
    response.on_hover_text(format_color(color))
}
