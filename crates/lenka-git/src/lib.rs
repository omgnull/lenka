// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The Git plugin. It commits and pushes project data with the installed `git`.
//!
//! This plugin runs `git` with fixed arguments and no shell. It uses the network only for
//! `fetch` and `push`, so it is off until `plugins.toml` lists it. Data commits skip all Git
//! hooks of the user, because those hooks are for the code of the user, not for this data.
//!
//! The data lives on the branch [`BRANCH`]. A project in `<git-common-dir>/lenka-data` is a
//! worktree of the clone of the user. Any other project has its own repository.
//!
//! The Git settings of this plugin are in the local Git configuration, in the keys `lenka.*`. A
//! merge cannot change that file, so the remote and the use of the network stay a choice of each
//! clone. A commit holds only the paths in [`lenka_core::layout`], and a branch from another clone
//! must hold only these paths.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use lenka_core::files;
use lenka_core::gitdir;
use lenka_core::init::{GIT_DATA_DIR, GITATTRIBUTES};
use lenka_core::layout::{self, TEMP_PREFIX};
use lenka_core::settings::PROJECT_FILE;
use lenka_core::{EventHook, Plugin, PluginInfo, Project, ProjectConfig, ProjectEvent, Registry};
use lenka_plugin_api::{ErrorCode, LenkaError, Result};
use serde::Serialize;

/// The name of the data branch.
pub const BRANCH: &str = "lenka-data";

const BRANCH_REF: &str = "refs/heads/lenka-data";

/// The default remote name.
const DEFAULT_REMOTE: &str = "origin";

/// The keys of the settings in the local Git configuration.
const KEY_ENABLED: &str = "lenka.enabled";
const KEY_REMOTE: &str = "lenka.remote";
const KEY_PUSH_ON_FINISH: &str = "lenka.pushOnFinish";
/// This key is on from the start of a merge until the caller rebuilds the task index.
const KEY_NEEDS_REINDEX: &str = "lenka.needsReindex";

/// The largest settings file that lenka reads from a branch.
const MAX_SETTINGS_BYTES: u64 = 1024 * 1024;

/// The count of paths in one call of `git update-index`.
const PATHS_IN_ONE_CALL: usize = 100;

/// The text fields that a person or lenka checks for conflict markers on each write. Other files
/// can hold such a line in normal text.
const MARKER_PATHS: [&str; 3] = [
    ":(glob)tasks/*/task.md",
    ":(glob)tasks/*/plan.md",
    ":(glob)memory/*.md",
];
const MARKER_PATTERN: &str = "^<<<<<<< ";

/// The identity for commits when the user has no Git identity.
const FALLBACK_NAME: &str = "lenka";
const FALLBACK_EMAIL: &str = "lenka@localhost.invalid";

/// The environment settings that would point `git` at another repository.
const CLEARED_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_NAMESPACE",
];

/// The temporary directory of `enable` has this text after the name of the data directory.
const CONNECT_SUFFIX: &str = ".new-";

/// The time limit of a push in an agent call. A person can stop a push, and an agent cannot.
const DEFAULT_PUSH_LIMIT: Duration = Duration::from_secs(60);
const MAX_PUSH_LIMIT: Duration = Duration::from_secs(3600);
/// Sets the time limit of a push in an agent call, in milliseconds.
const PUSH_LIMIT_ENV: &str = "LENKA_GIT_PUSH_LIMIT_MS";

/// Tells if `git` can ask a person for credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prompt {
    /// A person runs the command.
    Allowed,
    /// An agent call waits for the command, so a prompt must fail at once.
    Denied,
}

/// A `git` runner for one directory.
#[derive(Debug)]
struct Git {
    dir: PathBuf,
    prompt: Prompt,
}

impl Git {
    fn new(dir: &Path, prompt: Prompt) -> Self {
        Self {
            dir: dir.to_path_buf(),
            prompt,
        }
    }

    /// Returns a hook directory that cannot exist. Git LFS tries to create this directory and to
    /// add its hooks there. On Windows, `/dev/null` is a relative path for Git LFS, and a path below
    /// a regular file cannot become a directory.
    #[cfg(windows)]
    fn no_hooks_path(&self) -> OsString {
        let file = [PROJECT_FILE, "config", "HEAD"]
            .into_iter()
            .map(|name| self.dir.join(name))
            .find(|path| path.is_file())
            .unwrap_or_else(|| self.dir.clone());
        file.join("no-hooks").into_os_string()
    }

