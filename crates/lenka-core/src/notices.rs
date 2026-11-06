// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The change feed and the inboxes.
//!
//! A notice is data. It does not tell an agent what to do. Without this rule, one compromised
//! agent can steer the others.

use std::path::Path;

use lenka_plugin_api::{AgentKey, DocPath, NoticeId, Result, StorageDriver, Timestamp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The path of the change feed.
pub const FEED_PATH: &str = ".runtime/feed.jsonl";

/// The thing that changed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Subject {
    /// A task, by its full ID.
    Task(String),
    /// An artifact, by its full ID.
    Artifact(String),
}

/// A change notice in an inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    /// The notice ID. These IDs are time-ordered.
    pub id: NoticeId,
    /// The thing that changed.
    pub subject: Subject,
    /// The new version.
    pub version: u64,
    /// The changed parts.
    pub changed: Vec<String>,
    /// The author label.
    pub author: String,
    /// The reason that the author gave.
    pub reason: String,
    /// The time of the change.
    pub at: Timestamp,
    /// The claim handle that this notice applies to.
    pub claim: String,
    /// The claimed task, by its full ID.
    pub claimed_task: String,
}

/// The kind of a feed event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedKind {
    /// The task text or fields changed.
    Task,
    /// The task status or stage changed.
    State,
    /// A claim changed.
    Claim,
    /// An entry was added.
    Entry,
    /// The task was deleted.
    TaskDeleted,
    /// The artifact changed.
    Artifact,
    /// The artifact was deleted.
    ArtifactDeleted,
    /// A shared memory record changed.
    Memory,
    /// Many files can have changed, for example after a Git pull.
    Reindex,
}

/// One line of the change feed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedEvent {
    /// The kind of change.
    pub kind: FeedKind,
    /// The full ID of the changed thing. A reindex event has an empty ID.
    pub id: String,
    /// The version after the change.
    pub version: u64,
    /// The author label.
    pub author: String,
    /// The reason for a material change.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// The changed parts of a material change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<String>,
    /// The time of the change.
    pub at: Timestamp,
}

/// An event for plugins, after a write completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectEvent {
    /// A stage completed or moved back.
    StageChanged {
        /// The full task ID.
        task: String,
    },
    /// A task finished with an outcome.
    TaskFinished {
        /// The full task ID.
        task: String,
    },
}

/// Calculates the inbox identity of a worktree and an optional agent name.
#[must_use]
pub fn agent_key(worktree: &Path, agent: Option<&str>) -> AgentKey {
    let mut hasher = Sha256::new();
    hasher.update(worktree.as_os_str().as_encoded_bytes());
    hasher.update([0]);
    hasher.update(agent.unwrap_or("").as_bytes());
    let digest = hasher.finalize();
    AgentKey(hex(&digest[..12]))
}

/// Formats bytes in lowercase hexadecimal.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Returns the path of an inbox.
///
/// # Errors
///
/// Returns an error if the key is not a valid path part.
pub fn inbox_path(key: &AgentKey) -> Result<DocPath> {
    DocPath::new(format!(".runtime/inbox/{}.jsonl", key.0))
}

/// Reads the open notices of an inbox, oldest first. A bad line is skipped, because an inbox is
/// runtime data that lenka can lose with no harm to the tasks.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub fn read_inbox(driver: &dyn StorageDriver, key: &AgentKey) -> Result<Vec<Notice>> {
    let Some(bytes) = driver.read(&inbox_path(key)?)? else {
        return Ok(Vec::new());
    };
    Ok(parse_inbox(&bytes))
}

/// Parses inbox bytes. A bad line is skipped.
#[must_use]
pub fn parse_inbox(bytes: &[u8]) -> Vec<Notice> {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Formats an inbox file from notices.
///
/// # Errors
///
/// Returns an error if a notice cannot be formatted.
pub fn format_inbox(notices: &[Notice]) -> Result<String> {
    let mut text = String::new();
    for notice in notices {
        text.push_str(&format_line(notice)?);
        text.push('\n');
    }
    Ok(text)
}

/// Formats one JSON line.
///
/// # Errors
///
/// Returns an error if the value cannot be formatted.
pub fn format_line(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|err| lenka_plugin_api::LenkaError::invalid_file(format!("JSON: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_key_separates_worktrees_and_agents() {
        let first = agent_key(Path::new("/a"), None);
        assert_eq!(first, agent_key(Path::new("/a"), None));
        assert_ne!(first, agent_key(Path::new("/b"), None));
        assert_ne!(first, agent_key(Path::new("/a"), Some("x")));
        assert!(DocPath::new(format!("x/{}", first.0)).is_ok());
    }

    #[test]
    fn a_bad_inbox_line_is_skipped() -> Result<()> {
        let notice = Notice {
            id: NoticeId::generate(),
            subject: Subject::Task("t".to_owned()),
            version: 2,
            changed: vec!["plan".to_owned()],
            author: "agent:a".to_owned(),
            reason: "r".to_owned(),
            at: Timestamp::from_unix_ms(1),
            claim: "c".to_owned(),
            claimed_task: "t".to_owned(),
        };
        let text = format!("not json\n{}", format_inbox(std::slice::from_ref(&notice))?);
        assert_eq!(parse_inbox(text.as_bytes()), vec![notice]);
        Ok(())
    }
}
