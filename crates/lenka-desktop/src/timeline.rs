// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The calculations of the roadmap and the Gantt chart. This module has no UI.

use std::collections::{HashMap, HashSet};

use lenka_plugin_api::{Day, TaskId};
use lenka_tasks::{BoardRow, Link, TaskFront};

use crate::data::BoardModel;

/// A range of days, with the first day and the last day. Both are day numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DaySpan {
    pub start: i32,
    pub end: i32,
}

impl DaySpan {
    pub(crate) fn union(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// Returns the count of days, which is at least one.
    pub(crate) fn days(self) -> i32 {
        self.end - self.start + 1
    }
}

/// Returns the planned days of a task. A task with one date has a span of one day. A due date
/// before the start date gives the start date only.
pub(crate) fn own_span(front: &TaskFront) -> Option<DaySpan> {
    match (front.start, front.due) {
        (Some(start), Some(due)) => {
            let start = start.number();
            Some(DaySpan {
                start,
                end: due.number().max(start),
            })
        }
        (Some(day), None) | (None, Some(day)) => Some(DaySpan {
            start: day.number(),
            end: day.number(),
        }),
        (None, None) => None,
    }
}

/// The values of a top-level task on the roadmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rollup {
    pub index: usize,
    /// The task dates, or else the earliest and latest dates of its descendants.
    pub span: Option<DaySpan>,
    /// The count of closed descendants. A task with no descendants counts as its own part.
    pub closed: usize,
    pub total: usize,
}

impl Rollup {
    /// Returns the part that is complete, from 0 to 1.
    pub(crate) fn progress(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        #[expect(clippy::cast_precision_loss, reason = "task counts are small")]
        let part = self.closed as f32 / self.total as f32;
        part
    }
}

/// Calculates the rollup of one task. The walk ignores a parent cycle from a Git merge.
pub(crate) fn rollup(model: &BoardModel, index: usize) -> Rollup {
    let mut seen = HashSet::from([index]);
    let mut stack: Vec<usize> = model.children_of(index).to_vec();
    let (mut closed, mut total, mut from_children) = (0, 0, None::<DaySpan>);
    while let Some(current) = stack.pop() {
        if !seen.insert(current) {
            continue;
        }
        let row = &model.rows[current];
        total += 1;
        if row.meta.state.status.is_closed() {
            closed += 1;
        }
        if let Some(span) = own_span(&row.meta.front) {
            from_children = Some(from_children.map_or(span, |value| value.union(span)));
        }
        stack.extend_from_slice(model.children_of(current));
    }
    let row = &model.rows[index];
    if total == 0 {
        total = 1;
        closed = usize::from(row.meta.state.status.is_closed());
    }
    Rollup {
        index,
        span: own_span(&row.meta.front).or(from_children),
        closed,
        total,
    }
}

/// Returns the roadmap rows: all top-level tasks, in board order.
pub(crate) fn roadmap_rows(model: &BoardModel) -> Vec<Rollup> {
    model
        .roots
        .iter()
        .map(|index| rollup(model, *index))
        .collect()
}

/// A mark on the time axis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tick {
    pub day: i32,
    pub label: String,
    /// Tells if the tick starts a larger unit, such as a year or a month.
    pub major: bool,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn month_name(day: Day) -> &'static str {
    MONTHS[usize::from(day.month().clamp(1, 12)) - 1]
}

/// Returns the first day of the month of a day number.
pub(crate) fn month_start(number: i32) -> i32 {
    number - i32::from(Day::from_number(number).day_of_month()) + 1
}

/// Returns the first day of the next month of a day number.
pub(crate) fn next_month(number: i32) -> i32 {
    month_start(month_start(number) + 32)
}