    #[cfg(not(windows))]
    #[expect(
        clippy::unused_self,
        reason = "the Windows form of this function reads the directory"
    )]
    fn no_hooks_path(&self) -> OsString {
        OsString::from("/dev/null")
    }

    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .env("LC_ALL", "C")
            // A merge must not depend on a download. `download_lfs` gets the bytes after a merge.
            .env("GIT_LFS_SKIP_SMUDGE", "1");
        for name in CLEARED_ENV {
            command.env_remove(name);
        }
        if self.prompt == Prompt::Denied {
            // An empty value turns off all programs that ask for a password.
            command
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_ASKPASS", "");
        }
        // The hook path is outside the data, so that no data from a remote can become a hook.
        let mut hooks = OsString::from("core.hooksPath=");
        hooks.push(self.no_hooks_path());
        command.arg("-c").arg(hooks).args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "commit.gpgsign=false",
        ]);
        command
    }

    fn output<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command().args(args).output().map_err(|err| {
            LenkaError::new(
                ErrorCode::Io,
                format!("lenka cannot run git: {err}. Install Git, and try again."),
            )
        })
    }

    /// Runs a command that uses the network. An agent call has a time limit and no SSH prompt.
    fn run_network(&self, what: &str, args: &[&str]) -> Result<()> {
        let mut command = self.command();
        command.args(args);
        if self.prompt == Prompt::Allowed {
            let output = command.output().map_err(|err| git_start_error(&err))?;
            return check_output(what, &output);
        }
        // `GIT_TERMINAL_PROMPT` has no effect on the SSH client. A setting of the person stays.
        if std::env::var_os("GIT_SSH_COMMAND").is_none()
            && std::env::var_os("GIT_SSH").is_none()
            && !self.passes(["config", "--get", "core.sshCommand"])?
        {
            command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
        let limit = parse_push_limit(std::env::var(PUSH_LIMIT_ENV).ok().as_deref());
        let output = output_with_limit(command, limit).map_err(|err| git_start_error(&err))?;
        match output {
            Some(output) => check_output(what, &output),
            None => Err(LenkaError::new(
                ErrorCode::Io,
                format!(
                    "`git {what}` did not end in {} ms, so lenka stopped it. Run `lenka sync` to send the data.",
                    limit.as_millis()
                ),
            )),
        }
    }

    /// Runs a command that must pass, and returns its output text.
    fn run<I, S>(&self, what: &str, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.output(args)?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_owned())
        } else {
            Err(git_error(what, &output))
        }
    }

    /// Runs a command for its exit status only.
    fn passes<I, S>(&self, args: I) -> Result<bool>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Ok(self.output(args)?.status.success())
    }

    /// Reads a value of the local Git configuration.
    fn local_value(&self, key: &str) -> Result<Option<String>> {
        let output = self.output(["config", "--local", "--get", key])?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
    }

    fn set_local_value(&self, key: &str, value: &str) -> Result<()> {
        self.run("config", ["config", "--local", key, value])
            .map(|_| ())
    }

    fn unset_local_value(&self, key: &str) -> Result<()> {
        if self.local_value(key)?.is_some() {
            self.run("config", ["config", "--local", "--unset", key])?;
        }
        Ok(())
    }

    /// Returns the options for a command that creates a commit.
    fn identity_args(&self) -> Result<Vec<String>> {
        let name = self.passes(["config", "--get", "user.name"])?;
        let email = self.passes(["config", "--get", "user.email"])?;
        if name && email {
            return Ok(Vec::new());
        }
        Ok(vec![
            "-c".to_owned(),
            format!("user.name={FALLBACK_NAME}"),
            "-c".to_owned(),
            format!("user.email={FALLBACK_EMAIL}"),
        ])
    }
}

fn git_error(what: &str, output: &Output) -> LenkaError {
    // Git LFS can write its failure to the standard output.
    let stream = if output.stderr.iter().all(u8::is_ascii_whitespace) {
        &output.stdout
    } else {
        &output.stderr
    };
    let text = String::from_utf8_lossy(stream);
    let lines: Vec<&str> = text.lines().rev().take(6).collect();
    let detail: String = lines.into_iter().rev().collect::<Vec<_>>().join(" ");
    LenkaError::new(
        ErrorCode::Io,
        format!("git {what} failed: {}", scrub_credentials(&detail)),
    )
}

/// Removes a user name and a password from each URL in a text, so that an error shows no secret.
fn scrub_credentials(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find("://") {
        let (head, tail) = rest.split_at(index + 3);
        clean.push_str(head);
        let end = tail
            .find(|c: char| c == '/' || c == '\'' || c == '"' || c.is_whitespace())
            .unwrap_or(tail.len());
        match tail[..end].rfind('@') {
            Some(at) => {
                clean.push_str("***@");
                rest = &tail[at + 1..];
            }
            None => rest = tail,
        }
    }
    clean.push_str(rest);
    clean
}

/// Refuses a remote name that `git` could read as an option. `lenka.toml` is untrusted input.
fn validate_remote_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(LenkaError::invalid_argument(
            "A Git remote name can hold only letters, digits, '.', '_', and '-'.",
        ))
    }
}

fn local_dir(project: &Project) -> Result<PathBuf> {
    project
        .driver()
        .local_dir()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            LenkaError::invalid_argument("The Git plugin needs a project with a local directory.")
        })
}

/// Opens the repository of a project, and refuses anything other than the data branch. Without
/// this check, `git` could find a parent repository and commit in the code of the user.
fn open_data_repo(project: &Project, prompt: Prompt) -> Result<Git> {
    let dir = local_dir(project)?;
    if fs::symlink_metadata(dir.join(".git")).is_err() {
        return Err(LenkaError::invalid_argument(
            "The data directory is not a Git repository. Run `lenka git enable` first.",
        ));
    }
    let git = Git::new(&dir, prompt);
    let head = git.output(["symbolic-ref", "--quiet", "HEAD"])?;
    if String::from_utf8_lossy(&head.stdout).trim() != BRANCH_REF {
        return Err(LenkaError::invalid_argument(format!(
            "The data directory is not on the branch {BRANCH}. Switch to that branch in {}.",
            dir.display()
        )));
    }
    Ok(git)
}

/// The result of a commit of all data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Stored {
    /// Tells if a commit exists.
    committed: bool,
    /// The files that are not data, so no commit holds them.
    skipped: Vec<String>,
}

/// Stops if a merge or a similar operation of a person is open. A commit now would complete that
/// operation, and it would publish conflict markers.
fn refuse_open_merge(git: &Git) -> Result<()> {
    for name in ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"] {
        if git.passes(["rev-parse", "-q", "--verify", name])? {
            return Err(open_merge_error(&git.dir));
        }
    }
    if !git.run("ls-files", ["ls-files", "-u"])?.is_empty() {
        return Err(open_merge_error(&git.dir));
    }
    Ok(())
}

fn open_merge_error(dir: &Path) -> LenkaError {
    LenkaError::invalid_argument(format!(
        "The data directory {} has an open merge, so lenka did not commit. Finish it with `git commit`, or stop it with `git merge --abort`.",
        dir.display()
    ))
}

/// Refuses a tree or the index if a text file of lenka has a conflict marker.
fn refuse_markers(git: &Git, revision: Option<&str>, what: &str) -> Result<()> {
    let mut args = vec!["grep", "-l", "-E"];
    match revision {
        Some(revision) => args.extend([MARKER_PATTERN, revision]),
        None => args.extend(["--cached", MARKER_PATTERN]),
    }
    args.push("--");
    args.extend(MARKER_PATHS);
    let output = git.output(args)?;
    match output.status.code() {
        Some(0) => Err(LenkaError::invalid_file(format!(
            "{what} has conflict markers in {}. Resolve them first.",
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .escape_default()
        ))),
        Some(1) => Ok(()),
        _ => Err(git_error("grep", &output)),
    }
}

