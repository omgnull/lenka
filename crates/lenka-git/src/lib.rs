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

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use lenka_core::gitdir;
use lenka_core::init::GIT_DATA_DIR;
use lenka_core::{EventHook, Plugin, PluginInfo, Project, ProjectEvent, Registry};
use lenka_plugin_api::{ErrorCode, LenkaError, Result};
use serde::Serialize;

/// The name of the data branch.
pub const BRANCH: &str = "lenka-data";

const BRANCH_REF: &str = "refs/heads/lenka-data";

/// The file that replaces the Git excludes of the user, relative to a data directory.
const EXCLUDES_FILE: &str = ".runtime/git-excludes";

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
];

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

    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .env("LC_ALL", "C");
        for name in CLEARED_ENV {
            command.env_remove(name);
        }
        if self.prompt == Prompt::Denied {
            command.env("GIT_TERMINAL_PROMPT", "0");
        }
        // The hook path is outside the data, so that no data from a remote can become a hook.
        command.args([
            "-c",
            "core.hooksPath=/dev/null",
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
    let text = String::from_utf8_lossy(&output.stderr);
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

/// Stages all data and commits it. The result tells if a commit exists.
fn commit_all(git: &Git, message: &str) -> Result<bool> {
    // The excludes of the user must not hide an artifact, and `.runtime` must stay out even if
    // a person removed `.gitignore`. This file replaces the excludes of the user for this call.
    let file = git.dir.join(EXCLUDES_FILE);
    fs::write(&file, ".runtime/\n")
        .map_err(|err| LenkaError::io("Write the Git excludes", &err))?;
    let mut excludes = OsString::from("core.excludesFile=");
    excludes.push(&file);
    git.run(
        "add",
        [
            OsStr::new("-c"),
            &excludes,
            OsStr::new("add"),
            OsStr::new("-A"),
            OsStr::new("."),
        ],
    )?;
    if git.passes(["diff", "--cached", "--quiet"])? {
        return Ok(false);
    }
    let mut args = git.identity_args()?;
    args.extend(["commit", "--no-verify", "-q", "-m", message].map(str::to_owned));
    git.run("commit", args)?;
    Ok(true)
}

/// Commits all data of a project under the project write lock, so that a commit never holds half
/// of a write.
fn commit_locked(project: &Project, git: &Git, message: &str) -> Result<bool> {
    project.write_locked(|_| commit_all(git, message))
}

fn has_remote(git: &Git, remote: &str) -> Result<bool> {
    git.passes(["remote", "get-url", "--", remote])
}

/// Sends the branch to a remote. The hooks stay off, so this function uploads Git LFS objects
/// itself. Without them, a fresh clone cannot download the artifact bytes.
fn push(git: &Git, remote: &str) -> Result<()> {
    if git.passes(["config", "--get", "filter.lfs.clean"])? && git.passes(["lfs", "version"])? {
        git.run("lfs push", ["lfs", "push", remote, BRANCH])?;
    }
    let refspec = format!("{BRANCH_REF}:{BRANCH_REF}");
    git.run("push", ["push", "-q", remote, refspec.as_str()])?;
    Ok(())
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
    /// The remote name in the project settings.
    pub remote: String,
    /// Tells if the repository has that remote now.
    pub remote_found: bool,
    /// Tells if this call made a commit.
    pub committed: bool,
}

/// Connects a project to Git, sets `[git] enabled = true`, and makes the first commit. A repeat
/// call is safe. Stop other lenka processes of this project first, because the first call moves
/// the data directory for a short time.
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
    }
    let git = open_data_repo(project, Prompt::Allowed)?;

    let mut config = (*project.config()).clone();
    if let Some(value) = &options.remote {
        config.git.remote = configure_remote(&git, layout, value)?;
    }
    config.git.enabled = true;
    config.git.push_on_finish = config.git.push_on_finish || options.push_on_finish;
    validate_remote_name(&config.git.remote)?;
    let remote = config.git.remote.clone();
    if config != *project.config() {
        project.save_config(config)?;
    }
    let committed = commit_locked(project, &git, "lenka: connect the data to Git")?;
    Ok(EnableReport {
        layout,
        remote_found: has_remote(&git, &remote)?,
        remote,
        committed,
    })
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
    const NAME: &str = "origin";
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

/// Tells if a file from another clone can be data. `.runtime` holds local state, such as the write
/// journal, and a replay of a journal from a remote can change any file.
fn is_data_path(path: &str) -> bool {
    matches!(path, "lenka.toml" | ".gitignore" | ".gitattributes")
        || ["tasks/", "artifacts/", "memory/"]
            .iter()
            .any(|prefix| path.starts_with(prefix))
}

/// Refuses a commit from another clone if its tree holds anything other than plain data files.
fn check_incoming_tree(git: &Git, revision: &str) -> Result<()> {
    let listing = git.run("ls-tree", ["ls-tree", "-r", "-z", "--full-tree", revision])?;
    for row in listing.split('\0').filter(|row| !row.is_empty()) {
        let Some((info, path)) = row.split_once('\t') else {
            continue;
        };
        let mode = info.split(' ').next().unwrap_or_default();
        if !matches!(mode, "100644" | "100755") || !is_data_path(path) {
            return Err(LenkaError::invalid_file(format!(
                "The branch {BRANCH} of another clone holds a file that is not lenka data: {}. lenka did not merge it. Check that remote.",
                path.escape_default()
            )));
        }
    }
    Ok(())
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
    let mut aside = dir.as_os_str().to_owned();
    aside.push(format!(".aside-{}", std::process::id()));
    let aside = PathBuf::from(aside);
    fs::rename(dir, &aside).map_err(|err| LenkaError::io("Move the data directory aside", &err))?;
    let added = add_worktree(&git, dir);
    if let Err(error) = added {
        let _ = fs::remove_dir_all(dir);
        fs::rename(&aside, dir)
            .map_err(|err| LenkaError::io("Move the data directory back", &err))?;
        return Err(error);
    }
    move_contents(&aside, dir).map_err(|err| {
        LenkaError::new(
            ErrorCode::Io,
            format!(
                "Move the data back: {err}. The rest of the data is in {}.",
                aside.display()
            ),
        )
    })?;
    fs::remove_dir_all(&aside).map_err(|err| LenkaError::io("Remove the empty directory", &err))
}

fn add_worktree(git: &Git, dir: &Path) -> Result<()> {
    git.run("worktree prune", ["worktree", "prune"])?;
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

/// Moves all entries of a directory into another directory. A file in the source replaces the
/// file in the target, because the source holds the newest data.
fn move_contents(from: &Path, to: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        match fs::symlink_metadata(&target) {
            Ok(meta) if meta.is_dir() && entry.file_type()?.is_dir() => {
                move_contents(&entry.path(), &target)?;
                fs::remove_dir(entry.path())?;
                continue;
            }
            Ok(meta) if meta.is_dir() => fs::remove_dir_all(&target)?,
            Ok(_) => fs::remove_file(&target)?,
            Err(_) => {}
        }
        fs::rename(entry.path(), target)?;
    }
    Ok(())
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
    if dir.join(lenka_core::settings::PROJECT_FILE).is_file() {
        return Ok(Some(dir));
    }
    if fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_some()) {
        return Ok(None);
    }
    let git = clone_git(&location.common_dir);
    if git.passes(["show-ref", "--verify", "--quiet", BRANCH_REF])? {
        check_incoming_tree(&git, BRANCH_REF)?;
    } else {
        let Some(tracking) = tracking_refs(&git)?.into_iter().next() else {
            return Ok(None);
        };
        check_incoming_tree(&git, &tracking)?;
        git.run(
            "branch",
            ["branch", "--no-track", BRANCH, tracking.as_str()],
        )?;
    }
    add_worktree(&git, &dir)?;
    if !dir.join(lenka_core::settings::PROJECT_FILE).is_file() {
        return Err(LenkaError::invalid_file(format!(
            "The branch {BRANCH} has no lenka.toml, so it is not lenka data. Remove the worktree {}, or repair the branch.",
            dir.display()
        )));
    }
    Ok(Some(dir))
}