/// Returns a tick for each month that starts in a range, and for the month of the first day.
pub(crate) fn month_ticks(from: i32, to: i32) -> Vec<Tick> {
    let mut ticks = Vec::new();
    let mut day = month_start(from);
    while day <= to && ticks.len() < 2400 {
        let value = Day::from_number(day);
        let label = if value.month() == 1 {
            format!("{} {}", month_name(value), value.year())
        } else {
            month_name(value).to_owned()
        };
        ticks.push(Tick {
            day,
            label,
            major: value.month() == 1,
        });
        day = next_month(day);
    }
    ticks
}

/// Returns ticks in a range for a day scale. The step depends on the pixels for each day.
pub(crate) fn day_ticks(from: i32, to: i32, px_per_day: f32) -> Vec<Tick> {
    let step = if px_per_day >= 24.0 {
        1
    } else if px_per_day >= 7.0 {
        7
    } else {
        return month_ticks(from, to);
    };
    let mut ticks = Vec::new();
    // A Julian day number modulo 7 gives the weekday, with 0 for Monday.
    let mut day = if step == 7 {
        from - from.rem_euclid(7)
    } else {
        from
    };
    while day <= to && ticks.len() < 2400 {
        let value = Day::from_number(day);
        let label = format!("{} {}", value.day_of_month(), month_name(value));
        let major = day.rem_euclid(7) == 0;
        ticks.push(Tick { day, label, major });
        day += step;
    }
    ticks
}

/// The link between days and horizontal positions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Scale {
    /// The day at the left side of the chart, with a fraction.
    pub origin: f64,
    pub px_per_day: f32,
}

impl Scale {
    pub(crate) const MIN_PX_PER_DAY: f32 = 0.5;
    pub(crate) const MAX_PX_PER_DAY: f32 = 80.0;

    /// Returns the distance from the left side of the chart to the start of a day.
    pub(crate) fn x_of(&self, day: f64) -> f32 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "chart positions fit in f32"
        )]
        let x = ((day - self.origin) * f64::from(self.px_per_day)) as f32;
        x
    }

    /// Returns the day at a distance from the left side of the chart.
    pub(crate) fn day_at(&self, x: f32) -> f64 {
        self.origin + f64::from(x) / f64::from(self.px_per_day)
    }

    /// Changes the zoom, and keeps the day at a position in place.
    pub(crate) fn zoom_at(&mut self, x: f32, factor: f32) {
        let fixed = self.day_at(x);
        self.px_per_day =
            (self.px_per_day * factor).clamp(Self::MIN_PX_PER_DAY, Self::MAX_PX_PER_DAY);
        self.origin = fixed - f64::from(x) / f64::from(self.px_per_day);
    }

    /// Moves the chart by a distance in pixels.
    pub(crate) fn pan(&mut self, dx: f32) {
        self.origin -= f64::from(dx) / f64::from(self.px_per_day);
    }

    /// Sets the scale so that a range fits in a width, with a small margin.
    pub(crate) fn fit(span: DaySpan, width: f32) -> Self {
        let margin = 0.06;
        let days = f64::from(span.days()).max(14.0);
        let total = days * (1.0 + margin * 2.0);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the clamp limits the value"
        )]
        let px_per_day = (f64::from(width.max(100.0)) / total) as f32;
        Self {
            origin: f64::from(span.start) - days * margin,
            px_per_day: px_per_day.clamp(Self::MIN_PX_PER_DAY, Self::MAX_PX_PER_DAY),
        }
    }
}

/// What a drag on a Gantt bar changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BarDrag {
    Move,
    Start,
    End,
}

/// Returns the span after a drag by some days. The first day never passes the last day.
pub(crate) fn dragged_span(span: DaySpan, drag: BarDrag, days: i32) -> DaySpan {
    match drag {
        BarDrag::Move => DaySpan {
            start: span.start + days,
            end: span.end + days,
        },
        BarDrag::Start => DaySpan {
            start: (span.start + days).min(span.end),
            end: span.end,
        },
        BarDrag::End => DaySpan {
            start: span.start,
            end: (span.end + days).max(span.start),
        },
    }
}