/// Converts a path from the raw output of `git`.
fn os_path(bytes: &[u8]) -> OsString {
    #[cfg(unix)]
    {
        OsString::from(<OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(bytes))
    }
    #[cfg(not(unix))]
    {
        OsString::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// Tells if a blob holds exactly this text. The size check comes first, so that a large blob from
/// a remote stays out of memory.
fn blob_is(git: &Git, blob: &str, text: &str) -> Result<bool> {
    let size = git.run("cat-file", ["cat-file", "-s", blob])?;
    if size != text.len().to_string() {
        return Ok(false);
    }
    let output = git.output(["cat-file", "blob", blob])?;
    Ok(output.status.success() && output.stdout == text.as_bytes())
}

/// Removes all that is not data from the index. The files stay on the disk.
fn unstage_other_files(git: &Git) -> Result<Vec<String>> {
    let output = git.output(["ls-files", "-s", "-z"])?;
    if !output.status.success() {
        return Err(git_error("ls-files", &output));
    }
    let mut remove: Vec<OsString> = Vec::new();
    let mut skipped = Vec::new();
    for row in output.stdout.split(|byte| *byte == 0) {
        let Some(tab) = row.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let (info, path) = (&row[..tab], &row[tab + 1..]);
        let text = String::from_utf8_lossy(path);
        let info = String::from_utf8_lossy(info);
        let mut fields = info.split(' ');
        let mode = fields.next().unwrap_or_default();
        let blob = fields.next().unwrap_or_default();
        if !is_file_mode(mode) || !layout::is_data_path(&text) {
            remove.push(os_path(path));
            skipped.push(text.escape_default().to_string());
        } else if text == ".gitattributes" && !blob_is(git, blob, GITATTRIBUTES)? {
            // Other clones refuse other text, so the commit keeps the text of the last commit.
            git.run("reset", ["reset", "-q", "--", ".gitattributes"])?;
            skipped.push(text.into_owned());
        }
    }
    for paths in remove.chunks(PATHS_IN_ONE_CALL) {
        let mut args = vec![
            OsString::from("update-index"),
            OsString::from("--force-remove"),
            OsString::from("--"),
        ];
        args.extend_from_slice(paths);
        git.run("update-index", args)?;
    }
    Ok(skipped)
}

/// Stages all data and commits it. Forced staging ignores all exclude rules, so that no local
/// file and no file from a remote can hide data.
fn commit_all(git: &Git, message: &str) -> Result<Stored> {
    refuse_open_merge(git)?;
    let temp_files = format!(":(exclude,glob)**/{TEMP_PREFIX}*");
    git.run(
        "add",
        [
            "add",
            "-A",
            "-f",
            "--",
            ".",
            ":(exclude).runtime",
            temp_files.as_str(),
        ],
    )?;
    let skipped = unstage_other_files(git)?;
    refuse_markers(git, None, "The data")?;
    if git.passes(["diff", "--cached", "--quiet"])? {
        return Ok(Stored {
            committed: false,
            skipped,
        });
    }
    let mut args = git.identity_args()?;
    args.extend(["commit", "--no-verify", "-q", "-m", message].map(str::to_owned));
    git.run("commit", args)?;
    Ok(Stored {
        committed: true,
        skipped,
    })
}

/// Commits all data of a project under the project write lock, so that a commit never holds half
/// of a write.
fn commit_locked(project: &Project, git: &Git, message: &str) -> Result<Stored> {
    project.write_locked(|_| commit_all(git, message))
}

fn has_remote(git: &Git, remote: &str) -> Result<bool> {
    git.passes(["remote", "get-url", "--", remote])
}

/// Tells if this clone uses Git LFS.
fn uses_lfs(git: &Git) -> Result<bool> {
    Ok(git.passes(["config", "--get", "filter.lfs.clean"])? && git.passes(["lfs", "version"])?)
}

/// Sends the branch to a remote. The hooks stay off, so this function uploads Git LFS objects
/// itself. Without them, a fresh clone cannot download the artifact bytes. A pointer with no local
/// object does not stop the push, because `doctor` reports that artifact.
/// Tells what a push does with a file that matches a secret pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutgoingSecrets {
    /// Stop the push. This is the default, because a hand edit gets no check at its write.
    #[default]
    Refuse,
    /// Push the file, for a person who chose this.
    Allow,
}

/// Checks the files that a push adds or changes. A write through lenka has a check, but a hand edit
/// and a merge do not.
fn check_outgoing(git: &Git, remote: &str, secrets: OutgoingSecrets) -> Result<()> {
    if secrets == OutgoingSecrets::Allow {
        return Ok(());
    }
    let tracking = format!("refs/remotes/{remote}/{BRANCH}");
    let listing = if git.passes(["rev-parse", "--verify", "-q", tracking.as_str()])? {
        git.output([
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--diff-filter=d",
            tracking.as_str(),
            "HEAD",
        ])?
    } else {
        git.output(["ls-tree", "-r", "-z", "--name-only", "HEAD"])?
    };
    if !listing.status.success() {
        return Err(git_error("list the changed files", &listing));
    }
    for name in listing.stdout.split(|byte| *byte == 0) {
        if !name.is_empty() {
            check_outgoing_file(git, name)?;
        }
    }
    Ok(())
}

fn check_outgoing_file(git: &Git, name: &[u8]) -> Result<()> {
    let path = git.dir.join(os_path(name));
    let shown = String::from_utf8_lossy(name).escape_default().to_string();
    let mut file = match files::open_regular(&path, files::Links::Refuse) {
        Ok(file) => file,
        // A later write can remove the file, and a symlink has no bytes to check.
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
            ) =>
        {
            return Ok(());
        }
        Err(err) => return Err(LenkaError::io("Read a file for the secret check", &err)),
    };
    lenka_core::secret::check_stream(&shown, &mut file).map_err(|error| {
        LenkaError::new(
            error.code(),
            format!(
                "{error} lenka did not push. Remove this text, and rotate the secret, because the local commits keep it. To push the file anyway, run `lenka sync --allow-secret`."
            ),
        )
    })
}

fn push(git: &Git, remote: &str) -> Result<()> {
    if uses_lfs(git)? {
        git.run_network(
            "lfs push",
            &[
                "-c",
                "lfs.allowincompletepush=true",
                "lfs",
                "push",
                remote,
                BRANCH,
            ],
        )?;
    }
    let refspec = format!("{BRANCH_REF}:{BRANCH_REF}");
    git.run_network("push", &["push", "-q", remote, refspec.as_str()])
}

