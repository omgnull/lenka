// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The drawing format and the editor state. This module has no UI, so tests can reach all of it.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize, Serializer};

/// The only format that this version reads and writes.
pub(crate) const FORMAT: u32 = 1;
/// The largest count of shapes in one drawing.
pub(crate) const MAX_SHAPES: usize = 100_000;
/// The largest count of points in one drawing.
const MAX_POINTS: usize = 1_000_000;
/// The largest count of bytes of a drawing file.
pub(crate) const MAX_BYTES: usize = 64 * 1024 * 1024;
/// The largest count of bytes of the text of one shape.
const MAX_TEXT_BYTES: usize = 64 * 1024;
/// The largest absolute coordinate.
const MAX_COORDINATE: f64 = 10_000_000.0;
/// The largest stroke width.
const MAX_WIDTH: f64 = 500.0;
/// The largest count of undo steps.
const MAX_UNDO: usize = 200;
/// The font size of a text shape, in drawing units.
pub(crate) const TEXT_SIZE: f64 = 18.0;

/// The kind of a shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    Rect,
    Ellipse,
    Line,
    Arrow,
    Freehand,
    Text,
}

impl Kind {
    /// Tells if the shape stores its form in `points`.
    pub(crate) fn has_points(self) -> bool {
        matches!(self, Self::Line | Self::Arrow | Self::Freehand)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Rect => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Line => "Line",
            Self::Arrow => "Arrow",
            Self::Freehand => "Freehand line",
            Self::Text => "Text",
        }
    }
}

fn number<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    let rounded = (value * 100.0).round() / 100.0;
    if rounded.fract() == 0.0 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "validation limits the coordinates"
        )]
        serializer.serialize_i64(rounded as i64)
    } else {
        serializer.serialize_f64(rounded)
    }
}

fn point_list<S: Serializer>(
    value: &Option<Vec<[f64; 2]>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    #[derive(Serialize)]
    struct Item(
        #[serde(serialize_with = "number")] f64,
        #[serde(serialize_with = "number")] f64,
    );
    match value {
        Some(points) => serializer.collect_seq(points.iter().map(|point| Item(point[0], point[1]))),
        None => serializer.serialize_none(),
    }
}

/// One shape. The fields are in alphabetical order, so the JSON keys are sorted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Shape {
    /// The fill color, as `#rrggbb`.
    #[serde(default)]
    pub fill: Option<String>,
    /// The height of the bounding box.
    #[serde(serialize_with = "number")]
    pub h: f64,
    /// The ID, which is unique in the drawing.
    pub id: String,
    /// The kind.
    pub kind: Kind,
    /// The points of a line, an arrow, or a freehand line, in drawing coordinates.
    #[serde(default, serialize_with = "point_list")]
    pub points: Option<Vec<[f64; 2]>>,
    /// The stroke color, as `#rrggbb`.
    pub stroke: String,
    /// The text of a text shape.
    #[serde(default)]
    pub text: Option<String>,
    /// The width of the bounding box.
    #[serde(serialize_with = "number")]
    pub w: f64,
    /// The stroke width.
    #[serde(serialize_with = "number")]
    pub width: f64,
    /// The left side of the bounding box.
    #[serde(serialize_with = "number")]
    pub x: f64,
    /// The top side of the bounding box.
    #[serde(serialize_with = "number")]
    pub y: f64,
}

/// A rectangle in drawing coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Bounds {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Bounds {
    pub(crate) fn of_points(a: [f64; 2], b: [f64; 2]) -> Self {
        Self {
            x: a[0].min(b[0]),
            y: a[1].min(b[1]),
            w: (a[0] - b[0]).abs(),
            h: (a[1] - b[1]).abs(),
        }
    }

    pub(crate) fn union(self, other: Self) -> Self {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            w: (self.x + self.w).max(other.x + other.w) - x,
            h: (self.y + self.h).max(other.y + other.h) - y,
        }
    }

    pub(crate) fn intersects(self, other: Self) -> bool {
        self.x <= other.x + other.w
            && other.x <= self.x + self.w
            && self.y <= other.y + other.h
            && other.y <= self.y + self.h
    }

    pub(crate) fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }

    /// Grows the box on each side.
    pub(crate) fn expand(self, by: f64) -> Self {
        Self {
            x: self.x - by,
            y: self.y - by,
            w: self.w + by * 2.0,
            h: self.h + by * 2.0,
        }
    }
}

fn distance_to_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let length = dx * dx + dy * dy;
    let t = if length == 0.0 {
        0.0
    } else {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / length).clamp(0.0, 1.0)
    };
    let (qx, qy) = (a[0] + t * dx, a[1] + t * dy);
    ((p[0] - qx).powi(2) + (p[1] - qy).powi(2)).sqrt()
}

impl Shape {
    /// Creates a shape with a new ID.
    pub(crate) fn new(kind: Kind, stroke: &str, fill: Option<&str>, width: f64) -> Self {
        Self {
            fill: fill.map(str::to_owned),
            h: 0.0,
            id: new_id(),
            kind,
            points: None,
            stroke: stroke.to_owned(),
            text: None,
            w: 0.0,
            width,
            x: 0.0,
            y: 0.0,
        }
    }

    /// Returns the bounding box.
    pub(crate) fn bounds(&self) -> Bounds {
        Bounds {
            x: self.x,
            y: self.y,
            w: self.w,
            h: self.h,
        }
    }