/// Tells if a task starts before its open blocker ends.
pub(crate) fn starts_before_blocker(task: DaySpan, blocker: DaySpan) -> bool {
    task.start <= blocker.end
}

/// A row of the Gantt chart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GanttRow {
    pub index: usize,
    pub span: DaySpan,
    /// Tells if the task starts before one of its open blockers ends.
    pub conflict: bool,
}

/// An arrow from a blocker row to a dependent row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GanttArrow {
    pub from: usize,
    pub to: usize,
}

/// The layout of the Gantt chart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GanttLayout {
    pub rows: Vec<GanttRow>,
    pub arrows: Vec<GanttArrow>,
    /// The tasks with no dates.
    pub undated: Vec<usize>,
}

/// Calculates the Gantt chart for some tasks. The rows go by first day, then by board order.
pub(crate) fn gantt_layout(rows: &[BoardRow], visible: &[usize]) -> GanttLayout {
    let mut layout = GanttLayout::default();
    for index in visible {
        match own_span(&rows[*index].meta.front) {
            Some(span) => layout.rows.push(GanttRow {
                index: *index,
                span,
                conflict: false,
            }),
            None => layout.undated.push(*index),
        }
    }
    layout.rows.sort_by_key(|row| (row.span.start, row.index));
    let row_of: HashMap<TaskId, usize> = layout
        .rows
        .iter()
        .enumerate()
        .map(|(position, row)| (rows[row.index].meta.id(), position))
        .collect();
    let spans: Vec<(DaySpan, bool)> = layout
        .rows
        .iter()
        .map(|row| (row.span, rows[row.index].meta.state.status.is_closed()))
        .collect();
    let mut arrows = Vec::new();
    for (to, row) in layout.rows.iter_mut().enumerate() {
        let front = &rows[row.index].meta.front;
        for target in front
            .blocked_by
            .iter()
            .filter_map(|link: &Link| link.local())
        {
            let Some(from) = row_of.get(&target).copied() else {
                continue;
            };
            arrows.push(GanttArrow { from, to });
            let (span, closed) = spans[from];
            if !closed && starts_before_blocker(row.span, span) {
                row.conflict = true;
            }
        }
    }
    layout.arrows = arrows;
    layout
}

/// Parses the text of a date field. An empty text means no date.
pub(crate) fn parse_day_field(text: &str) -> Result<Option<Day>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    Day::parse(text)
        .map(Some)
        .map_err(|err| err.message().to_owned())
}

#[cfg(test)]
mod tests {
    use lenka_plugin_api::Timestamp;
    use lenka_tasks::{Priority, State, Status, TaskMeta};

    use super::*;

    fn day(text: &str) -> i32 {
        Day::parse(text).expect("a valid day").number()
    }

    fn row(
        start: Option<&str>,
        due: Option<&str>,
        parent: Option<TaskId>,
        status: Status,
    ) -> BoardRow {
        BoardRow {
            meta: TaskMeta {
                front: TaskFront {
                    id: TaskId::generate(),
                    kind: "task".to_owned(),
                    title: "T".to_owned(),
                    parent,
                    blocked_by: Vec::new(),
                    relates: Vec::new(),
                    artifacts: Vec::new(),
                    priority: Priority::Normal,
                    labels: Vec::new(),
                    created_by: "t".to_owned(),
                    created_at: Timestamp::from_unix_ms(0),
                    start: start.map(|text| Day::parse(text).expect("a valid start")),
                    due: due.map(|text| Day::parse(text).expect("a valid due date")),
                    coordinated: false,
                },
                state: State {
                    version: 1,
                    status,
                    stage: "work".to_owned(),
                    unblocked: false,
                },
                has_plan: false,
                stamps: [None, None, None],
            },
            blocked: false,
            claimed_by: None,
            child_count: 0,
            closed_children: 0,
            invalid: false,
        }
    }