fn git_start_error(err: &std::io::Error) -> LenkaError {
    LenkaError::new(
        ErrorCode::Io,
        format!("lenka cannot run git: {err}. Install Git, and try again."),
    )
}

fn check_output(what: &str, output: &Output) -> Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        Err(git_error(what, output))
    }
}

fn parse_push_limit(value: Option<&str>) -> Duration {
    value
        .and_then(|text| text.parse::<u64>().ok())
        .map_or(DEFAULT_PUSH_LIMIT, Duration::from_millis)
        .min(MAX_PUSH_LIMIT)
}

/// Runs a command, and stops it at a time limit. The result is `None` at the limit.
fn output_with_limit(mut command: Command, limit: Duration) -> std::io::Result<Option<Output>> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = read_in_background(child.stdout.take());
    let stderr = read_in_background(child.stderr.take());
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            // A child of `git` can hold the pipe after `git` ends, so this wait has a limit too.
            let collect = |bytes: Receiver<Vec<u8>>| {
                bytes
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap_or_default()
            };
            return Ok(Some(Output {
                status,
                stdout: collect(stdout),
                stderr: collect(stderr),
            }));
        }
        if start.elapsed() >= limit {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The call already failed with a time limit."
            )]
            let _ = child.kill();
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The call already failed with a time limit."
            )]
            let _ = child.wait();
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn read_in_background(pipe: Option<impl Read + Send + 'static>) -> Receiver<Vec<u8>> {
    let (sender, receiver) = std::sync::mpsc::channel();
    if let Some(mut pipe) = pipe {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            #[expect(
                clippy::let_underscore_must_use,
                reason = "The caller uses the bytes that this read got."
            )]
            let _ = pipe.read_to_end(&mut bytes);
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A send fails only if the call already gave up."
            )]
            let _ = sender.send(bytes);
        });
    }
    receiver
}

/// Downloads the Git LFS bytes of the current tree. A failure is not a failure of the merge,
/// because `doctor` reports the artifacts that are not downloaded. The result is the failure text.
fn download_lfs(git: &Git, remote: Option<&str>) -> Result<Option<String>> {
    if !uses_lfs(git)? {
        return Ok(None);
    }
    let mut command = git.command();
    command
        .env_remove("GIT_LFS_SKIP_SMUDGE")
        .args(["lfs", "pull"]);
    if let Some(remote) = remote {
        command.arg(remote);
    }
    let output = command
        .output()
        .map_err(|err| LenkaError::new(ErrorCode::Io, format!("lenka cannot run git: {err}.")))?;
    Ok((!output.status.success()).then(|| git_error("lfs pull", &output).to_string()))
}

/// The settings of one clone, in the local Git configuration. A merge cannot change them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalSettings {
    enabled: bool,
    remote: String,
    push_on_finish: bool,
}

impl LocalSettings {
    fn read(git: &Git) -> Result<Self> {
        let remote = git.local_value(KEY_REMOTE)?;
        let settings = Self {
            enabled: git.local_value(KEY_ENABLED)?.as_deref() == Some("true"),
            remote: remote.unwrap_or_else(|| DEFAULT_REMOTE.to_owned()),
            push_on_finish: git.local_value(KEY_PUSH_ON_FINISH)?.as_deref() == Some("true"),
        };
        validate_remote_name(&settings.remote)?;
        Ok(settings)
    }

    fn write(&self, git: &Git) -> Result<()> {
        git.set_local_value(KEY_ENABLED, if self.enabled { "true" } else { "false" })?;
        git.set_local_value(KEY_REMOTE, &self.remote)?;
        if self.push_on_finish {
            git.set_local_value(KEY_PUSH_ON_FINISH, "true")?;
        }
        Ok(())
    }
}

/// What `enable` did with the data directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// The data directory is a worktree of the clone, on the branch [`BRANCH`].
    Worktree,
    /// The data directory is its own repository.
    Repository,
}

/// The options of [`enable`].
#[derive(Debug, Clone, Default)]
pub struct EnableOptions {
    /// A remote: the name of a remote of the clone, or a URL for a separate repository.
    pub remote: Option<String>,
    /// Tells if `task.finish` also pushes.
    pub push_on_finish: bool,
}

/// The result of [`enable`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnableReport {
    /// The layout of the data directory.
    pub layout: Layout,
    /// The remote name in the settings of this clone.
    pub remote: String,
    /// Tells if the repository has that remote now.
    pub remote_found: bool,
    /// Tells if this call made a commit.
    pub committed: bool,
}

