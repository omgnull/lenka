// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The plugin host of lenka: settings, project lookup, checks, journal, and change notices.
//!
//! The secret check and the version check run in [`Locked::commit`], so a plugin cannot skip
//! them by accident.

pub mod clock;
pub mod gitdir;
pub mod host;
pub mod init;
pub mod input;
pub mod journal;
pub mod layout;
pub mod notices;
pub mod project;
pub mod secret;
pub mod settings;

pub use clock::{Clock, FixedClock, SystemClock};
pub use host::{
    Builder, EventHook, Host, PeerLookup, Peers, Plugin, PluginInfo, PluginState, Registry,
    ToolContext, ToolProvider,
};
pub use journal::Op;
pub use lenka_plugin_api as api;
pub use notices::{FeedEvent, FeedKind, Notice, ProjectEvent, Subject};
pub use project::{Commit, LEFTOVER_AGE, Locked, Project, is_leftover};
pub use secret::{SecretPolicy, TextCheck};
pub use settings::{
    Paths, ProjectConfig, ProjectEntry, ProjectsFile, Stage, TypeDef, unknown_sections,
};
