// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The write journal. It makes a group of document changes safe against a crash.
//!
//! A write stores all its operations in the journal, and flushes the journal to the disk. It then
//! applies the operations in order, and empties the journal. The next lock holder replays a
//! journal that it finds. All operations are safe to apply two times.
//!
//! The one flush gives the time limit of a write. A process crash cannot lose a completed write
//! on any file system. After a power failure, a file system that keeps the order of writes, such
//! as ext4 with `data=ordered`, also loses nothing. Another file system can lose the newest
//! writes, so a project can set `strict_flush` in `lenka.toml`, which flushes each file.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use lenka_plugin_api::{DocPath, LenkaError, Result, StorageDriver, WriteTxn};
use serde::{Deserialize, Serialize};

/// One document change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Replace a document.
    Put {
        /// The document path.
        path: String,
        /// The new text.
        text: String,
    },
    /// Add a line to an append-only document.
    Append {
        /// The document path.
        path: String,
        /// The line, with no line end.
        line: String,
    },
    /// Remove a document.
    Remove {
        /// The document path.
        path: String,
    },
    /// Remove a directory.
    RemoveTree {
        /// The directory path.
        path: String,
    },
}

impl Op {
    fn path(&self) -> &str {
        match self {
            Self::Put { path, .. }
            | Self::Append { path, .. }
            | Self::Remove { path }
            | Self::RemoveTree { path } => path,
        }
    }
}

/// A test control: the journal stops after this count of operations, with no cleanup.
/// This has the same effect on the disk as a process that stops at that step.
#[derive(Debug, Default)]
pub struct CrashPoint(AtomicUsize);

impl CrashPoint {
    /// Sets the count of operations to apply before the stop. The value 1 stops after the journal
    /// file exists and before any operation. The value 0 turns the control off.
    pub fn set(&self, steps: usize) {
        self.0.store(steps, Ordering::SeqCst);
    }

    fn should_stop(&self) -> bool {
        let value = self.0.load(Ordering::SeqCst);
        if value == 0 {
            return false;
        }
        self.0.store(value - 1, Ordering::SeqCst);
        value == 1
    }
}

/// Writes the journal, applies all operations, and removes the journal.
///
/// # Errors
///
/// Returns an error if a step fails. The next lock holder replays the journal.
pub fn run(txn: &mut dyn WriteTxn, ops: &[Op], crash: &CrashPoint) -> Result<()> {
    let text = serde_json::to_vec(ops)
        .map_err(|err| LenkaError::invalid_file(format!("Journal: {err}")))?;
    // A failed check here changes nothing. A failure after the journal write would repeat at each
    // replay, and it would stop all later writes.
    for op in ops {
        let path = DocPath::new(op.path())?;
        match op {
            Op::Put { .. } | Op::Append { .. } => txn.check_file_path(&path)?,
            Op::Remove { .. } | Op::RemoveTree { .. } => txn.check_path(&path)?,
        }
    }
    txn.journal_write(&text)?;
    for op in ops {
        if crash.should_stop() {
            return Err(LenkaError::new(
                lenka_plugin_api::ErrorCode::Io,
                "Test stop in a write.",
            ));
        }
        apply(txn, op)?;
    }
    if crash.should_stop() {
        return Err(LenkaError::new(
            lenka_plugin_api::ErrorCode::Io,
            "Test stop in a write.",
        ));
    }
    txn.journal_clear()
}