/// Connects a project to Git, stores the settings in the local Git configuration, and makes the
/// first commit. A repeat call is safe, and other lenka processes can continue to write.
///
/// For a data directory at `<common_dir>/lenka-data`, this function creates the branch with no
/// change to the checkout of the user, and makes the directory a worktree of that branch. For any
/// other directory, it runs `git init` in the directory.
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidArgument`] for a bad remote, or if the remote has the data branch
/// already. Returns [`ErrorCode::Io`] if `git` or a file move fails.
pub fn enable(
    project: &Project,
    common_dir: Option<&Path>,
    options: &EnableOptions,
) -> Result<EnableReport> {
    let dir = local_dir(project)?;
    let real_dir =
        gitdir::real_path(&dir).map_err(|err| LenkaError::io("Read the data directory", &err))?;
    let linked_common = common_dir
        .and_then(|common| gitdir::real_path(common).ok())
        .filter(|common| common.join(GIT_DATA_DIR) == real_dir);
    let layout = if linked_common.is_some() {
        Layout::Worktree
    } else {
        Layout::Repository
    };
    if fs::symlink_metadata(dir.join(".git")).is_err() {
        match &linked_common {
            Some(common) => connect_worktree(common, &real_dir)?,
            None => init_repository(&dir)?,
        }
    } else if let Some(common) = &linked_common {
        // An `enable` can stop before its last step. A repair of a good worktree changes nothing.
        #[expect(
            clippy::let_underscore_must_use,
            reason = "A repair of a good worktree changes nothing, and the next step reports a bad one."
        )]
        let _ = clone_git(common).output([
            OsStr::new("worktree"),
            OsStr::new("repair"),
            real_dir.as_os_str(),
        ]);
    }
    let git = open_data_repo(project, Prompt::Allowed)?;

    let mut settings = LocalSettings::read(&git)?;
    if let Some(value) = &options.remote {
        settings.remote = configure_remote(&git, layout, value)?;
        if layout == Layout::Repository {
            refuse_data_of_another_machine(&git, &settings.remote)?;
        }
    }
    settings.enabled = true;
    settings.push_on_finish = settings.push_on_finish || options.push_on_finish;
    settings.write(&git)?;
    let committed = commit_locked(project, &git, "lenka: connect the data to Git")?.committed;
    Ok(EnableReport {
        layout,
        remote_found: has_remote(&git, &settings.remote)?,
        remote: settings.remote,
        committed,
    })
}

/// The steps for a machine that needs the data of a separate repository.
const RESTORE_STEPS: &str = "To use this data, run `git clone --branch lenka-data <url> <directory>` and `lenka init --dir <directory> --name <name>`.";

/// Refuses a remote with data from another start of the project, because the two histories can
/// never merge. A network failure does not stop the call, because the first sync reports it.
fn refuse_data_of_another_machine(git: &Git, remote: &str) -> Result<()> {
    let tracking = format!("refs/remotes/{remote}/{BRANCH}");
    if git.passes(["show-ref", "--verify", "--quiet", tracking.as_str()])? {
        return Ok(());
    }
    if git.passes(["ls-remote", "--exit-code", remote, BRANCH_REF])? {
        return Err(LenkaError::invalid_argument(format!(
            "The remote has the branch {BRANCH} from another machine, and this project has separate data. {RESTORE_STEPS}"
        )));
    }
    Ok(())
}

fn configure_remote(git: &Git, layout: Layout, value: &str) -> Result<String> {
    let is_url = value.contains('/') || value.contains(':');
    if !is_url {
        validate_remote_name(value)?;
        if has_remote(git, value)? {
            return Ok(value.to_owned());
        }
        return Err(LenkaError::invalid_argument(match layout {
            Layout::Worktree => format!(
                "This clone has no remote {value}. Add it with `git remote add`, and try again."
            ),
            Layout::Repository => format!(
                "This repository has no remote {value}. A separate repository needs a URL in --remote."
            ),
        }));
    }
    if layout == Layout::Worktree {
        return Err(LenkaError::invalid_argument(
            "The data is in a worktree of this clone, so --remote must name a remote of the clone.",
        ));
    }
    if value.starts_with('-') {
        return Err(LenkaError::invalid_argument(
            "A Git remote URL cannot start with '-'.",
        ));
    }
    const NAME: &str = DEFAULT_REMOTE;
    if has_remote(git, NAME)? {
        git.run("remote set-url", ["remote", "set-url", "--", NAME, value])?;
    } else {
        git.run("remote add", ["remote", "add", "--", NAME, value])?;
    }
    Ok(NAME.to_owned())
}

fn init_repository(dir: &Path) -> Result<()> {
    let git = Git::new(dir, Prompt::Allowed);
    // The empty template adds no sample hooks.
    git.run("init", ["init", "-q", "--template=", "-b", BRANCH])
        .map(|_| ())
}

fn clone_git(common_dir: &Path) -> Git {
    Git::new(common_dir, Prompt::Allowed)
}

fn is_file_mode(mode: &str) -> bool {
    matches!(mode, "100644" | "100755")
}

/// Reads the settings of a commit. A missing or a large file is an error.
fn read_settings(git: &Git, revision: &str) -> Result<ProjectConfig> {
    let object = format!("{revision}:{PROJECT_FILE}");
    let size = git.output(["cat-file", "-s", object.as_str()])?;
    let size: Option<u64> = String::from_utf8_lossy(&size.stdout).trim().parse().ok();
    let Some(size) = size else {
        return Err(LenkaError::invalid_file(format!(
            "The branch {BRANCH} has no {PROJECT_FILE}, so it is not lenka data."
        )));
    };
    if size > MAX_SETTINGS_BYTES {
        return Err(LenkaError::invalid_file(format!(
            "The file {PROJECT_FILE} of the branch {BRANCH} is too large."
        )));
    }
    let output = git.output(["cat-file", "blob", object.as_str()])?;
    ProjectConfig::parse(&String::from_utf8_lossy(&output.stdout))
}

/// Refuses settings from another clone that make this clone another project.
fn check_same_project(local: &ProjectConfig, incoming: &ProjectConfig) -> Result<()> {
    if incoming.id != local.id || incoming.storage != local.storage {
        return Err(LenkaError::invalid_file(format!(
            "The branch {BRANCH} of another clone changes the project ID or the storage settings. lenka did not merge it. Check that remote. {RESTORE_STEPS}"
        )));
    }
    Ok(())
}

/// Refuses a commit from another clone if its tree holds anything other than the files of lenka.
fn check_incoming_tree(git: &Git, revision: &str, local: Option<&ProjectConfig>) -> Result<()> {
    let listing = git.output(["ls-tree", "-r", "-z", "--full-tree", revision])?;
    if !listing.status.success() {
        return Err(git_error("ls-tree", &listing));
    }
    for row in listing.stdout.split(|byte| *byte == 0) {
        let row = String::from_utf8_lossy(row);
        let Some((info, path)) = row.split_once('\t') else {
            continue;
        };
        let mut fields = info.split(' ');
        let mode = fields.next().unwrap_or_default();
        let blob = fields.nth(1).unwrap_or_default();
        let is_data = is_file_mode(mode)
            && layout::is_data_path(path)
            && (path != ".gitattributes" || blob_is(git, blob, GITATTRIBUTES)?);
        if !is_data {
            return Err(LenkaError::invalid_file(format!(
                "The branch {BRANCH} of another clone holds a file that is not lenka data: {}. lenka did not merge it. Check that remote.",
                path.escape_default()
            )));
        }
    }
    let incoming = read_settings(git, revision)?;
    if let Some(local) = local {
        check_same_project(local, &incoming)?;
    }
    refuse_markers(
        git,
        Some(revision),
        &format!("The branch {BRANCH} of another clone"),
    )
}

/// Returns the data directory of a clone if it has a project. This call does not run Git.
#[must_use]
pub fn existing_data(cwd: &Path) -> Option<PathBuf> {
    let dir = gitdir::locate(cwd)?.common_dir.join(GIT_DATA_DIR);
    dir.join(PROJECT_FILE).is_file().then_some(dir)
}

/// Creates the branch with no change to the checkout, and makes the data directory a worktree.
fn connect_worktree(common_dir: &Path, dir: &Path) -> Result<()> {
    let git = clone_git(common_dir);
    if !git.passes(["show-ref", "--verify", "--quiet", BRANCH_REF])? {
        if !tracking_refs(&git)?.is_empty() {
            return Err(LenkaError::invalid_argument(format!(
                "The remote has the branch {BRANCH} already. Move {} aside, and run `lenka init` to restore the data.",
                dir.display()
            )));
        }
        let tree = git.run(
            "hash-object",
            ["hash-object", "-w", "-t", "tree", "--stdin"],
        )?;
        let mut args = git.identity_args()?;
        args.extend(
            [
                "commit-tree",
                tree.as_str(),
                "-m",
                "lenka: start the data branch",
            ]
            .map(str::to_owned),
        );
        let commit = git.run("commit-tree", args)?;
        git.run("branch", ["branch", "--no-track", BRANCH, commit.as_str()])?;
    }
    remove_old_connect_dirs(dir);
    forget_absent_worktree(common_dir);
    let mut temp = dir.as_os_str().to_owned();
    temp.push(format!("{CONNECT_SUFFIX}{}", std::process::id()));
    let temp = PathBuf::from(temp);
    let connected = link_worktree(&git, &temp.join(GIT_DATA_DIR), dir);
    #[expect(
        clippy::let_underscore_must_use,
        reason = "A leftover temporary directory does no harm to the data."
    )]
    let _ = fs::remove_dir_all(&temp);
    connected
}

/// Connects an existing data directory to a new worktree, with no move of the data. A writer in
/// another process can continue, and a stop at any step keeps all data at its normal path. The
/// rename of the `.git` file is the last step that changes the meaning of the directory.
fn link_worktree(git: &Git, temp: &Path, dir: &Path) -> Result<()> {
    git.run(
        "worktree add",
        [
            // The `.git` file moves, so it must hold an absolute path.
            OsStr::new("-c"),
            OsStr::new("worktree.useRelativePaths=false"),
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-q"),
            OsStr::new("--no-checkout"),
            temp.as_os_str(),
            OsStr::new(BRANCH),
        ],
    )?;
    let linked = Git::new(temp, Prompt::Allowed);
    linked.run("read-tree", ["read-tree", "HEAD"])?;
    let mut work_tree = OsString::from("--work-tree=");
    work_tree.push(dir);
    // A file on the disk is newer than the branch, so only a missing file comes from the branch.
    let listing = linked.output(["ls-files", "-z"])?;
    for path in listing.stdout.split(|byte| *byte == 0) {
        let path = String::from_utf8_lossy(path);
        if !path.is_empty() && fs::symlink_metadata(dir.join(path.as_ref())).is_err() {
            linked.run(
                "checkout-index",
                [
                    work_tree.as_os_str(),
                    OsStr::new("checkout-index"),
                    OsStr::new("--"),
                    OsStr::new(path.as_ref()),
                ],
            )?;
        }
    }
    fs::rename(temp.join(".git"), dir.join(".git"))
        .map_err(|err| LenkaError::io("Connect the data directory", &err))?;
    git.run(
        "worktree repair",
        [
            OsStr::new("worktree"),
            OsStr::new("repair"),
            dir.as_os_str(),
        ],
    )
    .map(|_| ())
}

/// Removes the temporary directories of an `enable` that stopped. They hold no data.
fn remove_old_connect_dirs(dir: &Path) {
    let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
        return;
    };
    let mut prefix = name.to_os_string();
    prefix.push(CONNECT_SUFFIX);
    let prefix = prefix.to_string_lossy().into_owned();
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "A leftover temporary directory does no harm to the data."
            )]
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Removes the Git record of a data worktree that is absent, so that a new worktree can use the
/// branch. `git worktree prune` is not safe here: it also removes the records of the worktrees of
/// the person, and such a worktree can be absent for a short time.
fn forget_absent_worktree(common_dir: &Path) {
    let Ok(entries) = fs::read_dir(common_dir.join("worktrees")) else {
        return;
    };
    let head = format!("ref: {BRANCH_REF}");
    for entry in entries.flatten() {
        let record = entry.path();
        let holds_branch = read_record(&record.join("HEAD")).is_ok_and(|text| text.trim() == head);
        let absent = read_record(&record.join("gitdir"))
            .is_ok_and(|target| fs::symlink_metadata(record.join(target.trim())).is_err());
        if holds_branch && absent && !record.join("locked").exists() {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Git reports a record that stays."
            )]
            let _ = fs::remove_dir_all(&record);
        }
    }
}

fn add_worktree(git: &Git, dir: &Path) -> Result<()> {
    forget_absent_worktree(&git.dir);
    git.run(
        "worktree add",
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("-q"),
            dir.as_os_str(),
            OsStr::new(BRANCH),
        ],
    )
    .map(|_| ())
}

/// Returns the tracking refs of the data branch, with `origin` first.
fn tracking_refs(git: &Git) -> Result<Vec<String>> {
    let text = git.run(
        "for-each-ref",
        [
            "for-each-ref",
            "--format=%(refname)",
            "refs/remotes/*/lenka-data",
        ],
    )?;
    let mut refs: Vec<String> = text.lines().map(str::to_owned).collect();
    refs.sort_by_key(|name| name != "refs/remotes/origin/lenka-data");
    Ok(refs)
}

/// Finds the data of a fresh clone, with no network call, and makes the worktree at
/// `<git-common-dir>/lenka-data`. The result is the data directory, or `None` if this clone has no
/// data. An existing data directory with `lenka.toml` is also a result.
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidFile`] if the branch has no `lenka.toml`, and [`ErrorCode::Io`] if
/// `git` fails.
pub fn restore(cwd: &Path) -> Result<Option<PathBuf>> {
    let Some(location) = gitdir::locate(cwd) else {
        return Ok(None);
    };
    let dir = location.common_dir.join(GIT_DATA_DIR);
    if dir.join(PROJECT_FILE).is_file() {
        return Ok(Some(dir));
    }
    if fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_some()) {
        return Ok(None);
    }
    let git = clone_git(&location.common_dir);
    let tracking = tracking_refs(&git)?;
    if git.passes(["show-ref", "--verify", "--quiet", BRANCH_REF])? {
        check_incoming_tree(&git, BRANCH_REF, None)?;
    } else {
        let Some(first) = tracking.first() else {
            return Ok(None);
        };
        let incoming = commit_of(&git, first)?;
        check_incoming_tree(&git, &incoming, None)?;
        git.run(
            "branch",
            ["branch", "--no-track", BRANCH, incoming.as_str()],
        )?;
    }
    add_worktree(&git, &dir)?;
    if !dir.join(PROJECT_FILE).is_file() {
        return Err(LenkaError::invalid_file(format!(
            "The branch {BRANCH} has no {PROJECT_FILE}, so it is not lenka data. Remove the worktree {}, or repair the branch.",
            dir.display()
        )));
    }
    let data = Git::new(&dir, Prompt::Allowed);
    let mut settings = LocalSettings::read(&data)?;
    settings.enabled = true;
    if let Some(remote) = tracking
        .first()
        .and_then(|name| remote_of_tracking_ref(name))
    {
        settings.remote = remote.to_owned();
    }
    settings.write(&data)?;
    if let Some(failure) = download_lfs(&data, Some(&settings.remote))? {
        tracing::warn!(failure = %failure, "lenka did not download the Git LFS bytes");
    }
    Ok(Some(dir))
}

