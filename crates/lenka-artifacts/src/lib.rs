// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The artifact plugin. One artifact can link to many tasks in its project.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use lenka_core::notices::{self, FEED_PATH};
use lenka_core::secret::{self, SecretPolicy, TextCheck};
use lenka_core::{
    Commit, FeedEvent, FeedKind, Op, Plugin, PluginInfo, Project, Registry, ToolContext,
    ToolProvider,
};
use lenka_plugin_api::{
    Actor, ArtifactId, Caller, DocPath, ErrorCode, LenkaError, Result, StoredBytes, Timestamp,
    ToolDef,
};
use lenka_tasks::{
    ArtifactRecord, ArtifactVersion, ClaimRef, TaskService, is_safe_file_name, read_artifact,
    service_of, with_notes,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// The media type of a drawing board.
pub const DRAWING_MEDIA_TYPE: &str = "application/vnd.lenka.drawing+json";

/// The largest artifact that lenka accepts.
const MAX_ARTIFACT_BYTES: u64 = 1024 * 1024 * 1024;

/// The first bytes of a Git LFS pointer file, with the longest text first.
const LFS_POINTER_STARTS: [&str; 2] = [
    "version https://git-lfs.github.com/spec/",
    "version https://hawser.github.com/spec/",
];

/// What happens to the old version in an update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateMode {
    /// Keep the old version.
    #[default]
    Keep,
    /// Delete the old version. Committed bytes stay in the Git history.
    Replace,
}

/// The source of artifact bytes.
#[derive(Debug, Clone, Copy)]
pub enum Source<'a> {
    /// A local file.
    File(&'a Path),
    /// Bytes in memory.
    Bytes {
        /// The file name.
        name: &'a str,
        /// The media type.
        media_type: &'a str,
        /// The bytes.
        bytes: &'a [u8],
    },
}

/// A request for a new artifact version.
#[derive(Debug, Clone)]
pub struct NewVersion<'a> {
    /// The version that the caller read.
    pub expected_version: u64,
    /// The source of the bytes.
    pub source: Source<'a>,
    /// The reason for the change.
    pub reason: &'a str,
    /// What happens to the old version.
    pub mode: UpdateMode,
    /// The relation to a claim.
    pub claim: ClaimRef,
}

/// The artifact operations of one project.
#[derive(Debug)]
pub struct ArtifactService {
    project: Arc<Project>,
    tasks: Arc<TaskService>,
}

/// Returns the artifact service of a project, and creates it on first use.
///
/// # Errors
///
/// Returns an error if the task service cannot open.
pub fn artifacts_of(project: &Arc<Project>) -> Result<Arc<ArtifactService>> {
    project.extension(|project| {
        Ok(ArtifactService {
            project: Arc::clone(project),
            tasks: service_of(project)?,
        })
    })
}

fn safe_file_name(raw: &str) -> String {
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim_start_matches('.');
    if !lenka_core::layout::is_artifact_file_name(clean) {
        "artifact".to_owned()
    } else {
        clean.to_owned()
    }
}

