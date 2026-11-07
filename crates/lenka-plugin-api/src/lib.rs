// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Types and traits that lenka plugins implement. This crate does no I/O.

mod caller;
mod driver;
mod error;
mod ids;
mod time_types;
mod tool;

pub use caller::{Actor, AgentKey, Caller, ProjectScope};
pub use driver::{ArtifactStore, DocPath, DocStat, StorageDriver, StoredBytes, WriteTxn};
pub use error::{ErrorCode, LenkaError, Result};
pub use ids::{ArtifactId, EntryId, MemoryId, NoticeId, ProjectId, TaskId, check_short_id};
pub use time_types::{Day, Timestamp};
pub use tool::ToolDef;