/// Returns the remote of a tracking ref of the data branch, if the name is valid.
fn remote_of_tracking_ref(name: &str) -> Option<&str> {
    let remote = name
        .strip_prefix("refs/remotes/")?
        .strip_suffix(&format!("/{BRANCH}"))?;
    validate_remote_name(remote).ok().map(|()| remote)
}

/// The result of [`sync`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    /// Tells if this call made a commit of local data.
    pub committed: bool,
    /// The remote name, if the repository has that remote.
    pub remote: Option<String>,
    /// Tells if a merge changed local files. If so, the caller must rebuild the task index, and
    /// then call [`mark_indexed`].
    pub merged: bool,
    /// Tells if the data reached the remote.
    pub pushed: bool,
    /// The failure of the push. The merge and the commit stay stored.
    pub push_error: Option<String>,
    /// The files in conflict. If this list is not empty, the merge stopped and nothing changed.
    pub conflict: Vec<String>,
    /// The files that are not lenka data, so they stay in this clone and go in no commit.
    pub skipped: Vec<String>,
    /// The sections of the shared settings that the merge changed.
    pub settings_changed: Vec<String>,
    /// The failure of a Git LFS download. `doctor` lists the artifacts that are not downloaded.
    pub lfs_warning: Option<String>,
}

impl SyncReport {
    /// Tells if the merge stopped on a conflict.
    #[must_use]
    pub fn has_conflict(&self) -> bool {
        !self.conflict.is_empty()
    }