fn guess_media_type(name: &str) -> &'static str {
    let extension = name
        .rsplit_once('.')
        .map_or("", |(_, ext)| ext)
        .to_ascii_lowercase();
    match extension.as_str() {
        "md" | "markdown" => "text/markdown",
        "txt" | "log" => "text/plain",
        "json" => "application/json",
        "toml" => "application/toml",
        "yaml" | "yml" => "application/yaml",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "rs" | "py" | "js" | "ts" | "go" | "c" | "h" | "cpp" | "java" | "sh" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Refuses a source file that an agent cannot use. An agent can use only files inside its current
/// worktree, and never `.git` or a symlink that leads outside.
fn check_source(caller: &Caller, source: &Path) -> Result<PathBuf> {
    let joined = if source.is_absolute() {
        source.to_path_buf()
    } else {
        caller.worktree.join(source)
    };
    let real = std::fs::canonicalize(&joined).map_err(|err| {
        LenkaError::invalid_argument(format!("The file is not available: {err}."))
    })?;
    if !real.is_file() {
        return Err(LenkaError::invalid_argument("The path is not a file."));
    }
    if caller.actor == Actor::Human {
        return Ok(real);
    }
    let root = std::fs::canonicalize(&caller.worktree).unwrap_or_else(|_| caller.worktree.clone());
    let Ok(relative) = real.strip_prefix(&root) else {
        return Err(LenkaError::new(
            ErrorCode::ActorDenied,
            "An artifact file must be inside the current worktree.",
        ));
    };
    if relative
        .components()
        .any(|part| matches!(part, Component::Normal(name) if name == ".git"))
    {
        return Err(LenkaError::new(
            ErrorCode::ActorDenied,
            "An artifact file cannot come from the .git directory.",
        ));
    }
    Ok(real)
}

fn record_path(id: ArtifactId) -> String {
    format!("artifacts/{id}/artifact.toml")
}

fn toml_text(record: &ArtifactRecord) -> Result<String> {
    toml::to_string(record).map_err(|err| LenkaError::invalid_file(format!("TOML: {err}")))
}

fn feed_op(
    kind: FeedKind,
    id: ArtifactId,
    version: u64,
    author: &str,
    reason: &str,
    at: Timestamp,
) -> Result<Op> {
    let event = FeedEvent {
        kind,
        id: id.to_string(),
        version,
        author: author.to_owned(),
        reason: reason.to_owned(),
        changed: if reason.is_empty() {
            Vec::new()
        } else {
            vec!["content".to_owned()]
        },
        at,
    };
    Ok(Op::Append {
        path: FEED_PATH.to_owned(),
        line: notices::format_line(&event)?,
    })
}

impl ArtifactService {
    fn prepare(&self, caller: &Caller, source: Source<'_>) -> Result<Prepared> {
        match source {
            Source::File(path) => {
                let real = check_source(caller, path)?;
                let size = std::fs::metadata(&real)
                    .map_err(|err| LenkaError::io("Read the file metadata", &err))?
                    .len();
                if size > MAX_ARTIFACT_BYTES {
                    return Err(LenkaError::invalid_argument(format!(
                        "The file is larger than the limit of {MAX_ARTIFACT_BYTES} bytes."
                    )));
                }
                let raw_name = real
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let file = safe_file_name(&raw_name);
                Ok(Prepared {
                    display_name: raw_name,
                    media_type: guess_media_type(&file).to_owned(),
                    file,
                    content: Content::Path(real),
                })
            }
            Source::Bytes {
                name,
                media_type,
                bytes,
            } => Ok(Prepared {
                display_name: name.to_owned(),
                file: safe_file_name(name),
                media_type: media_type.to_owned(),
                content: Content::Bytes(bytes.to_vec()),
            }),
        }
    }

    fn store(&self, id: ArtifactId, version: u64, prepared: &Prepared) -> Result<StoredBytes> {
        let store = self.project.artifacts();
        match &prepared.content {
            Content::Path(path) => store.put_file(id, version, &prepared.file, path),
            Content::Bytes(bytes) => store.put_bytes(id, version, &prepared.file, bytes),
        }
    }

    /// Adds an artifact.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ActorDenied`] for a file outside the worktree, and
    /// [`ErrorCode::SecretDetected`] if a text artifact matches a secret pattern.
    pub fn create(
        &self,
        caller: &Caller,
        source: Source<'_>,
        name: Option<&str>,
        media_type: Option<&str>,
    ) -> Result<ArtifactRecord> {
        let prepared = self.prepare(caller, source)?;
        let final_name = name.map_or_else(|| prepared.display_name.clone(), str::to_owned);
        if !is_safe_file_name(&final_name) {
            return Err(LenkaError::invalid_argument(
                "An artifact name is one file name, with no path and no control characters.",
            ));
        }
        check_content(&prepared)?;
        let final_type = media_type.map_or_else(|| prepared.media_type.clone(), str::to_owned);
        lenka_core::input::check_short("The media type", &final_type)?;
        let checks = vec![
            TextCheck::new("artifact name", final_name.clone()),
            TextCheck::new("artifact media type", final_type.clone()),
        ];
        secret::check(&checks, SecretPolicy::Enforce)?;
        let id = ArtifactId::generate();
        self.tasks.with_artifact_write(|locked, _, now| {
            let stored = self.store(id, 1, &prepared)?;
            let record = ArtifactRecord {
                id,
                name: final_name.clone(),
                media_type: final_type.clone(),
                version: 1,
                created_by: caller.label.clone(),
                created_at: now,
                versions: vec![ArtifactVersion {
                    version: 1,
                    file: prepared.file.clone(),
                    size: stored.size,
                    sha256: stored.sha256,
                    author: caller.label.clone(),
                    at: now,
                    reason: String::new(),
                }],
            };
            let ops = vec![
                Op::Put {
                    path: record_path(id),
                    text: toml_text(&record)?,
                },
                feed_op(FeedKind::Artifact, id, 1, &caller.label, "", now)?,
            ];
            locked.commit(&Commit {
                checks: checks.clone(),
                ops,
                ..Commit::default()
            })?;
            Ok(record)
        })
    }

    /// Adds a new version of an artifact. A new version is a material change.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::StaleVersion`] for an old version, [`ErrorCode::ReasonRequired`] with
    /// no reason, and [`ErrorCode::LoopLimit`] after too many agent changes.
    pub fn update(
        &self,
        caller: &Caller,
        id: ArtifactId,
        request: &NewVersion<'_>,
    ) -> Result<ArtifactRecord> {
        let NewVersion {
            expected_version,
            source,
            reason,
            mode,
            claim,
        } = request;
        let (expected_version, mode) = (*expected_version, *mode);
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(LenkaError::new(
                ErrorCode::ReasonRequired,
                "A new artifact version is a material change, so it requires a reason.",
            ));
        }
        lenka_core::input::check_reason("The reason", reason)?;
        let prepared = self.prepare(caller, *source)?;
        check_content(&prepared)?;
        let checks = vec![TextCheck::new("reason", reason)];
        secret::check(&checks, SecretPolicy::Enforce)?;
        self.tasks.check_artifact_coordinator(id, caller, claim)?;
        self.tasks.with_artifact_write(|locked, routing, now| {
            let mut record = read_artifact(&self.project, id)?.ok_or_else(|| missing(id))?;
            locked.check_version("The artifact", record.version, expected_version)?;
            let loop_op = routing.check_loop(&self.project, caller, id)?;
            let version = record.version + 1;
            let stored = self.store(id, version, &prepared)?;
            let old = record.current().version;
            if mode == UpdateMode::Replace {
                record.versions.retain(|item| item.version != old);
            }
            record.version = version;
            record.versions.push(ArtifactVersion {
                version,
                file: prepared.file.clone(),
                size: stored.size,
                sha256: stored.sha256,
                author: caller.label.clone(),
                at: now,
                reason: reason.to_owned(),
            });
            let mut ops = vec![
                Op::Put {
                    path: record_path(id),
                    text: toml_text(&record)?,
                },
                feed_op(FeedKind::Artifact, id, version, &caller.label, reason, now)?,
            ];
            ops.extend(routing.notice_ops(id, version, caller, &["content".to_owned()], reason)?);
            ops.extend(loop_op);
            locked.commit(&Commit {
                checks: checks.clone(),
                ops,
                ..Commit::default()
            })?;
            if mode == UpdateMode::Replace {
                self.project.artifacts().delete_version(id, old)?;
            }
            Ok(record)
        })
    }

    /// Deletes an artifact.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::DeleteRefused`] if a task still links to the artifact.
    pub fn delete(&self, caller: &Caller, id: ArtifactId) -> Result<()> {
        if caller.actor == Actor::Agent && !self.project.config().limits.agent_delete {
            return Err(LenkaError::new(
                ErrorCode::DeleteRefused,
                "A project setting denies the delete tools to agents.",
            ));
        }
        self.tasks.with_artifact_write(|locked, routing, now| {
            // The record can be invalid. That must not keep the artifact from a delete.
            let record = DocPath::new(record_path(id))?;
            if self.project.driver().stat(&record)?.is_none() {
                return Err(missing(id));
            }
            if routing.is_linked(id) {
                return Err(LenkaError::new(
                    ErrorCode::DeleteRefused,
                    "A task still links to this artifact. Remove the links first.",
                ));
            }
            let ops = vec![
                Op::RemoveTree {
                    path: format!("artifacts/{id}"),
                },
                feed_op(FeedKind::ArtifactDeleted, id, 0, &caller.label, "", now)?,
            ];
            locked.commit(&Commit {
                ops,
                ..Commit::default()
            })
        })
    }

    /// Returns the record of an artifact.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] if the artifact does not exist.
    pub fn record(&self, id: ArtifactId) -> Result<ArtifactRecord> {
        read_artifact(&self.project, id)?.ok_or_else(|| missing(id))
    }

    /// Returns all artifacts, sorted by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the data cannot be read.
    pub fn list(&self) -> Result<Vec<ArtifactRecord>> {
        self.list_with_problems().map(|(records, _)| records)
    }

    /// Returns all artifacts, sorted by ID, and the IDs of the artifacts with an invalid record.
    ///
    /// # Errors
    ///
    /// Returns an error if the data cannot be read.
    pub fn list_with_problems(&self) -> Result<(Vec<ArtifactRecord>, Vec<ArtifactId>)> {
        let dir = DocPath::new("artifacts")?;
        let mut records = Vec::new();
        let mut invalid = Vec::new();
        for name in self.project.driver().list(&dir)? {
            let Ok(id) = ArtifactId::parse(&name) else {
                continue;
            };
            // One bad file from a merge must not hide all other artifacts.
            match read_artifact(&self.project, id) {
                Ok(Some(record)) => records.push(record),
                Ok(None) => {}
                Err(error) if error.code() == ErrorCode::InvalidFile => {
                    tracing::warn!(artifact = %id, code = error.code().as_str(), "skipped an invalid artifact");
                    invalid.push(id);
                }
                Err(error) => return Err(error),
            }
        }
        Ok((records, invalid))
    }

    /// Reads the bytes of a version. No version selects the current version.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] if the version does not exist.
    pub fn read_bytes(&self, id: ArtifactId, version: Option<u64>) -> Result<Vec<u8>> {
        let record = self.record(id)?;
        let wanted = version.unwrap_or(record.version);
        let item = record
            .versions
            .iter()
            .find(|item| item.version == wanted)
            .ok_or_else(|| {
                LenkaError::not_found(format!("The artifact has no version {wanted}."))
            })?;
        self.project.artifacts().get(id, wanted, &item.file)
    }

    /// Reads the bytes of the newest version, and stops at a limit. A larger file does not enter
    /// the memory.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidFile`] if the version is larger than the limit, or an error if
    /// the bytes cannot be read.
    pub fn read_newest_limited(
        &self,
        id: ArtifactId,
        max_bytes: u64,
    ) -> Result<(ArtifactRecord, Vec<u8>)> {
        let record = self.record(id)?;
        let item = record.current();
        let bytes =
            self.project
                .artifacts()
                .get_limited(id, record.version, &item.file, max_bytes)?;
        Ok((record, bytes))
    }

    /// Resolves a full ID or a short ID.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] or [`ErrorCode::AmbiguousId`].
    pub fn resolve(&self, text: &str) -> Result<ArtifactId> {
        if let Ok(id) = ArtifactId::parse(text) {
            return Ok(id);
        }
        lenka_plugin_api::check_short_id(text)?;
        let dir = DocPath::new("artifacts")?;
        let names = self.project.driver().list(&dir)?;
        let mut found = names.iter().filter(|name| name.ends_with(text));
        match (found.next(), found.next()) {
            (Some(name), None) => ArtifactId::parse(name),
            (Some(_), Some(_)) => Err(LenkaError::new(
                ErrorCode::AmbiguousId,
                format!("The short ID {text} matches more than one artifact. Use the full ID."),
            )),
            _ => Err(LenkaError::not_found(format!(
                "No artifact matches the ID {text}."
            ))),
        }
    }
}