/// The result of [`sync`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    /// Tells if this call made a commit of local data.
    pub committed: bool,
    /// The remote name, if the repository has that remote.
    pub remote: Option<String>,
    /// Tells if the merge changed local files. If so, the caller must rebuild the task index.
    pub merged: bool,
    /// Tells if the data reached the remote.
    pub pushed: bool,
    /// The files in conflict. If this list is not empty, the merge stopped and nothing changed.
    pub conflict: Vec<String>,
}

impl SyncReport {
    /// Tells if the merge stopped on a conflict.
    #[must_use]
    pub fn has_conflict(&self) -> bool {
        !self.conflict.is_empty()
    }
}

/// Commits the data, fetches, merges, and pushes. A conflict stops the merge, and the report lists
/// the files. lenka has no automatic merge. After a merge, the caller rebuilds the task index.
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidArgument`] if the project is not connected to Git, and
/// [`ErrorCode::Io`] if `git` fails, for example on unrelated histories.
pub fn sync(project: &Project) -> Result<SyncReport> {
    let settings = project.config().git.clone();
    if !settings.enabled {
        return Err(LenkaError::invalid_argument(
            "Git is off for this project. Run `lenka git enable` first.",
        ));
    }
    validate_remote_name(&settings.remote)?;
    let git = open_data_repo(project, Prompt::Allowed)?;
    let mut report = SyncReport {
        committed: commit_locked(project, &git, "lenka: sync")?,
        ..SyncReport::default()
    };
    if !has_remote(&git, &settings.remote)? {
        return Ok(report);
    }
    let remote = settings.remote.as_str();
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
                report.committed |= commit_all(&git, "lenka: sync")?;
                merge(&git, &tracking, &mut report)
            })?;
            if report.has_conflict() {
                return Ok(report);
            }
        }
        // The remote has no data branch yet, so this push creates it.
        Some(2) => {}
        _ => return Err(git_error("ls-remote", &listing)),
    }
    push(&git, remote)?;
    report.pushed = true;
    Ok(report)
}

fn merge(git: &Git, tracking: &str, report: &mut SyncReport) -> Result<()> {
    check_incoming_tree(git, tracking)?;
    let before = git.run("rev-parse", ["rev-parse", "HEAD"])?;
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
        report.merged = git.run("rev-parse", ["rev-parse", "HEAD"])? != before;
        return Ok(());
    }
    let unmerged = git.run("diff", ["diff", "--name-only", "--diff-filter=U", "-z"])?;
    let files: Vec<String> = unmerged
        .split('\0')
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    if files.is_empty() {
        return Err(git_error("merge", &output));
    }
    git.run("merge --abort", ["merge", "--abort"])?;
    report.conflict = files;
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
        let settings = project.config().git.clone();
        validate_remote_name(&settings.remote)?;
        let git = open_data_repo(project, Prompt::Denied)?;
        commit_locked(project, &git, message)?;
        if finished && settings.push_on_finish && has_remote(&git, &settings.remote)? {
            push(&git, &settings.remote)?;
        }
        Ok(())
    }
}

impl EventHook for GitHook {
    fn on_event(&self, project: &Project, event: &ProjectEvent) -> Result<()> {
        if !project.config().git.enabled {
            return Ok(());
        }
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

    #[test]
    fn move_contents_merges_directories() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let from = temp.path().join("from");
        let to = temp.path().join("to");
        fs::create_dir_all(from.join("d"))?;
        fs::create_dir_all(to.join("d"))?;
        fs::write(from.join("d/new"), "new")?;
        fs::write(from.join("same"), "ours")?;
        fs::write(to.join("same"), "theirs")?;
        fs::write(to.join("d/old"), "old")?;
        move_contents(&from, &to)?;
        assert_eq!(fs::read_to_string(to.join("d/new"))?, "new");
        assert_eq!(fs::read_to_string(to.join("d/old"))?, "old");
        assert_eq!(fs::read_to_string(to.join("same"))?, "ours");
        assert_eq!(fs::read_dir(&from)?.count(), 0);
        Ok(())
    }
}