/// Replays a journal that a stopped process left. The caller holds the lock.
///
/// # Errors
///
/// Returns an error if the journal is not valid or a step fails.
pub fn repair(driver: &dyn StorageDriver, txn: &mut dyn WriteTxn) -> Result<bool> {
    let Some(bytes) = driver.journal_read()? else {
        return Ok(false);
    };
    let Ok(ops) = serde_json::from_slice::<Vec<Op>>(&bytes) else {
        // A process stopped in the journal write. No operation applied, so nothing changed.
        tracing::warn!("ignored an incomplete write journal");
        txn.journal_clear()?;
        return Ok(false);
    };
    // `run` checks each path before the journal write, so this journal does not come from lenka.
    if ops.iter().any(|op| DocPath::new(op.path()).is_err()) {
        tracing::warn!("ignored a write journal with a path that is not valid");
        txn.journal_clear()?;
        return Ok(false);
    }
    let mut appends = replay_plan(driver, &ops)?;
    for op in &ops {
        apply_replay(txn, op, &mut appends).map_err(|err| {
            LenkaError::new(
                err.code(),
                format!(
                    "lenka cannot finish an unfinished write at {}: {err}. Repair that path, and run the command again.",
                    op.path()
                ),
            )
        })?;
    }
    txn.journal_clear()?;
    tracing::info!(operations = ops.len(), "repaired an unfinished write");
    Ok(true)
}

fn apply(txn: &mut dyn WriteTxn, op: &Op) -> Result<()> {
    match op {
        Op::Put { path, text } => txn.put(&DocPath::new(path.clone())?, text.as_bytes()),
        Op::Append { path, line } => {
            let mut bytes = line.clone().into_bytes();
            bytes.push(b'\n');
            txn.append(&DocPath::new(path.clone())?, &bytes)
        }
        Op::Remove { path } => txn.remove(&DocPath::new(path.clone())?),
        Op::RemoveTree { path } => txn.remove_tree(&DocPath::new(path.clone())?),
    }
}

/// What a replay must do for one file that gets lines.
#[derive(Debug, Default)]
struct AppendPlan {
    /// The count of lines that the stopped write added.
    skip: usize,
    /// Tells if the file ends in a part of a line. A new line must not join it.
    needs_line_end: bool,
}

fn apply_replay(
    txn: &mut dyn WriteTxn,
    op: &Op,
    plans: &mut HashMap<String, AppendPlan>,
) -> Result<()> {
    if let Op::Append { path, .. } = op
        && let Some(plan) = plans.get_mut(path)
    {
        if plan.skip > 0 {
            plan.skip -= 1;
            return Ok(());
        }
        if plan.needs_line_end {
            plan.needs_line_end = false;
            txn.append(&DocPath::new(path.clone())?, b"\n")?;
        }
    }
    apply(txn, op)
}

/// Compares each file that gets lines with the lines of the journal.
fn replay_plan(driver: &dyn StorageDriver, ops: &[Op]) -> Result<HashMap<String, AppendPlan>> {
    let mut lines: HashMap<&str, Vec<&str>> = HashMap::new();
    for op in ops {
        if let Op::Append { path, line } = op {
            lines.entry(path).or_default().push(line);
        }
    }
    let mut plans = HashMap::with_capacity(lines.len());
    for (path, wanted) in lines {
        let doc = DocPath::new(path)?;
        let Some(stat) = driver.stat(&doc)? else {
            continue;
        };
        // A torn line is shorter than the line in the journal, so two times the size is enough.
        let size: u64 = wanted.iter().map(|line| line.len() as u64 + 1).sum();
        let tail = driver.read_from(&doc, stat.len.saturating_sub(size.saturating_mul(2)))?;
        plans.insert(path.to_owned(), plan_for_tail(&tail, &wanted));
    }
    Ok(plans)
}

/// Finds the lines that a file already has. The file can end in lines of the journal, in order,
/// and after them in a part of a line.
fn plan_for_tail(tail: &[u8], wanted: &[&str]) -> AppendPlan {
    let whole = tail
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(&tail[..0], |end| &tail[..=end]);
    let skip = (0..=wanted.len())
        .rev()
        .find(|count| {
            let mut expected = Vec::new();
            for line in &wanted[..*count] {
                expected.extend_from_slice(line.as_bytes());
                expected.push(b'\n');
            }
            whole.ends_with(&expected)
        })
        .unwrap_or(0);
    AppendPlan {
        skip,
        needs_line_end: tail.last().is_some_and(|byte| *byte != b'\n'),
    }
}