    /// Sets the bounding box from the points.
    pub(crate) fn fit_points(&mut self) {
        let Some(points) = &self.points else {
            return;
        };
        let Some(first) = points.first() else {
            return;
        };
        let (mut low, mut high) = (*first, *first);
        for point in points {
            low = [low[0].min(point[0]), low[1].min(point[1])];
            high = [high[0].max(point[0]), high[1].max(point[1])];
        }
        self.x = low[0];
        self.y = low[1];
        self.w = high[0] - low[0];
        self.h = high[1] - low[1];
    }

    /// Sets the bounding box of a text shape from an estimate of the text size.
    pub(crate) fn fit_text(&mut self) {
        if self.kind != Kind::Text {
            return;
        }
        let text = self.text.as_deref().unwrap_or("");
        let widest = text
            .lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
        #[expect(clippy::cast_precision_loss, reason = "text sizes are small")]
        {
            self.w = (widest as f64 * TEXT_SIZE * 0.55).max(TEXT_SIZE);
            self.h = (text.lines().count().max(1) as f64) * TEXT_SIZE * 1.3;
        }
    }

    fn translate(&mut self, dx: f64, dy: f64) {
        self.x += dx;
        self.y += dy;
        if let Some(points) = &mut self.points {
            for point in points {
                point[0] += dx;
                point[1] += dy;
            }
        }
    }

    /// Moves and scales the shape from one box to another box.
    fn transform(&mut self, from: Bounds, to: Bounds) {
        let scale_x = if from.w > 0.0 { to.w / from.w } else { 1.0 };
        let scale_y = if from.h > 0.0 { to.h / from.h } else { 1.0 };
        let map = |point: [f64; 2]| {
            [
                to.x + (point[0] - from.x) * scale_x,
                to.y + (point[1] - from.y) * scale_y,
            ]
        };
        let corner = map([self.x, self.y]);
        self.x = corner[0];
        self.y = corner[1];
        if self.kind != Kind::Text {
            self.w *= scale_x;
            self.h *= scale_y;
        }
        if let Some(points) = &mut self.points {
            for point in points {
                *point = map(*point);
            }
        }
    }

    /// Tells if a point touches the shape, with a tolerance in drawing units.
    pub(crate) fn hit(&self, x: f64, y: f64, tolerance: f64) -> bool {
        let reach = tolerance + self.width / 2.0;
        if !self.bounds().expand(reach).contains(x, y) {
            return false;
        }
        match self.kind {
            Kind::Text => true,
            Kind::Rect => {
                self.fill.is_some()
                    || !self.bounds().expand(-reach).contains(x, y)
                    || self.w <= reach * 2.0
                    || self.h <= reach * 2.0
            }
            Kind::Ellipse => {
                let (rx, ry) = (self.w / 2.0, self.h / 2.0);
                if rx <= 0.0 || ry <= 0.0 {
                    return true;
                }
                let (cx, cy) = (self.x + rx, self.y + ry);
                let outer = ((x - cx) / (rx + reach)).powi(2) + ((y - cy) / (ry + reach)).powi(2);
                let inner_x = (rx - reach).max(0.001);
                let inner_y = (ry - reach).max(0.001);
                let inner = ((x - cx) / inner_x).powi(2) + ((y - cy) / inner_y).powi(2);
                outer <= 1.0 && (self.fill.is_some() || inner >= 1.0)
            }
            Kind::Line | Kind::Arrow | Kind::Freehand => {
                self.points.as_ref().is_some_and(|points| {
                    points
                        .windows(2)
                        .any(|pair| distance_to_segment([x, y], pair[0], pair[1]) <= reach)
                })
            }
        }
    }

    /// Returns a text description for a screen reader. Its position in the list is one-based.
    pub(crate) fn describe(&self, position: usize) -> String {
        let mut text = format!(
            "{position}. {} at {:.0}, {:.0}, size {:.0} by {:.0}, stroke {}",
            self.kind.name(),
            self.x,
            self.y,
            self.w,
            self.h,
            self.stroke
        );
        match &self.fill {
            Some(fill) => text.push_str(&format!(", fill {fill}")),
            None => text.push_str(", no fill"),
        }
        if let Some(content) = &self.text {
            text.push_str(&format!(", text: {content}"));
        }
        text
    }
}

fn new_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    id[..12].to_owned()
}

/// Parses the color form `#rrggbb` into its parts.
pub(crate) fn parse_color(text: &str) -> Option<[u8; 3]> {
    let hex = text.strip_prefix('#')?;
    if hex.len() != 6 || !hex.is_ascii() {
        return None;
    }
    let part = |from: usize| u8::from_str_radix(&hex[from..from + 2], 16).ok();
    Some([part(0)?, part(2)?, part(4)?])
}

/// Formats a color as `#rrggbb`.
pub(crate) fn format_color(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

/// Removes the points of a freehand line that do not change its form (Ramer-Douglas-Peucker).
pub(crate) fn simplify(points: &[[f64; 2]], tolerance: f64) -> Vec<[f64; 2]> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((first, last)) = stack.pop() {
        let mut farthest = (0.0, first);
        for (offset, point) in points[first + 1..last].iter().enumerate() {
            let distance = distance_to_segment(*point, points[first], points[last]);
            if distance > farthest.0 {
                farthest = (distance, first + 1 + offset);
            }
        }
        if farthest.0 > tolerance {
            keep[farthest.1] = true;
            stack.push((first, farthest.1));
            stack.push((farthest.1, last));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(point, keep)| keep.then_some(*point))
        .collect()
}

/// A drawing file that this version cannot use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DrawingError(String);

impl fmt::Display for DrawingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn bad(message: impl Into<String>) -> DrawingError {
    DrawingError(message.into())
}

#[derive(Serialize, Deserialize)]
struct FileForm {
    format: u32,
    shapes: Vec<Shape>,
}

/// The shapes of a drawing. The order of the list is the z-order, from back to front.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Document {
    pub shapes: Vec<Shape>,
}

