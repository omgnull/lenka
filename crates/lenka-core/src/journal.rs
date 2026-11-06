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
        apply(txn, op, None)?;
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
    for op in &ops {
        apply(txn, op, Some(driver)).map_err(|err| {
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

fn apply(txn: &mut dyn WriteTxn, op: &Op, replay: Option<&dyn StorageDriver>) -> Result<()> {
    match op {
        Op::Put { path, text } => txn.put(&DocPath::new(path.clone())?, text.as_bytes()),
        Op::Append { path, line } => {
            let doc = DocPath::new(path.clone())?;
            if let Some(driver) = replay
                && ends_with_line(driver, &doc, line)?
            {
                return Ok(());
            }
            let mut bytes = line.clone().into_bytes();
            bytes.push(b'\n');
            txn.append(&doc, &bytes)
        }
        Op::Remove { path } => txn.remove(&DocPath::new(path.clone())?),
        Op::RemoveTree { path } => txn.remove_tree(&DocPath::new(path.clone())?),
    }
}

fn ends_with_line(driver: &dyn StorageDriver, doc: &DocPath, line: &str) -> Result<bool> {
    let Some(stat) = driver.stat(doc)? else {
        return Ok(false);
    };
    let wanted = line.len() as u64 + 1;
    if stat.len < wanted {
        return Ok(false);
    }
    let tail = driver.read_from(doc, stat.len - wanted)?;
    Ok(tail.strip_suffix(b"\n") == Some(line.as_bytes()))
}
