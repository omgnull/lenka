// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The cached task list and its filters. This module has no UI.

use std::collections::{BTreeSet, HashMap, HashSet};

use lenka_core::ProjectConfig;
use lenka_plugin_api::{Actor, TaskId};
use lenka_tasks::{BoardRow, Priority, Status};

/// All tasks of a project, with the links that the views need. It holds no task text.
#[derive(Debug, Default)]
pub(crate) struct BoardModel {
    pub rows: Vec<BoardRow>,
    pub by_id: HashMap<TaskId, usize>,
    children: HashMap<usize, Vec<usize>>,
    /// The tasks with no parent, or with a parent that does not exist.
    pub roots: Vec<usize>,
    pub kinds: BTreeSet<String>,
    pub labels: BTreeSet<String>,
}

impl BoardModel {
    /// Creates the model from the rows of the service.
    pub(crate) fn new(rows: Vec<BoardRow>) -> Self {
        let by_id: HashMap<TaskId, usize> = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.meta.id(), index))
            .collect();
        let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut roots = Vec::new();
        let mut kinds = BTreeSet::new();
        let mut labels = BTreeSet::new();
        for (index, row) in rows.iter().enumerate() {
            match row.meta.front.parent.and_then(|parent| by_id.get(&parent)) {
                Some(parent) => children.entry(*parent).or_default().push(index),
                None => roots.push(index),
            }
            if !kinds.contains(&row.meta.front.kind) {
                kinds.insert(row.meta.front.kind.clone());
            }
            for label in &row.meta.front.labels {
                if !labels.contains(label) {
                    labels.insert(label.clone());
                }
            }
        }
        Self {
            rows,
            by_id,
            children,
            roots,
            kinds,
            labels,
        }
    }

    /// Returns the children of a task.
    pub(crate) fn children_of(&self, index: usize) -> &[usize] {
        self.children.get(&index).map_or(&[], Vec::as_slice)
    }

    /// Returns the row of a task.
    pub(crate) fn row(&self, id: TaskId) -> Option<&BoardRow> {
        self.by_id.get(&id).map(|index| &self.rows[*index])
    }
}

/// The filter of the board, the tree, and the Gantt chart.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub(crate) struct TaskFilter {
    pub text: String,
    pub kind: Option<String>,
    pub label: Option<String>,
    pub priority: Option<Priority>,
    pub blocked_only: bool,
    pub ready_only: bool,
}

impl TaskFilter {
    /// Tells if no part of the filter is set.
    pub(crate) fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Tells if a task is ready for an actor, with the rules of the spec, section 2.8.
pub(crate) fn is_ready(row: &BoardRow, config: &ProjectConfig, actor: Actor) -> bool {
    let status_ok = match row.meta.state.status {
        Status::Todo => true,
        Status::InProgress => row.claimed_by.is_none(),
        _ => false,
    };
    if !status_ok || row.invalid || row.blocked || row.child_count > row.closed_children {
        return false;
    }
    config
        .types
        .get(&row.meta.front.kind)
        .and_then(|def| {
            def.stages
                .iter()
                .find(|stage| stage.name == row.meta.state.stage)
        })
        .is_some_and(|stage| actor.can_do(stage.actor))
}

/// Returns the positions of the rows that match a filter.
pub(crate) fn apply_filter(
    rows: &[BoardRow],
    filter: &TaskFilter,
    config: &ProjectConfig,
    actor: Actor,
) -> Vec<usize> {
    let text = filter.text.trim().to_lowercase();
    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            let front = &row.meta.front;
            filter.kind.as_ref().is_none_or(|kind| *kind == front.kind)
                && filter
                    .label
                    .as_ref()
                    .is_none_or(|label| front.labels.contains(label))
                && filter
                    .priority
                    .is_none_or(|priority| priority == front.priority)
                && (!filter.blocked_only || row.blocked)
                && (!filter.ready_only || is_ready(row, config, actor))
                && (text.is_empty()
                    || front.title.to_lowercase().contains(&text)
                    || front.id.matches_short(&text))
        })
        .map(|(index, _)| index)
        .collect()
}

/// A row of the task tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TreeRow {
    pub index: usize,
    pub depth: usize,
    pub has_children: bool,
    /// Tells if the task matches the filter. Other rows show only as parents of a match.
    pub matched: bool,
}