impl Document {
    /// Parses and validates the bytes of a drawing file.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, DrawingError> {
        if bytes.len() > MAX_BYTES {
            return Err(bad(format!(
                "The drawing is larger than the limit of {MAX_BYTES} bytes."
            )));
        }
        let file: FileForm = serde_json::from_slice(bytes).map_err(|err| {
            bad(format!(
                "The drawing is not valid JSON for format {FORMAT}: {err}"
            ))
        })?;
        if file.format != FORMAT {
            return Err(bad(format!(
                "The drawing has the format {}, and this lenka knows only the format {FORMAT}.",
                file.format
            )));
        }
        let document = Self {
            shapes: file.shapes,
        };
        document.validate()?;
        Ok(document)
    }

    /// Formats the drawing as sorted, pretty JSON with a final line break.
    pub(crate) fn to_text(&self) -> String {
        let file = FileForm {
            format: FORMAT,
            shapes: self.shapes.clone(),
        };
        let mut text = serde_json::to_string_pretty(&file).unwrap_or_else(|err| {
            panic!("the drawing has only finite numbers and text, so JSON cannot fail: {err}")
        });
        text.push('\n');
        text
    }

    /// Formats the drawing for a save. A file that a later open refuses cannot be saved.
    ///
    /// # Errors
    ///
    /// Returns an error if the drawing is outside a limit of a drawing file.
    pub(crate) fn checked_text(&self) -> Result<String, DrawingError> {
        self.validate()?;
        let text = self.to_text();
        if text.len() > MAX_BYTES {
            return Err(bad(format!(
                "The drawing is larger than the limit of {MAX_BYTES} bytes."
            )));
        }
        Ok(text)
    }

    fn validate(&self) -> Result<(), DrawingError> {
        if self.shapes.len() > MAX_SHAPES {
            return Err(bad(format!(
                "The drawing has {} shapes, and the limit is {MAX_SHAPES}.",
                self.shapes.len()
            )));
        }
        let mut ids = BTreeSet::new();
        let mut point_count = 0;
        for (index, shape) in self.shapes.iter().enumerate() {
            let at = format!("Shape {}", index + 1);
            if shape.id.is_empty() || shape.id.len() > 64 {
                return Err(bad(format!("{at} needs an ID with 1 to 64 bytes.")));
            }
            if !ids.insert(shape.id.as_str()) {
                return Err(bad(format!("{at} has an ID that another shape also has.")));
            }
            for value in [shape.x, shape.y, shape.w, shape.h] {
                if !value.is_finite() || value.abs() > MAX_COORDINATE {
                    return Err(bad(format!("{at} has a coordinate outside the limit.")));
                }
            }
            if shape.w < 0.0 || shape.h < 0.0 {
                return Err(bad(format!("{at} has a negative size.")));
            }
            if !shape.width.is_finite() || shape.width < 0.0 || shape.width > MAX_WIDTH {
                return Err(bad(format!("{at} has a stroke width outside the limit.")));
            }
            if parse_color(&shape.stroke).is_none() {
                return Err(bad(format!("{at} has a stroke color that is not #rrggbb.")));
            }
            if shape
                .fill
                .as_deref()
                .is_some_and(|fill| parse_color(fill).is_none())
            {
                return Err(bad(format!("{at} has a fill color that is not #rrggbb.")));
            }
            if shape
                .text
                .as_ref()
                .is_some_and(|text| text.len() > MAX_TEXT_BYTES)
            {
                return Err(bad(format!("{at} has text larger than the limit.")));
            }
            if shape.kind.has_points() {
                let points = shape
                    .points
                    .as_ref()
                    .filter(|points| points.len() >= 2)
                    .ok_or_else(|| bad(format!("{at} needs at least two points.")))?;
                point_count += points.len();
                if point_count > MAX_POINTS {
                    return Err(bad(format!(
                        "The drawing has more than {MAX_POINTS} points."
                    )));
                }
                let good = points
                    .iter()
                    .flatten()
                    .all(|v| v.is_finite() && v.abs() <= MAX_COORDINATE);
                if !good {
                    return Err(bad(format!("{at} has a point outside the limit.")));
                }
            }
        }
        Ok(())
    }

    /// Returns the box around all shapes.
    pub(crate) fn bounds(&self) -> Option<Bounds> {
        self.shapes.iter().map(Shape::bounds).reduce(Bounds::union)
    }

    /// Returns the index of the top shape at a point.
    pub(crate) fn hit(&self, x: f64, y: f64, tolerance: f64) -> Option<usize> {
        self.shapes
            .iter()
            .rposition(|shape| shape.hit(x, y, tolerance))
    }
}

/// A corner of the selection box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    pub(crate) const ALL: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    /// Returns the position of this corner on a box.
    pub(crate) fn on(self, b: Bounds) -> [f64; 2] {
        match self {
            Self::TopLeft => [b.x, b.y],
            Self::TopRight => [b.x + b.w, b.y],
            Self::BottomLeft => [b.x, b.y + b.h],
            Self::BottomRight => [b.x + b.w, b.y + b.h],
        }
    }

    /// Returns the box after this corner moves to a point. The opposite corner stays.
    pub(crate) fn resize(self, b: Bounds, point: [f64; 2]) -> Bounds {
        let fixed = match self {
            Self::TopLeft => Self::BottomRight,
            Self::TopRight => Self::BottomLeft,
            Self::BottomLeft => Self::TopRight,
            Self::BottomRight => Self::TopLeft,
        }
        .on(b);
        Bounds::of_points(fixed, point)
    }
}