#[derive(Debug)]
enum Content {
    Path(PathBuf),
    Bytes(Vec<u8>),
}

#[derive(Debug)]
struct Prepared {
    display_name: String,
    file: String,
    media_type: String,
    content: Content,
}

/// Checks all bytes of an artifact for secrets.
/// Refuses a Git LFS pointer. It is only a reference to bytes, and with Git LFS on, a pointer that
/// has no object can stop each push of the project.
fn check_not_lfs_pointer(prepared: &Prepared) -> Result<()> {
    let mut start = [0_u8; LFS_POINTER_STARTS[0].len()];
    let count = match &prepared.content {
        Content::Path(path) => {
            let mut file = std::fs::File::open(path)
                .map_err(|err| LenkaError::io("Open the artifact source", &err))?;
            let mut count = 0;
            while count < start.len() {
                let read = file
                    .read(&mut start[count..])
                    .map_err(|err| LenkaError::io("Read the artifact source", &err))?;
                if read == 0 {
                    break;
                }
                count += read;
            }
            count
        }
        Content::Bytes(bytes) => {
            let count = bytes.len().min(start.len());
            start[..count].copy_from_slice(&bytes[..count]);
            count
        }
    };
    if LFS_POINTER_STARTS
        .iter()
        .any(|pointer| start[..count].starts_with(pointer.as_bytes()))
    {
        return Err(LenkaError::invalid_argument(
            "This file is a Git LFS pointer, not the real bytes. Run `git lfs pull`, and try again.",
        ));
    }
    Ok(())
}

