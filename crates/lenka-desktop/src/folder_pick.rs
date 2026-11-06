// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The native folder dialog of the OS.
//!
//! The dialog does not stop the UI thread. The result arrives in a [`PendingFolder`], and the thread
//! asks for a repaint when the person closes the dialog. Tests never open a real dialog: they call
//! [`PendingFolder::finish`] directly.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

/// The state of a folder dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum FolderPick {
    /// The dialog is open.
    #[default]
    Waiting,
    /// The person closed the dialog with no selection, or the OS has no dialog.
    Cancelled,
    /// The person selected this folder.
    Selected(PathBuf),
}

/// A folder dialog that can still be open. Two clones share one state.
#[derive(Debug, Clone, Default)]
pub(crate) struct PendingFolder(Arc<Mutex<FolderPick>>);

impl PendingFolder {
    /// Returns the current state.
    pub(crate) fn state(&self) -> FolderPick {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Sets the result of the dialog.
    pub(crate) fn finish(&self, pick: FolderPick) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = pick;
    }
}

/// Opens the native folder dialog. It starts in the folder `current`, if that folder exists.
pub(crate) fn start(ctx: &egui::Context, current: &str) -> PendingFolder {
    let pending = PendingFolder::default();
    #[cfg(not(test))]
    open_native(ctx, current, &pending);
    #[cfg(test)]
    let _ = (ctx, current);
    pending
}

#[cfg(not(test))]
fn open_native(ctx: &egui::Context, current: &str, pending: &PendingFolder) {
    let mut dialog = rfd::AsyncFileDialog::new().set_title("Select the data directory");
    let folder = std::path::Path::new(current.trim());
    if folder.is_absolute() && folder.is_dir() {
        dialog = dialog.set_directory(folder);
    }
    // On macOS, a dialog must start on the UI thread, so only the wait goes to the other thread.
    let future = dialog.pick_folder();
    let result = pending.clone();
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("lenka-folder-dialog".to_owned())
        .spawn(move || {
            let pick = pollster::block_on(future).map_or(FolderPick::Cancelled, |handle| {
                FolderPick::Selected(handle.path().to_path_buf())
            });
            result.finish(pick);
            ctx.request_repaint();
        });
    if spawned.is_err() {
        pending.finish(FolderPick::Cancelled);
    }
}
