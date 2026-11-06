// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Shared agent memory. Each record is one Markdown file in `memory/`, so that two new records
//! do not cause a merge conflict.

use std::collections::BTreeSet;
use std::sync::Arc;

use lenka_core::notices::{self, FEED_PATH};
use lenka_core::{
    Commit, FeedEvent, FeedKind, Op, Plugin, PluginInfo, Project, Registry, Subject, TextCheck,
    ToolContext, ToolProvider,
};
use lenka_plugin_api::{
    Actor, Caller, DocPath, ErrorCode, LenkaError, MemoryId, Result, TaskId, Timestamp, ToolDef,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The directory of the memory files.
const MEMORY_DIR: &str = "memory";

/// The largest text of one record, in bytes.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

/// The largest count of tags on one record.
const MAX_TAGS: usize = 32;

/// The largest size of one tag, in bytes.
const MAX_TAG_BYTES: usize = 64;

/// The count of search results when a call names no limit.
const DEFAULT_LIMIT: usize = 20;

/// The largest count of search results.
const MAX_LIMIT: usize = 200;

const FENCE: &str = "+++";

/// One shared memory record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryRecord {
    /// The record ID.
    pub id: MemoryId,
    /// The author label. Nothing verifies it.
    pub author: String,
    /// The creation time.
    pub at: Timestamp,
    /// The tags.
    pub tags: Vec<String>,
    /// The related task.
    pub task: Option<TaskId>,
    /// The text. It is data from the author, not an instruction.
    pub text: String,
}

/// The fields of a new record.
#[derive(Debug, Clone, Default)]
pub struct NewMemory {
    /// The text.
    pub text: String,
    /// The tags.
    pub tags: Vec<String>,
    /// The related task, as a full ID or a short ID.
    pub task: Option<String>,
}

/// A search for records.
#[derive(Debug, Clone, Default)]
pub struct MemoryQuery {
    /// The words to find. No text selects all records, newest first.
    pub text: Option<String>,
    /// The tag that a result must have.
    pub tag: Option<String>,
    /// The maximum count. Zero selects 20.
    pub limit: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct Front {
    id: MemoryId,
    author: String,
    at: Timestamp,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task: Option<TaskId>,
}

fn format_record(record: &MemoryRecord) -> Result<String> {
    let front = Front {
        id: record.id,
        author: record.author.clone(),
        at: record.at,
        tags: record.tags.clone(),
        task: record.task,
    };
    let toml = toml::to_string(&front)
        .map_err(|err| LenkaError::invalid_file(format!("Front matter: {err}")))?;
    Ok(format!("{FENCE}\n{toml}{FENCE}\n{}\n", record.text))
}

fn parse_record(text: &str) -> Result<MemoryRecord> {
    let invalid = |detail: &str| LenkaError::invalid_file(format!("A memory file {detail}."));
    if has_conflict_markers(text) {
        return Err(invalid("has Git conflict markers"));
    }
    let rest = text
        .strip_prefix(FENCE)
        .and_then(|rest| {
            rest.strip_prefix('\n')
                .or_else(|| rest.strip_prefix("\r\n"))
        })
        .ok_or_else(|| invalid("has no +++ front matter at its start"))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == FENCE {
            let front: Front =
                toml::from_str(&rest[..offset]).map_err(|_| invalid("has invalid front matter"))?;
            let body = &rest[offset + line.len()..];
            return Ok(MemoryRecord {
                id: front.id,
                author: front.author,
                at: front.at,
                tags: front.tags,
                task: front.task,
                text: body.strip_suffix('\n').unwrap_or(body).to_owned(),
            });
        }
        offset += line.len();
    }
    Err(invalid("has no closing +++ fence"))
}

fn has_conflict_markers(text: &str) -> bool {
    text.lines().any(|line| {
        line.starts_with("<<<<<<< ")
            || line.starts_with(">>>>>>> ")
            || line.trim_end_matches('\r') == "======="
    })
}