fn check_content(prepared: &Prepared) -> Result<()> {
    check_not_lfs_pointer(prepared)?;
    let location = format!("artifact {}", prepared.file);
    match &prepared.content {
        Content::Path(path) => {
            let mut file = std::fs::File::open(path)
                .map_err(|err| LenkaError::io("Open the artifact source", &err))?;
            secret::check_stream(&location, &mut file)
        }
        Content::Bytes(bytes) => {
            let mut reader = bytes.as_slice();
            secret::check_stream(&location, &mut reader)
        }
    }
}

fn missing(id: ArtifactId) -> LenkaError {
    LenkaError::not_found(format!("The artifact {} does not exist.", id.short()))
}

/// The artifact plugin.
#[derive(Debug, Clone, Copy, Default)]
pub struct ArtifactsPlugin;

/// Returns the artifact plugin.
#[must_use]
pub fn plugin() -> ArtifactsPlugin {
    ArtifactsPlugin
}

impl Plugin for ArtifactsPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "artifacts",
            description: "Artifacts with versions, which link to tasks",
            sensitive: false,
        }
    }

    fn register(&self, registry: &mut Registry) {
        registry.tools(Arc::new(ArtifactTools));
    }
}

#[derive(Debug)]
struct ArtifactTools;

#[derive(Deserialize)]
struct CreateArgs {
    path: String,
    name: Option<String>,
    media_type: Option<String>,
}