/// Flattens the task tree for a virtual list. A filter keeps a match and its ancestors.
pub(crate) fn flatten_tree(
    model: &BoardModel,
    matches: &[usize],
    collapsed: &HashSet<TaskId>,
) -> Vec<TreeRow> {
    let count = model.rows.len();
    let mut matched = vec![false; count];
    let mut keep = vec![false; count];
    for index in matches {
        matched[*index] = true;
        let mut current = Some(*index);
        while let Some(at) = current {
            if keep[at] {
                break;
            }
            keep[at] = true;
            current = model.rows[at]
                .meta
                .front
                .parent
                .and_then(|parent| model.by_id.get(&parent).copied());
        }
    }
    let mut out = Vec::with_capacity(matches.len());
    let mut seen = vec![false; count];
    let mut walk = |start: usize, out: &mut Vec<TreeRow>| {
        let mut stack = vec![(start, 0)];
        while let Some((index, depth)) = stack.pop() {
            if seen[index] || !keep[index] {
                continue;
            }
            seen[index] = true;
            let kids = model.children_of(index);
            let has_children = kids.iter().any(|kid| keep[*kid]);
            out.push(TreeRow {
                index,
                depth,
                has_children,
                matched: matched[index],
            });
            if !collapsed.contains(&model.rows[index].meta.id()) {
                for kid in kids.iter().rev() {
                    stack.push((*kid, depth + 1));
                }
            }
        }
    };
    for root in &model.roots {
        walk(*root, &mut out);
    }
    // A parent cycle from a Git merge has no root, so its tasks would stay hidden. A task below a
    // collapsed parent stays hidden with intent.
    let mut below_collapsed = vec![false; count];
    for (index, row) in model.rows.iter().enumerate() {
        if collapsed.contains(&row.meta.id()) {
            let mut stack: Vec<usize> = model.children_of(index).to_vec();
            while let Some(kid) = stack.pop() {
                if !below_collapsed[kid] {
                    below_collapsed[kid] = true;
                    stack.extend_from_slice(model.children_of(kid));
                }
            }
        }
    }
    for (index, hidden) in below_collapsed.iter().enumerate() {
        if !*hidden {
            walk(index, &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use lenka_core::{Stage, TypeDef};
    use lenka_plugin_api::Timestamp;
    use lenka_tasks::{State, TaskFront, TaskMeta};

    use super::*;

    fn row(title: &str, parent: Option<TaskId>) -> BoardRow {
        BoardRow {
            meta: TaskMeta {
                front: TaskFront {
                    id: TaskId::generate(),
                    kind: "task".to_owned(),
                    title: title.to_owned(),
                    parent,
                    blocked_by: Vec::new(),
                    relates: Vec::new(),
                    artifacts: Vec::new(),
                    priority: Priority::Normal,
                    labels: vec!["api".to_owned()],
                    created_by: "t".to_owned(),
                    created_at: Timestamp::from_unix_ms(0),
                    start: None,
                    due: None,
                    coordinated: false,
                },
                state: State {
                    version: 1,
                    status: Status::Todo,
                    stage: "work".to_owned(),
                    unblocked: false,
                    loop_stopped: false,
                    loop_until_done: false,
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

    fn config() -> ProjectConfig {
        let mut config = ProjectConfig::new("t");
        config.types.insert(
            "ops".to_owned(),
            TypeDef {
                stages: vec![Stage {
                    name: "deploy".to_owned(),
                    actor: Actor::Agent,
                    unblocks: false,
                }],
            },
        );
        config
    }

    #[test]
    fn ready_follows_status_blockers_children_and_actor() {
        let config = config();
        let mut item = row("A", None);
        assert!(is_ready(&item, &config, Actor::Human));
        item.blocked = true;
        assert!(!is_ready(&item, &config, Actor::Human));
        item.blocked = false;
        item.child_count = 2;
        item.closed_children = 1;
        assert!(!is_ready(&item, &config, Actor::Human));
        item.closed_children = 2;
        assert!(is_ready(&item, &config, Actor::Human));
        item.meta.state.status = Status::InProgress;
        assert!(is_ready(&item, &config, Actor::Human));
        item.claimed_by = Some("agent:a".to_owned());
        assert!(!is_ready(&item, &config, Actor::Human));
        item.claimed_by = None;
        item.meta.state.status = Status::Paused;
        assert!(!is_ready(&item, &config, Actor::Human));
        item.meta.state.status = Status::Todo;
        item.meta.front.kind = "ops".to_owned();
        item.meta.state.stage = "deploy".to_owned();
        assert!(
            !is_ready(&item, &config, Actor::Human),
            "a person cannot do an agent stage"
        );
        assert!(is_ready(&item, &config, Actor::Agent));
        item.meta.front.kind = "gone".to_owned();
        assert!(!is_ready(&item, &config, Actor::Agent));
    }

    #[test]
    fn filters_combine() {
        let config = config();
        let mut first = row("Parse the feed", None);
        first.meta.front.priority = Priority::High;
        let mut second = row("Write the feed", None);
        second.meta.front.labels = vec!["ui".to_owned()];
        second.blocked = true;
        let mut third = row("Other", None);
        third.meta.front.kind = "bug".to_owned();
        let short = third.meta.id().short();
        let rows = vec![first, second, third];
        let run = |filter: TaskFilter| apply_filter(&rows, &filter, &config, Actor::Human);
        assert_eq!(run(TaskFilter::default()), vec![0, 1, 2]);
        assert_eq!(
            run(TaskFilter {
                text: "FEED".to_owned(),
                ..TaskFilter::default()
            }),
            vec![0, 1]
        );
        assert_eq!(
            run(TaskFilter {
                text: short,
                ..TaskFilter::default()
            }),
            vec![2]
        );
        assert_eq!(
            run(TaskFilter {
                kind: Some("bug".to_owned()),
                ..TaskFilter::default()
            }),
            vec![2]
        );
        assert_eq!(
            run(TaskFilter {
                label: Some("ui".to_owned()),
                ..TaskFilter::default()
            }),
            vec![1]
        );
        assert_eq!(
            run(TaskFilter {
                priority: Some(Priority::High),
                ..TaskFilter::default()
            }),
            vec![0]
        );
        assert_eq!(
            run(TaskFilter {
                blocked_only: true,
                ..TaskFilter::default()
            }),
            vec![1]
        );
        assert_eq!(
            run(TaskFilter {
                ready_only: true,
                ..TaskFilter::default()
            }),
            vec![0, 2]
        );
        assert_eq!(
            run(TaskFilter {
                text: "feed".to_owned(),
                ready_only: true,
                ..TaskFilter::default()
            }),
            vec![0]
        );
    }

    #[test]
    fn tree_nests_collapses_and_keeps_parents_of_matches() {
        let root = row("root", None);
        let root_id = root.meta.id();
        let child = row("child", Some(root_id));
        let child_id = child.meta.id();
        let leaf = row("leaf", Some(child_id));
        let other = row("other", None);
        let model = BoardModel::new(vec![root, child, leaf, other]);
        let all: Vec<usize> = (0..4).collect();
        let flat = flatten_tree(&model, &all, &HashSet::new());
        let order: Vec<(usize, usize)> = flat.iter().map(|row| (row.index, row.depth)).collect();
        assert_eq!(order, vec![(0, 0), (1, 1), (2, 2), (3, 0)]);
        assert!(flat[0].has_children);
        assert!(!flat[2].has_children);

        let collapsed = HashSet::from([child_id]);
        let flat = flatten_tree(&model, &all, &collapsed);
        let order: Vec<usize> = flat.iter().map(|row| row.index).collect();
        assert_eq!(
            order,
            vec![0, 1, 3],
            "the leaf stays hidden below its collapsed parent"
        );

        let flat = flatten_tree(&model, &[2], &HashSet::new());
        let order: Vec<(usize, bool)> = flat.iter().map(|row| (row.index, row.matched)).collect();
        assert_eq!(order, vec![(0, false), (1, false), (2, true)]);
    }

    #[test]
    fn tree_shows_a_parent_cycle() {
        let mut first = row("a", None);
        let second = row("b", Some(first.meta.id()));
        first.meta.front.parent = Some(second.meta.id());
        let model = BoardModel::new(vec![first, second]);
        let flat = flatten_tree(&model, &[0, 1], &HashSet::new());
        assert_eq!(flat.len(), 2);
    }
}