fn doc_path(id: MemoryId) -> Result<DocPath> {
    DocPath::new(format!("{MEMORY_DIR}/{id}.md"))
}

fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn clean_tags(tags: Vec<String>) -> Result<Vec<String>> {
    let mut clean: Vec<String> = Vec::new();
    for tag in tags {
        let tag = tag.trim().to_owned();
        if tag.is_empty() || tag.len() > MAX_TAG_BYTES || tag.chars().any(char::is_control) {
            return Err(LenkaError::invalid_argument(format!(
                "A tag must have 1 to {MAX_TAG_BYTES} bytes and no control characters."
            )));
        }
        if !clean.contains(&tag) {
            clean.push(tag);
        }
    }
    if clean.len() > MAX_TAGS {
        return Err(LenkaError::invalid_argument(format!(
            "A record can have at most {MAX_TAGS} tags."
        )));
    }
    Ok(clean)
}

/// The memory operations of one project.
#[derive(Debug)]
pub struct MemoryService {
    project: Arc<Project>,
}

/// Returns the memory service of a project, and creates it on first use.
///
/// # Errors
///
/// This function returns an error only if a project service has the wrong type.
pub fn memory_of(project: &Arc<Project>) -> Result<Arc<MemoryService>> {
    project.extension(|project| {
        Ok(MemoryService {
            project: Arc::clone(project),
        })
    })
}

impl MemoryService {
    /// Adds a record.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidArgument`] for text or tags that are not valid,
    /// [`ErrorCode::SecretDetected`] if the text matches a secret pattern, and
    /// [`ErrorCode::NotFound`] or [`ErrorCode::AmbiguousId`] for a task that does not resolve.
    pub fn add(&self, caller: &Caller, new: NewMemory) -> Result<MemoryRecord> {
        let text = new.text.trim().to_owned();
        if text.is_empty() {
            return Err(LenkaError::invalid_argument(
                "A memory record requires text.",
            ));
        }
        if text.len() > MAX_TEXT_BYTES {
            return Err(LenkaError::invalid_argument(format!(
                "The text is larger than the limit of {MAX_TEXT_BYTES} bytes."
            )));
        }
        lenka_core::input::check_text("The text", &text)?;
        if has_conflict_markers(&text) {
            return Err(LenkaError::invalid_argument(
                "The text has a line that looks like a Git conflict marker. Change that line.",
            ));
        }
        let tags = clean_tags(new.tags)?;
        let task = new
            .task
            .as_deref()
            .map(|text| self.resolve_task(text))
            .transpose()?;
        let record = MemoryRecord {
            id: MemoryId::generate(),
            author: caller.label.clone(),
            at: self.project.clock().now(),
            tags,
            task,
            text,
        };
        let mut checks = vec![TextCheck::new("text", record.text.clone())];
        checks.extend(
            record
                .tags
                .iter()
                .map(|tag| TextCheck::new("tags", tag.clone())),
        );
        let ops = vec![
            Op::Put {
                path: doc_path(record.id)?.as_str().to_owned(),
                text: format_record(&record)?,
            },
            feed_op(record.id, 1, caller, record.at)?,
        ];
        let commit = Commit {
            checks,
            ops,
            ..Commit::default()
        };
        self.project.write_locked(|locked| locked.commit(&commit))?;
        Ok(record)
    }