#[derive(Deserialize)]
struct UpdateArgs {
    artifact: String,
    version: u64,
    path: String,
    reason: String,
    #[serde(default)]
    mode: UpdateMode,
    claim: Option<String>,
}

#[derive(Deserialize)]
struct DeleteArgs {
    artifact: String,
}

fn parse_args<T: serde::de::DeserializeOwned>(args: Value) -> Result<T> {
    serde_json::from_value(args)
        .map_err(|err| LenkaError::invalid_argument(format!("The arguments are not valid: {err}")))
}

fn record_json(record: &ArtifactRecord) -> Value {
    json!({
        "artifact": record.id.short(),
        "full_id": record.id.to_string(),
        "untrusted_data": {"name": record.name, "media_type": record.media_type},
        "version": record.version,
        "size": record.current().size,
        "sha256": record.current().sha256,
    })
}

impl ToolProvider for ArtifactTools {
    fn tools(&self) -> Vec<ToolDef> {
        let s = || json!({"type": "string"});
        let schema = |required: &[&str], properties: Value| {
            let mut map = properties.as_object().cloned().unwrap_or_default();
            map.insert("project".to_owned(), s());
            json!({"type": "object", "required": required, "properties": map})
        };
        vec![
            ToolDef {
                name: "artifact.create",
                description: "Add an artifact from a file in the current worktree. A relative path starts at the worktree root. Link it with task.update.",
                input_schema: schema(
                    &["path"],
                    json!({"path": s(), "name": s(), "media_type": s()}),
                ),
                read_only: false,
            },
            ToolDef {
                name: "artifact.update",
                description: "Add a new version from a file. Needs the version you read and a reason. Mode `keep` keeps the old version, and `replace` deletes it.",
                input_schema: schema(
                    &["artifact", "version", "path", "reason"],
                    json!({
                        "artifact": s(), "version": {"type": "integer"}, "path": s(), "reason": s(),
                        "mode": {"enum": ["keep", "replace"]}, "claim": s(),
                    }),
                ),
                read_only: false,
            },
            ToolDef {
                name: "artifact.delete",
                description: "Delete an artifact that no task links to.",
                input_schema: schema(&["artifact"], json!({"artifact": s()})),
                read_only: false,
            },
        ]
    }