/// Shapes at positions that changed in place, with their old forms.
pub(crate) type Snapshot = Vec<(usize, Shape)>;

/// One undo or redo step.
#[derive(Debug, Clone, PartialEq)]
struct Patch {
    /// The positions to remove, in ascending order.
    remove: Vec<usize>,
    /// The shapes to insert, with their final positions in ascending order.
    insert: Vec<(usize, Shape)>,
    /// The state ID after this patch.
    state: u64,
}

/// Applies a patch, and returns the patch that restores the old list.
fn apply(shapes: &mut Vec<Shape>, patch: Patch, old_state: u64) -> Patch {
    let mut removed = Vec::with_capacity(patch.remove.len());
    let mut next_remove = patch.remove.iter().copied().peekable();
    let mut kept = Vec::with_capacity(shapes.len());
    for (index, shape) in std::mem::take(shapes).into_iter().enumerate() {
        if next_remove.peek() == Some(&index) {
            next_remove.next();
            removed.push((index, shape));
        } else {
            kept.push(shape);
        }
    }
    let mut placed = Vec::with_capacity(patch.insert.len());
    let mut insert = patch.insert.into_iter().peekable();
    let mut kept = kept.into_iter();
    let mut result = Vec::with_capacity(kept.len() + insert.len());
    loop {
        if insert.peek().is_some_and(|(at, _)| *at <= result.len()) {
            if let Some((_, shape)) = insert.next() {
                placed.push(result.len());
                result.push(shape);
            }
        } else if let Some(shape) = kept.next() {
            result.push(shape);
        } else if let Some((_, shape)) = insert.next() {
            placed.push(result.len());
            result.push(shape);
        } else {
            break;
        }
    }
    *shapes = result;
    Patch {
        remove: placed,
        insert: removed,
        state: old_state,
    }
}

/// The edit state of a drawing: the shapes, the selection, and the undo history.
#[derive(Debug, Clone, Default)]
pub(crate) struct Editor {
    doc: Document,
    selection: BTreeSet<usize>,
    undo: Vec<Patch>,
    redo: Vec<Patch>,
    state: u64,
    saved_state: u64,
    next_state: u64,
    /// The old form of a text shape while a person types in it. Typing records no undo step for
    /// each key, so the step waits here until the typing ends.
    typing: Option<Snapshot>,
}

impl Editor {
    /// Creates an editor for a document.
    pub(crate) fn new(doc: Document) -> Self {
        Self {
            doc,
            next_state: 1,
            ..Self::default()
        }
    }

    pub(crate) fn doc(&self) -> &Document {
        &self.doc
    }

    pub(crate) fn selection(&self) -> &BTreeSet<usize> {
        &self.selection
    }

    /// Tells if the drawing has edits that are not saved.
    pub(crate) fn is_dirty(&self) -> bool {
        self.state != self.saved_state || self.typing.is_some()
    }