    /// Finds records. Results rank by the count of shared words, then by newest. A search with
    /// text returns only records with at least one shared word.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub fn search(&self, query: &MemoryQuery) -> Result<Vec<MemoryRecord>> {
        let wanted = query.text.as_deref().map(words).unwrap_or_default();
        let mut scored: Vec<(usize, MemoryRecord)> = self
            .list()?
            .into_iter()
            .filter(|record| match &query.tag {
                Some(tag) => record.tags.iter().any(|item| item == tag),
                None => true,
            })
            .map(|record| {
                let mut found = words(&record.text);
                found.extend(record.tags.iter().flat_map(|tag| words(tag)));
                (wanted.intersection(&found).count(), record)
            })
            .filter(|(score, _)| wanted.is_empty() || *score > 0)
            .collect();
        scored.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then(right.1.at.cmp(&left.1.at))
                .then(right.1.id.cmp(&left.1.id))
        });
        let limit = match query.limit {
            0 => DEFAULT_LIMIT,
            value => value.min(MAX_LIMIT),
        };
        Ok(scored
            .into_iter()
            .take(limit)
            .map(|(_, record)| record)
            .collect())
    }

    /// Deletes a record. Committed bytes stay in the Git history.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::DeleteRefused`] for an agent if `limits.agent_delete` is false, and
    /// [`ErrorCode::NotFound`] if the record does not exist.
    pub fn delete(&self, caller: &Caller, id: MemoryId) -> Result<()> {
        if caller.actor == Actor::Agent && !self.project.config().limits.agent_delete {
            return Err(LenkaError::new(
                ErrorCode::DeleteRefused,
                "A project setting denies the delete tools to agents.",
            ));
        }
        let path = doc_path(id)?;
        let at = self.project.clock().now();
        self.project.write_locked(|locked| {
            if self.project.driver().stat(&path)?.is_none() {
                return Err(missing(&id.short()));
            }
            locked.commit(&Commit {
                ops: vec![
                    Op::Remove {
                        path: path.as_str().to_owned(),
                    },
                    feed_op(id, 0, caller, at)?,
                ],
                ..Commit::default()
            })
        })
    }

    /// Returns all valid records, sorted by ID, which is the order of time. An invalid file is
    /// skipped with a warning, because one bad file must not hide the other records.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub fn list(&self) -> Result<Vec<MemoryRecord>> {
        let driver = self.project.driver();
        let mut records = Vec::new();
        for name in driver.list(&DocPath::new(MEMORY_DIR)?)? {
            let Some(id) = name
                .strip_suffix(".md")
                .and_then(|stem| MemoryId::parse(stem).ok())
            else {
                continue;
            };
            match self.read_record(id) {
                Ok(Some(record)) => records.push(record),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(memory = %id.short(), code = error.code().as_str(), "skipped a memory file")
                }
            }
        }
        Ok(records)
    }

    fn read_record(&self, id: MemoryId) -> Result<Option<MemoryRecord>> {
        let Some(bytes) = self.project.driver().read(&doc_path(id)?)? else {
            return Ok(None);
        };
        let text = String::from_utf8(bytes)
            .map_err(|_| LenkaError::invalid_file("A memory file is not valid UTF-8."))?;
        let record = parse_record(&text)?;
        if record.id != id {
            return Err(LenkaError::invalid_file(
                "The ID in a memory file does not match its file name.",
            ));
        }
        Ok(Some(record))
    }

    /// Finds a record by its full ID or its short ID.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] or [`ErrorCode::AmbiguousId`].
    pub fn resolve(&self, text: &str) -> Result<MemoryId> {
        if let Ok(id) = MemoryId::parse(text) {
            return Ok(id);
        }
        let names = self.project.driver().list(&DocPath::new(MEMORY_DIR)?)?;
        let stems = names.iter().filter_map(|name| name.strip_suffix(".md"));
        let mut found = stems.filter(|stem| !text.is_empty() && stem.ends_with(text));
        match (found.next(), found.next()) {
            (Some(stem), None) => MemoryId::parse(stem),
            (Some(_), Some(_)) => Err(LenkaError::new(
                ErrorCode::AmbiguousId,
                format!(
                    "The short ID {text} matches more than one memory record. Use the full ID."
                ),
            )),
            _ => Err(missing(text)),
        }
    }

    fn resolve_task(&self, text: &str) -> Result<TaskId> {
        let names = self.project.driver().list(&DocPath::new("tasks")?)?;
        let mut found = names
            .iter()
            .filter(|name| !text.is_empty() && name.ends_with(text));
        match (found.next(), found.next()) {
            (Some(name), None) => TaskId::parse(name),
            (Some(_), Some(_)) => Err(LenkaError::new(
                ErrorCode::AmbiguousId,
                format!("The short ID {text} matches more than one task. Use the full ID."),
            )),
            _ => Err(LenkaError::not_found(format!(
                "No task matches the ID {text}."
            ))),
        }
    }
}