    fn call(&self, ctx: ToolContext<'_>, name: &str, args: Value) -> Result<Value> {
        let requested = args.get("project").and_then(Value::as_str);
        let project = ctx.host.project_for(ctx.caller, requested)?;
        let service = artifacts_of(&project)?;
        let result = match name {
            "artifact.create" => {
                let args: CreateArgs = parse_args(args)?;
                let record = service.create(
                    ctx.caller,
                    Source::File(Path::new(&args.path)),
                    args.name.as_deref(),
                    args.media_type.as_deref(),
                )?;
                record_json(&record)
            }
            "artifact.update" => {
                let args: UpdateArgs = parse_args(args)?;
                let id = service.resolve(&args.artifact)?;
                let claim = args.claim.map_or(ClaimRef::Implicit, ClaimRef::Handle);
                let record = service.update(
                    ctx.caller,
                    id,
                    &NewVersion {
                        expected_version: args.version,
                        source: Source::File(Path::new(&args.path)),
                        reason: &args.reason,
                        mode: args.mode,
                        claim,
                    },
                )?;
                record_json(&record)
            }
            "artifact.delete" => {
                let args: DeleteArgs = parse_args(args)?;
                let id = service.resolve(&args.artifact)?;
                service.delete(ctx.caller, id)?;
                json!({"deleted": id.short()})
            }
            _ => return Err(LenkaError::not_found(format!("Unknown tool {name}."))),
        };
        Ok(with_notes(result, &service.tasks, ctx.caller))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_become_safe() {
        assert_eq!(safe_file_name("design v2.png"), "design_v2.png");
        assert_eq!(safe_file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(safe_file_name("...."), "artifact");
        assert_eq!(safe_file_name(""), "artifact");
    }

    #[test]
    fn media_types() {
        assert_eq!(guess_media_type("A.PNG"), "image/png");
        assert_eq!(guess_media_type("noext"), "application/octet-stream");
    }
}