    #[test]
    fn own_span_handles_missing_and_reversed_dates() {
        let both = row(Some("2026-11-01"), Some("2026-11-10"), None, Status::Todo);
        assert_eq!(
            own_span(&both.meta.front),
            Some(DaySpan {
                start: day("2026-11-01"),
                end: day("2026-11-10")
            })
        );
        let only_due = row(None, Some("2026-11-10"), None, Status::Todo);
        assert_eq!(own_span(&only_due.meta.front).map(DaySpan::days), Some(1));
        let reversed = row(Some("2026-11-10"), Some("2026-11-01"), None, Status::Todo);
        assert_eq!(own_span(&reversed.meta.front).map(DaySpan::days), Some(1));
        assert_eq!(
            own_span(&row(None, None, None, Status::Todo).meta.front),
            None
        );
    }

    #[test]
    fn rollup_uses_descendants_when_the_task_has_no_dates() {
        let root = row(None, None, None, Status::InProgress);
        let root_id = root.meta.id();
        let child = row(
            Some("2026-11-05"),
            Some("2026-11-20"),
            Some(root_id),
            Status::Done,
        );
        let child_id = child.meta.id();
        let grandchild = row(
            Some("2026-12-01"),
            Some("2026-12-24"),
            Some(child_id),
            Status::Todo,
        );
        let other = row(Some("2026-10-01"), None, None, Status::Done);
        let model = BoardModel::new(vec![root, child, grandchild, other]);
        let rows = roadmap_rows(&model);
        assert_eq!(rows.len(), 2, "only top-level tasks are rows");
        assert_eq!(
            rows[0].span,
            Some(DaySpan {
                start: day("2026-11-05"),
                end: day("2026-12-24")
            })
        );
        assert_eq!((rows[0].closed, rows[0].total), (1, 2));
        assert!((rows[0].progress() - 0.5).abs() < f32::EPSILON);
        assert_eq!(
            (rows[1].closed, rows[1].total),
            (1, 1),
            "a task with no descendants counts itself"
        );
    }

    #[test]
    fn rollup_prefers_the_own_dates_and_survives_a_cycle() {
        let mut first = row(Some("2026-01-01"), Some("2026-01-02"), None, Status::Todo);
        let mut second = row(
            Some("2026-05-01"),
            None,
            Some(first.meta.id()),
            Status::Todo,
        );
        first.meta.front.parent = Some(second.meta.id());
        second.meta.front.parent = Some(first.meta.id());
        let model = BoardModel::new(vec![first, second]);
        let value = rollup(&model, 0);
        assert_eq!(value.span.map(|span| span.start), Some(day("2026-01-01")));
        assert_eq!(value.total, 1);
    }

    #[test]
    fn month_ticks_cross_year_ends() {
        let ticks = month_ticks(day("2026-11-15"), day("2027-02-02"));
        let labels: Vec<&str> = ticks.iter().map(|tick| tick.label.as_str()).collect();
        assert_eq!(labels, vec!["Nov", "Dec", "Jan 2027", "Feb"]);
        assert_eq!(ticks[0].day, day("2026-11-01"));
        assert_eq!(ticks[1].day, day("2026-12-01"));
        assert_eq!(ticks[3].day, day("2027-02-01"));
        assert!(ticks[2].major);
        assert_eq!(next_month(day("2024-01-31")), day("2024-02-01"));
        assert_eq!(next_month(day("2024-02-29")), day("2024-03-01"));
    }

    #[test]
    fn day_ticks_change_their_step_with_zoom() {
        let (from, to) = (day("2026-11-02"), day("2026-11-16"));
        assert_eq!(day_ticks(from, to, 30.0).len(), 15);
        let weekly = day_ticks(from, to, 10.0);
        assert!(weekly.windows(2).all(|pair| pair[1].day - pair[0].day == 7));
        // The first day of November 2026 that this test uses is a Monday.
        assert_eq!(weekly[0].day, from);
        assert!(weekly[0].major);
        assert!(
            day_ticks(from, to, 2.0)
                .iter()
                .all(|tick| Day::from_number(tick.day).day_of_month() == 1)
        );
    }