    /// Tells if the sync did all of its work.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.has_conflict() && self.push_error.is_none()
    }

    fn note_commit(&mut self, stored: Stored) {
        self.committed |= stored.committed;
        for file in stored.skipped {
            if !self.skipped.contains(&file) {
                self.skipped.push(file);
            }
        }
    }
}

/// Commits the data, fetches, merges, and pushes. A conflict stops the merge, and the report lists
/// the files. lenka has no automatic merge. A failed push is in the report, because the merge is
/// complete at that point. After a merge, the caller rebuilds the task index, and then calls
/// [`mark_indexed`].
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidArgument`] if the project is not connected to Git, the selected
/// remote does not exist, or a merge of a person is open. Returns [`ErrorCode::InvalidFile`] or
/// [`ErrorCode::FormatTooNew`] for a branch that lenka refuses. Returns [`ErrorCode::Io`] if `git`
/// fails, for example on unrelated histories.
pub fn sync(project: &Project, secrets: OutgoingSecrets) -> Result<SyncReport> {
    let git = open_data_repo(project, Prompt::Allowed)?;
    let settings = LocalSettings::read(&git)?;
    if !settings.enabled {
        return Err(LenkaError::invalid_argument(
            "Git is off for this project. Run `lenka git enable` first.",
        ));
    }
    let mut report = SyncReport {
        // A stopped process can leave a merge with no index rebuild.
        merged: git.local_value(KEY_NEEDS_REINDEX)?.is_some(),
        ..SyncReport::default()
    };
    report.note_commit(commit_locked(project, &git, "lenka: sync")?);
    let remote = settings.remote.as_str();
    if !has_remote(&git, remote)? {
        let others = git.run("remote", ["remote"])?;
        if others.is_empty() {
            return Ok(report);
        }
        return Err(LenkaError::invalid_argument(format!(
            "This clone has no remote {remote}. Select another remote with `lenka git enable --remote <name>`, or add the remote."
        )));
    }
    report.remote = Some(remote.to_owned());

    let tracking = format!("refs/remotes/{remote}/{BRANCH}");
    let listing = git.output(["ls-remote", "--exit-code", "--heads", remote, BRANCH_REF])?;
    match listing.status.code() {
        Some(0) => {
            let refspec = format!("+{BRANCH_REF}:{tracking}");
            git.run(
                "fetch",
                ["fetch", "-q", "--no-tags", remote, refspec.as_str()],
            )?;
            project.write_locked(|_| {
                report.note_commit(commit_all(&git, "lenka: sync")?);
                merge(&git, &tracking, &mut report)
            })?;
            if report.has_conflict() {
                return Ok(report);
            }
            if report.merged {
                report.lfs_warning = download_lfs(&git, Some(remote))?;
            }
        }
        // The remote has no data branch yet, so this push creates it.
        Some(2) => {}
        _ => return Err(git_error("ls-remote", &listing)),
    }
    match check_outgoing(&git, remote, secrets).and_then(|()| push(&git, remote)) {
        Ok(()) => report.pushed = true,
        Err(error) => report.push_error = Some(error.to_string()),
    }
    Ok(report)
}

/// Records that the caller rebuilt the task index after a merge.
///
/// # Errors
///
/// Returns an error if the project is not connected to Git, or if `git` fails.
pub fn mark_indexed(project: &Project) -> Result<()> {
    let git = open_data_repo(project, Prompt::Allowed)?;
    git.unset_local_value(KEY_NEEDS_REINDEX)
}

/// Returns the commit ID of a ref.
fn commit_of(git: &Git, name: &str) -> Result<String> {
    let revision = format!("{name}^{{commit}}");
    git.run(
        "rev-parse",
        ["rev-parse", "--verify", "--quiet", revision.as_str()],
    )
}

fn merge(git: &Git, tracking: &str, report: &mut SyncReport) -> Result<()> {
    let local = read_settings(git, "HEAD")?;
    // Another process can move the ref, so the check and the merge use one commit ID.
    let incoming = commit_of(git, tracking)?;
    let tracking = incoming.as_str();
    check_incoming_tree(git, tracking, Some(&local))?;
    let before = git.run("rev-parse", ["rev-parse", "HEAD"])?;
    let was_pending = report.merged;
    git.set_local_value(KEY_NEEDS_REINDEX, "true")?;
    let result = run_merge(git, tracking, &before, &local, report);
    if !was_pending && !report.merged {
        git.unset_local_value(KEY_NEEDS_REINDEX)?;
    }
    result
}

fn run_merge(
    git: &Git,
    tracking: &str,
    before: &str,
    local: &ProjectConfig,
    report: &mut SyncReport,
) -> Result<()> {
    let mut args = git.identity_args()?;
    args.extend(
        [
            "-c",
            "merge.ff=true",
            "merge",
            "-q",
            "--no-edit",
            "--no-verify",
            "--no-verify-signatures",
            tracking,
        ]
        .map(str::to_owned),
    );
    let output = git.output(args)?;
    if output.status.success() {
        if git.run("rev-parse", ["rev-parse", "HEAD"])? == before {
            return Ok(());
        }
        // Two valid files can merge to a file that is not valid.
        let merged = read_settings(git, "HEAD")
            .and_then(|merged| check_same_project(local, &merged).map(|()| merged));
        return match merged {
            Ok(merged) => {
                report.merged = true;
                report.settings_changed = local
                    .changed_sections(&merged)
                    .into_iter()
                    .map(str::to_owned)
                    .collect();
                Ok(())
            }
            Err(error) => {
                restore_tree(git, before)?;
                Err(error)
            }
        };
    }
    let unmerged = git.run("diff", ["diff", "--name-only", "--diff-filter=U", "-z"])?;
    let files: Vec<String> = unmerged
        .split('\0')
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    if files.is_empty() {
        restore_tree(git, before)?;
        return Err(git_error("merge", &output));
    }
    git.run("merge --abort", ["merge", "--abort"])?;
    report.conflict = files;
    Ok(())
}

/// Puts the data back to a commit after a failed merge. The caller holds the lock and committed
/// all data just before the merge, so each untracked data file came from the failed merge.
fn restore_tree(git: &Git, commit: &str) -> Result<()> {
    git.run("reset", ["reset", "-q", "--hard", commit])?;
    let output = git.output(["ls-files", "-o", "-z"])?;
    for path in output.stdout.split(|byte| *byte == 0) {
        let text = String::from_utf8_lossy(path);
        if layout::is_data_path(&text) && text != PROJECT_FILE {
            fs::remove_file(git.dir.join(text.as_ref()))
                .map_err(|err| LenkaError::io("Remove a file of a failed merge", &err))?;
        }
    }
    Ok(())
}

/// The Git plugin.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitPlugin;

/// Returns the Git plugin.
#[must_use]
pub fn plugin() -> GitPlugin {
    GitPlugin
}

impl Plugin for GitPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "git",
            description: "Commit, push, and restore data with system Git. It uses the network",
            sensitive: true,
        }
    }

    fn register(&self, registry: &mut Registry) {
        registry.hook(Arc::new(GitHook));
    }
}

#[derive(Debug)]
struct GitHook;

impl GitHook {
    fn commit_and_push(project: &Project, message: &str, finished: bool) -> Result<()> {
        // With no `.git` here, `git` could find a parent repository of the user.
        let Some(dir) = project.driver().local_dir() else {
            return Ok(());
        };
        if fs::symlink_metadata(dir.join(".git")).is_err() {
            return Ok(());
        }
        let settings = LocalSettings::read(&Git::new(dir, Prompt::Denied))?;
        if !settings.enabled {
            return Ok(());
        }
        let git = open_data_repo(project, Prompt::Denied)?;
        commit_locked(project, &git, message)?;
        if finished && settings.push_on_finish && has_remote(&git, &settings.remote)? {
            check_outgoing(&git, &settings.remote, OutgoingSecrets::Refuse)?;
            push(&git, &settings.remote)?;
        }
        Ok(())
    }
}

impl EventHook for GitHook {
    fn on_event(&self, project: &Project, event: &ProjectEvent) -> Result<()> {
        let result = match event {
            ProjectEvent::StageChanged { task } => Self::commit_and_push(
                project,
                &format!("lenka: stage change of task {task}"),
                false,
            ),
            ProjectEvent::TaskFinished { task } => {
                Self::commit_and_push(project, &format!("lenka: finish task {task}"), true)
            }
        };
        if let Err(error) = &result {
            tracing::warn!(error = %error, "the Git plugin did not store the change");
        }
        result
    }
}

/// Reads a small record file that Git keeps for a worktree.
fn read_record(file: &Path) -> std::io::Result<String> {
    const RECORD_LIMIT: u64 = 64 * 1024;
    files::read_text_limited(file, files::Links::Refuse, RECORD_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_do_not_reach_an_error() {
        assert_eq!(
            scrub_credentials("fatal: unable to access 'https://me:token@example.com/a.git/': no"),
            "fatal: unable to access 'https://***@example.com/a.git/': no"
        );
        assert_eq!(
            scrub_credentials("https://example.com/a"),
            "https://example.com/a"
        );
        assert_eq!(
            scrub_credentials("ssh://git@host:22/x y://z"),
            "ssh://***@host:22/x y://z"
        );
        assert_eq!(scrub_credentials("plain text"), "plain text");
    }

    #[test]
    fn a_remote_name_cannot_be_an_option() {
        assert!(validate_remote_name("origin").is_ok());
        assert!(validate_remote_name("my-remote.1").is_ok());
        for bad in ["", "--upload-pack=x", "-x", "a b", "a/b", "a;b"] {
            assert!(validate_remote_name(bad).is_err(), "{bad}");
        }
    }
}