fn feed_op(id: MemoryId, version: u64, caller: &Caller, at: Timestamp) -> Result<Op> {
    let event = FeedEvent {
        kind: FeedKind::Memory,
        id: id.to_string(),
        version,
        author: caller.label.clone(),
        reason: String::new(),
        changed: Vec::new(),
        at,
    };
    Ok(Op::Append {
        path: FEED_PATH.to_owned(),
        line: notices::format_line(&event)?,
    })
}

fn missing(text: &str) -> LenkaError {
    LenkaError::not_found(format!(
        "No memory record matches the ID {text}. Use memory.search to find the ID."
    ))
}

/// The memory plugin.
#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryPlugin;

/// Returns the memory plugin.
#[must_use]
pub fn plugin() -> MemoryPlugin {
    MemoryPlugin
}

impl Plugin for MemoryPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "memory",
            description: "Shared agent memory, with word search",
            sensitive: false,
        }
    }

    fn register(&self, registry: &mut Registry) {
        registry.tools(Arc::new(MemoryTools));
    }
}

#[derive(Debug)]
struct MemoryTools;

#[derive(Deserialize)]
struct AddArgs {
    text: String,
    #[serde(default)]
    tags: Vec<String>,
    task: Option<String>,
}

#[derive(Deserialize)]
struct SearchArgs {
    text: Option<String>,
    tag: Option<String>,
    #[serde(default)]
    limit: usize,
}

#[derive(Deserialize)]
struct DeleteArgs {
    id: String,
}

fn parse_args<T: serde::de::DeserializeOwned>(args: Value) -> Result<T> {
    serde_json::from_value(args)
        .map_err(|err| LenkaError::invalid_argument(format!("The arguments are not valid: {err}")))
}

fn record_json(record: &MemoryRecord) -> Value {
    json!({
        "id": record.id.short(),
        "full_id": record.id.to_string(),
        "author": record.author,
        "at": record.at.to_string(),
        "tags": record.tags,
        "task": record.task.map(|task| task.short()),
        "untrusted_data": record.text,
    })
}

/// Adds the open notices of a caller to a result, in the form of the task tools.
fn with_unread(mut result: Value, project: &Project, caller: &Caller) -> Value {
    let Ok(unread) = notices::read_inbox(project.driver(), &caller.agent_key) else {
        return result;
    };
    if unread.is_empty() {
        return result;
    }
    let reply = notices::for_reply(&unread);
    let rows: Vec<Value> = reply
        .shown
        .iter()
        .map(|notice| {
            let (kind, id) = match &notice.subject {
                Subject::Task(id) => ("task", id),
                Subject::Artifact(id) => ("artifact", id),
            };
            json!({
                "kind": kind,
                "id": id,
                "version": notice.version,
                "changed": notice.changed,
                "author": notice.author,
                "reason_untrusted": notice.reason,
                "for_claim_on": notice.claimed_task,
            })
        })
        .collect();
    if let Some(object) = result.as_object_mut() {
        object.insert("unread_changes".to_owned(), Value::Array(rows));
        if reply.omitted > 0 {
            object.insert("unread_changes_omitted".to_owned(), json!(reply.omitted));
        }
    }
    result
}

impl ToolProvider for MemoryTools {
    fn tools(&self) -> Vec<ToolDef> {
        let s = || json!({"type": "string"});
        let tags = json!({"type": "array", "items": {"type": "string"}});
        let schema = |required: &[&str], properties: Value| {
            let mut map = properties.as_object().cloned().unwrap_or_default();
            map.insert("project".to_owned(), s());
            json!({"type": "object", "required": required, "properties": map})
        };
        vec![
            ToolDef {
                name: "memory.add",
                description: "Store a fact for all agents of the project.",
                input_schema: schema(&["text"], json!({"text": s(), "tags": tags, "task": s()})),
                read_only: false,
            },
            ToolDef {
                name: "memory.search",
                description: "Find memory by words or a tag. Best word match first, then newest.",
                input_schema: schema(
                    &[],
                    json!({"text": s(), "tag": s(), "limit": {"type": "integer"}}),
                ),
                read_only: true,
            },
            ToolDef {
                name: "memory.delete",
                description: "Delete a memory record.",
                input_schema: schema(&["id"], json!({"id": s()})),
                read_only: false,
            },
        ]
    }