    #[test]
    fn scale_converts_zooms_and_pans() {
        let mut scale = Scale {
            origin: 100.0,
            px_per_day: 10.0,
        };
        assert!((scale.x_of(105.0) - 50.0).abs() < 0.001);
        assert!((scale.day_at(50.0) - 105.0).abs() < 0.001);
        scale.zoom_at(50.0, 2.0);
        assert!((scale.px_per_day - 20.0).abs() < 0.001);
        assert!(
            (scale.day_at(50.0) - 105.0).abs() < 0.001,
            "the day under the pointer stays"
        );
        scale.pan(20.0);
        assert!((scale.origin - 101.5).abs() < 0.001);
        scale.zoom_at(0.0, 1_000.0);
        assert!((scale.px_per_day - Scale::MAX_PX_PER_DAY).abs() < 0.001);
        let fitted = Scale::fit(DaySpan { start: 0, end: 99 }, 1120.0);
        assert!(fitted.x_of(0.0) > 0.0);
        assert!(fitted.x_of(100.0) < 1120.0);
    }

    #[test]
    fn dragged_spans_keep_their_order() {
        let span = DaySpan { start: 10, end: 20 };
        assert_eq!(
            dragged_span(span, BarDrag::Move, 5),
            DaySpan { start: 15, end: 25 }
        );
        assert_eq!(
            dragged_span(span, BarDrag::Start, 3),
            DaySpan { start: 13, end: 20 }
        );
        assert_eq!(
            dragged_span(span, BarDrag::Start, 30),
            DaySpan { start: 20, end: 20 }
        );
        assert_eq!(
            dragged_span(span, BarDrag::End, -30),
            DaySpan { start: 10, end: 10 }
        );
        assert_eq!(
            dragged_span(span, BarDrag::End, 2),
            DaySpan { start: 10, end: 22 }
        );
    }

    #[test]
    fn gantt_sorts_finds_arrows_conflicts_and_undated_tasks() {
        let blocker = row(Some("2026-11-05"), Some("2026-11-10"), None, Status::Todo);
        let blocker_id = blocker.meta.id();
        let mut early = row(Some("2026-11-08"), Some("2026-11-12"), None, Status::Todo);
        early.meta.front.blocked_by = vec![Link::Local(blocker_id)];
        let mut late = row(Some("2026-11-11"), Some("2026-11-12"), None, Status::Todo);
        late.meta.front.blocked_by = vec![Link::Local(blocker_id)];
        let done = row(Some("2026-11-01"), Some("2026-11-30"), None, Status::Done);
        let mut after_done = row(Some("2026-11-02"), None, None, Status::Todo);
        after_done.meta.front.blocked_by = vec![Link::Local(done.meta.id())];
        let undated = row(None, None, None, Status::Todo);
        let rows = vec![early, blocker, late, done, after_done, undated];
        let visible: Vec<usize> = (0..rows.len()).collect();
        let layout = gantt_layout(&rows, &visible);
        assert_eq!(layout.undated, vec![5]);
        let order: Vec<usize> = layout.rows.iter().map(|item| item.index).collect();
        assert_eq!(order, vec![3, 4, 1, 0, 2]);
        assert_eq!(layout.arrows.len(), 3);
        assert!(
            layout.rows[3].conflict,
            "the task starts before its blocker ends"
        );
        assert!(
            !layout.rows[4].conflict,
            "the task starts after its blocker ends"
        );
        assert!(
            !layout.rows[1].conflict,
            "a closed blocker makes no conflict"
        );
    }

    #[test]
    fn date_fields_parse() {
        assert_eq!(parse_day_field(""), Ok(None));
        assert!(parse_day_field("2026-11-06").expect("a date").is_some());
        assert!(parse_day_field("tomorrow").is_err());
    }
}