    /// Records that the current state is saved.
    pub(crate) fn mark_saved(&mut self) {
        self.finish_typing();
        self.saved_state = self.state;
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn push_undo(&mut self, mut patch: Patch) {
        patch.state = self.state;
        self.state = self.next_state;
        self.next_state += 1;
        self.undo.push(patch);
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub(crate) fn undo(&mut self) {
        self.finish_typing();
        if let Some(patch) = self.undo.pop() {
            let target = patch.state;
            let inverse = apply(&mut self.doc.shapes, patch, self.state);
            self.state = target;
            self.redo.push(inverse);
            self.selection.clear();
        }
    }

    pub(crate) fn redo(&mut self) {
        self.finish_typing();
        if let Some(patch) = self.redo.pop() {
            let target = patch.state;
            let inverse = apply(&mut self.doc.shapes, patch, self.state);
            self.state = target;
            self.undo.push(inverse);
            self.selection.clear();
        }
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selection.clear();
    }

    pub(crate) fn select_only(&mut self, index: usize) {
        self.selection.clear();
        self.selection.insert(index);
    }

    /// Adds a shape to the selection, or removes a selected shape.
    pub(crate) fn toggle_selected(&mut self, index: usize) {
        if !self.selection.remove(&index) {
            self.selection.insert(index);
        }
    }

    /// Selects all shapes that touch a box.
    pub(crate) fn select_in(&mut self, area: Bounds) {
        self.selection = self
            .doc
            .shapes
            .iter()
            .enumerate()
            .filter(|(_, shape)| shape.bounds().intersects(area))
            .map(|(index, _)| index)
            .collect();
    }

    pub(crate) fn select_all(&mut self) {
        self.selection = (0..self.doc.shapes.len()).collect();
    }

    /// Returns the box around the selected shapes.
    pub(crate) fn selection_bounds(&self) -> Option<Bounds> {
        self.selection
            .iter()
            .filter_map(|index| self.doc.shapes.get(*index))
            .map(Shape::bounds)
            .reduce(Bounds::union)
    }

    /// Adds a shape on top, and selects it.
    pub(crate) fn add(&mut self, shape: Shape) {
        self.finish_typing();
        let at = self.doc.shapes.len();
        self.doc.shapes.push(shape);
        self.push_undo(Patch {
            remove: vec![at],
            insert: Vec::new(),
            state: 0,
        });
        self.select_only(at);
    }

    /// Returns the old forms of the selected shapes, before a change in place.
    pub(crate) fn snapshot(&self) -> Snapshot {
        self.selection
            .iter()
            .filter_map(|index| {
                self.doc
                    .shapes
                    .get(*index)
                    .map(|shape| (*index, shape.clone()))
            })
            .collect()
    }

    /// Records an undo step for a change in place.
    pub(crate) fn commit(&mut self, before: Snapshot) {
        self.finish_typing();
        let changed = before
            .iter()
            .any(|(index, shape)| self.doc.shapes.get(*index) != Some(shape));
        if changed {
            let remove = before.iter().map(|(index, _)| *index).collect();
            self.push_undo(Patch {
                remove,
                insert: before,
                state: 0,
            });
        }
    }

    /// Changes all selected shapes with one undo step.
    pub(crate) fn change_selected(&mut self, mut change: impl FnMut(&mut Shape)) {
        self.finish_typing();
        let before = self.snapshot();
        for (index, _) in &before {
            if let Some(shape) = self.doc.shapes.get_mut(*index) {
                change(shape);
            }
        }
        self.commit(before);
    }

    /// Returns the shape at a position of a snapshot. A position can hold another shape if the
    /// list changed after the snapshot, and a change must not reach that shape.
    fn shape_of_snapshot(&mut self, index: usize, old: &Shape) -> Option<&mut Shape> {
        self.doc
            .shapes
            .get_mut(index)
            .filter(|shape| shape.id == old.id)
    }

    /// Puts the shapes of a snapshot back. This records no undo step, because a gesture records
    /// its step only at its end.
    pub(crate) fn restore(&mut self, before: &Snapshot) {
        for (index, old) in before {
            if let Some(shape) = self.shape_of_snapshot(*index, old) {
                shape.clone_from(old);
            }
        }
    }

    /// Moves the shapes of a snapshot by an offset from their old positions. This records no undo
    /// step, so a drag can call it for each pointer move.
    pub(crate) fn drag_move(&mut self, before: &Snapshot, dx: f64, dy: f64) {
        for (index, old) in before {
            if let Some(shape) = self.shape_of_snapshot(*index, old) {
                shape.clone_from(old);
                shape.translate(dx, dy);
            }
        }
    }

    /// Scales the shapes of a snapshot from one box to another. This records no undo step.
    pub(crate) fn drag_resize(&mut self, before: &Snapshot, from: Bounds, to: Bounds) {
        for (index, old) in before {
            if let Some(shape) = self.shape_of_snapshot(*index, old) {
                shape.clone_from(old);
                shape.transform(from, to);
            }
        }
    }

    /// Sets the text of a text shape. The undo step waits until the typing ends, so typing can
    /// call this for each key.
    pub(crate) fn set_text_live(&mut self, index: usize, text: String) {
        let same_shape = self
            .typing
            .as_ref()
            .is_some_and(|before| before.iter().any(|(at, _)| *at == index));
        if !same_shape {
            self.finish_typing();
            if let Some(shape) = self.doc.shapes.get(index) {
                self.typing = Some(vec![(index, shape.clone())]);
            }
        }
        if let Some(shape) = self.doc.shapes.get_mut(index) {
            shape.text = Some(text);
            shape.fit_text();
        }
    }

    /// Ends the typing, and records its undo step. Each other change calls this first, because
    /// the step holds a position in the list.
    pub(crate) fn finish_typing(&mut self) {
        if let Some(before) = self.typing.take() {
            self.commit(before);
        }
    }

    /// Deletes the selected shapes.
    pub(crate) fn delete_selected(&mut self) {
        self.finish_typing();
        if self.selection.is_empty() {
            return;
        }
        let selected: Vec<usize> = std::mem::take(&mut self.selection).into_iter().collect();
        let patch = Patch {
            remove: selected,
            insert: Vec::new(),
            state: 0,
        };
        let mut inverse = apply(&mut self.doc.shapes, patch, 0);
        inverse.state = 0;
        self.push_undo(inverse);
    }

    /// Copies the selected shapes to the top, with an offset, and selects the copies.
    pub(crate) fn duplicate_selected(&mut self) {
        self.finish_typing();
        let copies: Vec<Shape> = self
            .selection
            .iter()
            .filter_map(|index| self.doc.shapes.get(*index))
            .map(|shape| {
                let mut copy = shape.clone();
                copy.id = new_id();
                copy.translate(16.0, 16.0);
                copy
            })
            .collect();
        if copies.is_empty() {
            return;
        }
        let first = self.doc.shapes.len();
        let count = copies.len();
        self.doc.shapes.extend(copies);
        self.push_undo(Patch {
            remove: (first..first + count).collect(),
            insert: Vec::new(),
            state: 0,
        });
        self.selection = (first..first + count).collect();
    }

    /// Moves the selected shapes to the front of the z-order.
    pub(crate) fn bring_to_front(&mut self) {
        self.reorder(|selected, mut others| {
            others.extend(selected);
            others
        });
    }

    /// Moves the selected shapes to the back of the z-order.
    pub(crate) fn send_to_back(&mut self) {
        self.reorder(|mut selected, others| {
            selected.extend(others);
            selected
        });
    }

    fn reorder(&mut self, join: impl FnOnce(Vec<Shape>, Vec<Shape>) -> Vec<Shape>) {
        self.finish_typing();
        if self.selection.is_empty() {
            return;
        }
        let old = self.doc.shapes.clone();
        let count = old.len();
        let mut selected = Vec::new();
        let mut others = Vec::new();
        for (index, shape) in std::mem::take(&mut self.doc.shapes).into_iter().enumerate() {
            if self.selection.contains(&index) {
                selected.push(shape);
            } else {
                others.push(shape);
            }
        }
        let ids: BTreeSet<String> = selected.iter().map(|shape| shape.id.clone()).collect();
        self.doc.shapes = join(selected, others);
        if self.doc.shapes == old {
            return;
        }
        self.selection = self
            .doc
            .shapes
            .iter()
            .enumerate()
            .filter(|(_, shape)| ids.contains(&shape.id))
            .map(|(index, _)| index)
            .collect();
        self.push_undo(Patch {
            remove: (0..count).collect(),
            insert: old.into_iter().enumerate().collect(),
            state: 0,
        });
    }

    /// Replaces the whole document with new content from a reload. This clears the history.
    pub(crate) fn replace(&mut self, doc: Document) {
        self.doc = doc;
        self.typing = None;
        self.selection.clear();
        self.undo.clear();
        self.redo.clear();
        self.state = 0;
        self.saved_state = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_shape(text: &str) -> Shape {
        let mut shape = Shape::new(Kind::Text, "#1f2937", None, 1.0);
        shape.text = Some(text.to_owned());
        shape.fit_text();
        shape
    }

    /// The sequence of a person who types, deletes the shape, and undoes both steps.
    #[test]
    fn undo_after_typing_and_delete_keeps_ids_unique() {
        let mut editor = Editor::new(Document::default());
        editor.add(rect(0.0, 0.0, 10.0, 10.0));
        editor.add(text_shape("old"));
        editor.set_text_live(1, "new".to_owned());
        editor.delete_selected();
        assert_eq!(editor.doc().shapes.len(), 1);
        editor.undo();
        assert_eq!(editor.doc().shapes[1].text.as_deref(), Some("new"));
        editor.undo();
        assert_eq!(editor.doc().shapes[1].text.as_deref(), Some("old"));
        assert_eq!(editor.doc().shapes.len(), 2);
        // A second finish finds nothing, as the check of the next frame does.
        editor.finish_typing();
        assert_eq!(editor.doc().shapes.len(), 2);
        assert!(Document::from_bytes(editor.doc().to_text().as_bytes()).is_ok());
    }

    #[test]
    fn typing_is_an_unsaved_edit_with_one_undo_step() {
        let mut editor = Editor::new(Document::default());
        editor.add(text_shape("old"));
        editor.mark_saved();
        assert!(!editor.is_dirty());
        editor.set_text_live(0, "n".to_owned());
        assert!(editor.is_dirty(), "a reload must not replace typed text");
        editor.set_text_live(0, "ne".to_owned());
        editor.finish_typing();
        editor.undo();
        assert_eq!(editor.doc().shapes[0].text.as_deref(), Some("old"));
    }

    #[test]
    fn typing_in_a_second_shape_records_the_first_step() {
        let mut editor = Editor::new(Document::default());
        editor.add(text_shape("one"));
        editor.add(text_shape("two"));
        editor.set_text_live(0, "one more".to_owned());
        editor.set_text_live(1, "two more".to_owned());
        editor.finish_typing();
        editor.undo();
        assert_eq!(editor.doc().shapes[1].text.as_deref(), Some("two"));
        assert_eq!(editor.doc().shapes[0].text.as_deref(), Some("one more"));
        editor.undo();
        assert_eq!(editor.doc().shapes[0].text.as_deref(), Some("one"));
    }

    #[test]
    fn a_save_ends_the_typing() {
        let mut editor = Editor::new(Document::default());
        editor.add(text_shape("old"));
        editor.set_text_live(0, "new".to_owned());
        editor.mark_saved();
        assert!(!editor.is_dirty());
        editor.finish_typing();
        assert!(
            !editor.is_dirty(),
            "the step of the saved text is not a new edit"
        );
    }

    #[test]
    fn a_drag_cannot_change_another_shape_after_the_list_changes() {
        let mut editor = Editor::new(Document::default());
        editor.add(rect(0.0, 0.0, 10.0, 10.0));
        editor.add(rect(50.0, 50.0, 10.0, 10.0));
        editor.select_only(0);
        let before = editor.snapshot();
        let other = editor.doc().shapes[1].clone();
        editor.delete_selected();
        editor.drag_move(&before, 5.0, 5.0);
        editor.restore(&before);
        assert_eq!(editor.doc().shapes, vec![other.clone()]);
        let from = before[0].1.bounds();
        editor.drag_resize(&before, from, from.expand(5.0));
        assert_eq!(editor.doc().shapes, vec![other]);
    }

    #[test]
    fn a_save_refuses_a_drawing_that_a_later_open_refuses() {
        let mut document = Document::default();
        assert!(document.checked_text().is_ok());
        document
            .shapes
            .push(text_shape(&"x".repeat(MAX_TEXT_BYTES + 1)));
        assert!(document.checked_text().is_err());
        let mut far = rect(0.0, 0.0, 1.0, 1.0);
        far.x = MAX_COORDINATE * 2.0;
        document.shapes = vec![far];
        assert!(document.checked_text().is_err());
        let copy = rect(0.0, 0.0, 1.0, 1.0);
        document.shapes = vec![copy.clone(), copy];
        assert!(document.checked_text().is_err(), "duplicate IDs");
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Shape {
        let mut shape = Shape::new(Kind::Rect, "#1f2937", None, 2.0);
        shape.x = x;
        shape.y = y;
        shape.w = w;
        shape.h = h;
        shape
    }

    fn line(a: [f64; 2], b: [f64; 2]) -> Shape {
        let mut shape = Shape::new(Kind::Line, "#1f2937", None, 2.0);
        shape.points = Some(vec![a, b]);
        shape.fit_points();
        shape
    }

    fn sample() -> Document {
        let mut text = Shape::new(Kind::Text, "#111827", None, 1.0);
        text.text = Some("Queue\nworker".to_owned());
        text.fit_text();
        let mut free = Shape::new(Kind::Freehand, "#ff0000", Some("#00ff00"), 1.5);
        free.points = Some(vec![[0.0, 0.0], [10.25, 3.5], [20.0, 0.0]]);
        free.fit_points();
        Document {
            shapes: vec![
                rect(1.0, 2.0, 30.0, 40.0),
                line([0.0, 0.0], [5.0, 5.0]),
                free,
                text,
            ],
        }
    }

    #[test]
    fn round_trip_is_stable_and_sorted() {
        let doc = sample();
        let text = doc.to_text();
        let parsed = Document::from_bytes(text.as_bytes()).expect("a saved drawing must parse");
        assert_eq!(parsed.to_text(), text);
        assert_eq!(parsed.shapes.len(), 4);
        assert_eq!(parsed.shapes[2].points.as_ref().map(Vec::len), Some(3));
        // Integers stay integers, so that Git shows small changes.
        assert!(text.contains("\"x\": 1,"), "{text}");
        let keys = [
            "\"fill\"",
            "\"h\"",
            "\"id\"",
            "\"kind\"",
            "\"points\"",
            "\"stroke\"",
            "\"text\"",
            "\"w\"",
            "\"width\"",
            "\"x\"",
            "\"y\"",
        ];
        let first = &text[text.find("\"fill\"").expect("the fill key")..];
        let mut last = 0;
        for key in keys {
            let at = first.find(key).expect("the key");
            assert!(at >= last, "{key} is out of order");
            last = at;
        }
        assert!(text.find("\"format\"") < text.find("\"shapes\""));
        assert!(text.ends_with("}\n"));
    }

    #[test]
    fn the_documented_example_parses() {
        let text = r##"{"format": 1, "shapes": [{"id": "a", "kind": "rect", "x": 0, "y": 0, "w": 10, "h": 10, "stroke": "#1f2937", "fill": null, "width": 2, "text": null, "points": null}]}"##;
        let doc = Document::from_bytes(text.as_bytes()).expect("the example must parse");
        assert_eq!(doc.shapes[0].kind, Kind::Rect);
        assert!((doc.shapes[0].w - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn bad_files_give_errors() {
        let cases: &[(&str, &str)] = &[
            ("not json", "not valid JSON"),
            (r#"{"format": 2, "shapes": []}"#, "format 2"),
            (r#"{"format": 1}"#, "not valid JSON"),
            (
                r##"{"format":1,"shapes":[{"id":"a","kind":"star","x":0,"y":0,"w":1,"h":1,"stroke":"#000000","width":1}]}"##,
                "not valid JSON",
            ),
            (
                r##"{"format":1,"shapes":[{"id":"a","kind":"rect","x":0,"y":0,"w":-1,"h":1,"stroke":"#000000","width":1}]}"##,
                "negative size",
            ),
            (
                r##"{"format":1,"shapes":[{"id":"a","kind":"rect","x":1e99,"y":0,"w":1,"h":1,"stroke":"#000000","width":1}]}"##,
                "coordinate",
            ),
            (
                r##"{"format":1,"shapes":[{"id":"a","kind":"rect","x":0,"y":0,"w":1,"h":1,"stroke":"red","width":1}]}"##,
                "stroke color",
            ),
            (
                r##"{"format":1,"shapes":[{"id":"a","kind":"rect","x":0,"y":0,"w":1,"h":1,"stroke":"#000000","fill":"#12","width":1}]}"##,
                "fill color",
            ),
            (
                r##"{"format":1,"shapes":[{"id":"a","kind":"line","x":0,"y":0,"w":1,"h":1,"stroke":"#000000","width":1}]}"##,
                "two points",
            ),
            (
                r##"{"format":1,"shapes":[{"id":"a","kind":"rect","x":0,"y":0,"w":1,"h":1,"stroke":"#000000","width":1},{"id":"a","kind":"rect","x":0,"y":0,"w":1,"h":1,"stroke":"#000000","width":1}]}"##,
                "another shape",
            ),
        ];
        for (text, wanted) in cases {
            let error = Document::from_bytes(text.as_bytes())
                .expect_err(text)
                .to_string();
            assert!(error.contains(wanted), "{text}: {error}");
        }
    }

    #[test]
    fn too_many_shapes_give_an_error() {
        let shape = rect(0.0, 0.0, 1.0, 1.0);
        let shapes: Vec<Shape> = (0..=MAX_SHAPES)
            .map(|index| Shape {
                id: index.to_string(),
                ..shape.clone()
            })
            .collect();
        let text = Document { shapes }.to_text();
        let error = Document::from_bytes(text.as_bytes())
            .expect_err("too many")
            .to_string();
        assert!(error.contains("limit"), "{error}");
    }

    #[test]
    fn hit_test_follows_z_order_and_fill() {
        let mut filled = rect(0.0, 0.0, 100.0, 100.0);
        filled.fill = Some("#ffffff".to_owned());
        let hollow = rect(50.0, 50.0, 100.0, 100.0);
        let doc = Document {
            shapes: vec![filled, hollow],
        };
        assert_eq!(
            doc.hit(50.0, 50.0, 2.0),
            Some(1),
            "the top shape at its border wins"
        );
        assert_eq!(
            doc.hit(75.0, 75.0, 2.0),
            Some(0),
            "a hollow center does not catch a click"
        );
        assert_eq!(doc.hit(140.0, 140.0, 2.0), None);
        let wire = line([0.0, 0.0], [100.0, 100.0]);
        assert!(wire.hit(50.0, 50.0, 1.0));
        assert!(!wire.hit(50.0, 60.0, 1.0));
    }

    #[test]
    fn undo_and_redo_restore_all_edits() {
        let mut editor = Editor::new(Document::default());
        assert!(!editor.is_dirty());
        editor.add(rect(0.0, 0.0, 10.0, 10.0));
        editor.add(rect(20.0, 0.0, 10.0, 10.0));
        editor.add(rect(40.0, 0.0, 10.0, 10.0));
        assert!(editor.is_dirty());
        let three = editor.doc().clone();

        editor.select_only(1);
        let before = editor.snapshot();
        editor.drag_move(&before, 5.0, 7.0);
        editor.commit(before);
        assert!((editor.doc().shapes[1].x - 25.0).abs() < f64::EPSILON);
        let moved = editor.doc().clone();

        editor.select_only(0);
        editor.delete_selected();
        assert_eq!(editor.doc().shapes.len(), 2);
        let deleted = editor.doc().clone();

        editor.undo();
        assert_eq!(editor.doc(), &moved);
        editor.undo();
        assert_eq!(editor.doc(), &three);
        editor.redo();
        assert_eq!(editor.doc(), &moved);
        editor.redo();
        assert_eq!(editor.doc(), &deleted);
        assert!(!editor.can_redo());

        for _ in 0..5 {
            editor.undo();
        }
        assert!(editor.doc().shapes.is_empty());
        assert!(!editor.is_dirty(), "a full undo returns to the saved state");
        assert!(!editor.can_undo());
    }

    #[test]
    fn a_new_edit_clears_redo_and_save_clears_dirty() {
        let mut editor = Editor::new(Document::default());
        editor.add(rect(0.0, 0.0, 10.0, 10.0));
        editor.mark_saved();
        assert!(!editor.is_dirty());
        editor.add(rect(1.0, 1.0, 1.0, 1.0));
        editor.undo();
        assert!(!editor.is_dirty());
        assert!(editor.can_redo());
        editor.add(rect(2.0, 2.0, 2.0, 2.0));
        assert!(!editor.can_redo());
        assert!(editor.is_dirty());
    }

    #[test]
    fn duplicate_makes_new_ids_and_z_order_changes_undo() {
        let mut editor = Editor::new(Document::default());
        editor.add(rect(0.0, 0.0, 10.0, 10.0));
        editor.add(rect(5.0, 5.0, 10.0, 10.0));
        editor.select_only(0);
        let first_id = editor.doc().shapes[0].id.clone();
        editor.bring_to_front();
        assert_eq!(editor.doc().shapes[1].id, first_id);
        assert_eq!(
            editor.selection().iter().copied().collect::<Vec<_>>(),
            vec![1]
        );
        editor.undo();
        assert_eq!(editor.doc().shapes[0].id, first_id);

        editor.select_only(0);
        editor.duplicate_selected();
        assert_eq!(editor.doc().shapes.len(), 3);
        assert_ne!(editor.doc().shapes[2].id, first_id);
        assert!((editor.doc().shapes[2].x - 16.0).abs() < f64::EPSILON);
        editor.select_only(2);
        editor.send_to_back();
        assert_eq!(
            editor.selection().iter().copied().collect::<Vec<_>>(),
            vec![0]
        );
        editor.undo();
        editor.undo();
        assert_eq!(editor.doc().shapes.len(), 2);
    }

    #[test]
    fn resize_scales_points_and_corners_keep_the_opposite_side() {
        let mut editor = Editor::new(Document::default());
        editor.add(line([0.0, 0.0], [10.0, 20.0]));
        let from = editor.selection_bounds().expect("a selection");
        let before = editor.snapshot();
        let to = Corner::BottomRight.resize(from, [20.0, 10.0]);
        assert_eq!(
            to,
            Bounds {
                x: 0.0,
                y: 0.0,
                w: 20.0,
                h: 10.0
            }
        );
        editor.drag_resize(&before, from, to);
        editor.commit(before);
        assert_eq!(
            editor.doc().shapes[0].points.as_deref(),
            Some(&[[0.0, 0.0], [20.0, 10.0]][..])
        );
        assert!((editor.doc().shapes[0].w - 20.0).abs() < f64::EPSILON);
        let top_left = Corner::TopLeft.resize(from, [5.0, 5.0]);
        assert_eq!(
            top_left,
            Bounds {
                x: 5.0,
                y: 5.0,
                w: 5.0,
                h: 15.0
            }
        );
    }

    #[test]
    fn simplify_keeps_the_form_and_the_ends() {
        let points: Vec<[f64; 2]> = (0..100).map(|i| [f64::from(i), 0.0]).collect();
        assert_eq!(simplify(&points, 0.5), vec![[0.0, 0.0], [99.0, 0.0]]);
        let corner = [[0.0, 0.0], [5.0, 0.0], [10.0, 0.0], [10.0, 10.0]];
        assert_eq!(
            simplify(&corner, 0.5),
            vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0]]
        );
    }

    #[test]
    fn descriptions_name_the_shape_for_screen_readers() {
        let mut shape = rect(1.0, 2.0, 3.0, 4.0);
        shape.text = Some("Note".to_owned());
        let text = shape.describe(7);
        assert_eq!(
            text,
            "7. Rectangle at 1, 2, size 3 by 4, stroke #1f2937, no fill, text: Note"
        );
    }

    #[test]
    fn colors_parse_and_format() {
        assert_eq!(parse_color("#1f2937"), Some([0x1f, 0x29, 0x37]));
        assert_eq!(parse_color("1f2937"), None);
        assert_eq!(parse_color("#1f29"), None);
        assert_eq!(parse_color("#1f293é"), None);
        assert_eq!(format_color([255, 0, 16]), "#ff0010");
    }
}