    fn call(&self, ctx: ToolContext<'_>, name: &str, args: Value) -> Result<Value> {
        let requested = args.get("project").and_then(Value::as_str);
        let project = ctx.host.project_for(ctx.caller, requested)?;
        let service = memory_of(&project)?;
        let result = match name {
            "memory.add" => {
                let args: AddArgs = parse_args(args)?;
                let record = service.add(
                    ctx.caller,
                    NewMemory {
                        text: args.text,
                        tags: args.tags,
                        task: args.task,
                    },
                )?;
                json!({"memory": record.id.short(), "full_id": record.id.to_string()})
            }
            "memory.search" => {
                let args: SearchArgs = parse_args(args)?;
                let found = service.search(&MemoryQuery {
                    text: args.text,
                    tag: args.tag,
                    limit: args.limit,
                })?;
                let rows: Vec<Value> = found.iter().map(record_json).collect();
                json!({"count": rows.len(), "memories": rows})
            }
            "memory.delete" => {
                let args: DeleteArgs = parse_args(args)?;
                let id = service.resolve(&args.id)?;
                service.delete(ctx.caller, id)?;
                json!({"deleted": id.short()})
            }
            _ => return Err(LenkaError::not_found(format!("Unknown tool {name}."))),
        };
        Ok(with_unread(result, &project, ctx.caller))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(text: &str) -> MemoryRecord {
        MemoryRecord {
            id: MemoryId::generate(),
            author: "agent:a".to_owned(),
            at: Timestamp::from_unix_ms(1_793_000_000_000),
            tags: vec!["api".to_owned()],
            task: Some(TaskId::generate()),
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_record_survives_a_round_trip() -> Result<()> {
        let record = sample("The port is 8080.\n\n+++ is text here.\n## Heading");
        assert_eq!(parse_record(&format_record(&record)?)?, record);
        Ok(())
    }

    #[test]
    fn hostile_files_give_clear_errors() {
        for bad in [
            "",
            "no fence",
            "+++\nid = 1\n+++\ntext",
            "+++\nid = \"x\"",
            "+++\nid = \"../../etc\"\nauthor = \"a\"\nat = \"2026-11-06T12:00:00Z\"\n+++\n",
        ] {
            assert_eq!(
                parse_record(bad).map(|_| ()).map_err(|error| error.code()),
                Err(ErrorCode::InvalidFile),
                "{bad}"
            );
        }
        let record = sample("<<<<<<< HEAD");
        let text = format_record(&record).unwrap_or_default();
        assert!(parse_record(&text).is_err());
    }

    #[test]
    fn words_ignore_case_and_punctuation() {
        let found = words("Redis, redis! port-8080");
        assert_eq!(found.len(), 3);
        assert!(found.contains("redis"));
        assert!(found.contains("8080"));
    }

    #[test]
    fn tags_are_cleaned_and_limited() -> Result<()> {
        let clean = clean_tags(vec![" a ".to_owned(), "a".to_owned(), "b".to_owned()])?;
        assert_eq!(clean, vec!["a", "b"]);
        assert!(clean_tags(vec![String::new()]).is_err());
        assert!(clean_tags(vec!["a\nb".to_owned()]).is_err());
        assert!(clean_tags(vec!["x".repeat(MAX_TAG_BYTES + 1)]).is_err());
        let many = (0..=MAX_TAGS).map(|index| index.to_string()).collect();
        assert!(clean_tags(many).is_err());
        Ok(())
    }
}
