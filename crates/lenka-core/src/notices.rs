// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The change feed and the inboxes.
//!
//! A notice is data. It does not tell an agent what to do. Without this rule, one compromised
//! agent can steer the others.

use crate::input::{MAX_REASON_BYTES, MAX_SHORT_BYTES, clip};
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

/// The part of an inbox that lenka reads.
const MAX_INBOX_READ_BYTES: u64 = 8 * 1024 * 1024;

/// The largest count of notices in one reply.
pub const MAX_NOTICES_IN_REPLY: usize = 50;

/// The largest size of the text of the notices in one reply.
pub const MAX_NOTICE_BYTES_IN_REPLY: usize = 64 * 1024;

/// The largest count of changed parts of a notice in one reply.
const MAX_CHANGED_IN_REPLY: usize = 32;

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

/// Calculates the identity of a person in a worktree. It differs from each agent key of that
/// worktree, so a person gets no notices of an agent, and a read by a person closes no notices of an
/// agent.
#[must_use]
pub fn person_key(worktree: &Path) -> AgentKey {
    let mut hasher = Sha256::new();
    hasher.update(worktree.as_os_str().as_encoded_bytes());
    // An agent name comes from the environment, so it cannot hold a zero byte.
    hasher.update([0, 0]);
    hasher.update(b"person");
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
    let path = inbox_path(key)?;
    let Some(stat) = driver.stat(&path)? else {
        return Ok(Vec::new());
    };
    if stat.len <= MAX_INBOX_READ_BYTES {
        return Ok(driver
            .read(&path)?
            .map_or_else(Vec::new, |bytes| parse_inbox(&bytes)));
    }
    // A very large inbox must not stop the work of an agent, so only the newest notices count.
    let bytes = driver.read_from(&path, stat.len - MAX_INBOX_READ_BYTES)?;
    let start = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |index| index + 1);
    Ok(parse_inbox(&bytes[start..]))
}

/// Adds the open notices of a caller to a tool result. Each author text is in `untrusted_data`.
pub fn add_to_reply(object: &mut serde_json::Map<String, serde_json::Value>, unread: &[Notice]) {
    if unread.is_empty() {
        return;
    }
    let reply = for_reply(unread);
    if reply.omitted > 0 {
        object.insert(
            "unread_changes_omitted".to_owned(),
            serde_json::json!(reply.omitted),
        );
    }
    let rows: Vec<serde_json::Value> = reply
        .shown
        .iter()
        .map(|notice| {
            let (kind, id) = match &notice.subject {
                Subject::Task(id) => ("task", id),
                Subject::Artifact(id) => ("artifact", id),
            };
            serde_json::json!({
                "kind": kind,
                "id": id,
                "version": notice.version,
                "for_claim_on": notice.claimed_task,
                "untrusted_data": {
                    "author": notice.author,
                    "reason": notice.reason,
                    "changed": notice.changed,
                },
            })
        })
        .collect();
    object.insert("unread_changes".to_owned(), serde_json::Value::Array(rows));
}

/// The notices of one reply, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplyNotices {
    /// The notices for this reply, with short text.
    pub shown: Vec<Notice>,
    /// The count of other open notices. They come in a later reply, after the agent reads these.
    pub omitted: usize,
}

/// Selects the notices for one reply. A limit on the count and on the bytes keeps each reply small,
/// so that one peer cannot make the replies of an agent too large for its client.
#[must_use]
pub fn for_reply(notices: &[Notice]) -> ReplyNotices {
    let mut shown = Vec::new();
    let mut bytes = 0;
    for notice in notices.iter().take(MAX_NOTICES_IN_REPLY) {
        let mut short = notice.clone();
        short.reason = clip(&notice.reason, MAX_REASON_BYTES).to_owned();
        short.author = clip(&notice.author, MAX_SHORT_BYTES).to_owned();
        short.changed.truncate(MAX_CHANGED_IN_REPLY);
        bytes += short.reason.len() + short.author.len();
        if bytes > MAX_NOTICE_BYTES_IN_REPLY && !shown.is_empty() {
            break;
        }
        shown.push(short);
    }
    ReplyNotices {
        omitted: notices.len() - shown.len(),
        shown,
    }
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
