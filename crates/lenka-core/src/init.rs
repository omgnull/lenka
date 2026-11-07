// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Project creation, project attach, and the call context of a process.

use std::fs;
use std::path::{Path, PathBuf};

use lenka_plugin_api::{Actor, Caller, ErrorCode, LenkaError, ProjectScope, Result};

use crate::gitdir;
use crate::notices::{agent_key, person_key};
use crate::settings::{
    CLONE_FILE, CloneFile, PROJECT_FILE, Paths, ProjectConfig, ProjectEntry, ProjectsFile,
    validate_project_name,
};

/// The name of the default data directory in the Git common directory.
pub const GIT_DATA_DIR: &str = "lenka-data";

/// The contents of `.gitignore` in a data directory.
const GITIGNORE: &str = ".runtime/\n.tmp-*\n";

/// The contents of `.gitattributes` in a data directory. A branch from another clone must have
/// this text, because other text can change how Git reads and writes the data.
pub const GITATTRIBUTES: &str = "artifacts/*/versions/** filter=lfs diff=lfs merge=lfs -text\n";

/// Where a new project stores its data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// In `<git-common-dir>/lenka-data`, or in the current directory outside Git.
    Default,
    /// In the user data directory, for many repositories.
    Shared,
    /// In a directory that a person selects.
    Directory(PathBuf),
}

/// The result of a project creation.
#[derive(Debug, Clone)]
pub struct Created {
    /// The project name.
    pub name: String,
    /// The data directory.
    pub directory: PathBuf,
    /// The clone file that lists the project, if the caller asked for one.
    pub clone_file: Option<PathBuf>,
    /// The reason that the project has no clone file, after [`Attach::IfPossible`].
    pub attach_warning: Option<String>,
}

/// Tells if a new project connects to the directory of the caller.
///
/// An agent in a connected directory, or in a directory below it, uses the project by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attach {
    /// Connect the directory. A failure is an error.
    Required,
    /// Connect the directory. A failure gives a warning, and the project stays.
    IfPossible,
    /// Do not connect the directory.
    Never,
}

/// The clone file for a directory: in the Git common directory, or in the user configuration.
#[must_use]
pub fn clone_file_for(paths: &Paths, dir: &Path) -> PathBuf {
    match gitdir::locate(dir) {
        Some(location) => location.common_dir.join(CLONE_FILE),
        None => {
            paths.plain_clone_file(&gitdir::real_path(dir).unwrap_or_else(|_| dir.to_path_buf()))
        }
    }
}

/// Finds an existing clone file for a directory.
///
/// A file in a checkout or a plain directory can come from another author, so only the Git
/// directory and the user configuration can select the projects of the user.
#[must_use]
pub fn find_clone_file(paths: &Paths, start: &Path) -> Option<PathBuf> {
    if let Some(location) = gitdir::locate(start) {
        let file = location.common_dir.join(CLONE_FILE);
        if file.is_file() {
            return Some(file);
        }
        // A person can run `git init` in a plain project, and the project stays connected.
        let plain = paths.plain_clone_file(&location.worktree);
        if plain.is_file() {
            return Some(plain);
        }
        // A linked worktree uses the connection of the main worktree.
        let main = paths.plain_clone_file(location.common_dir.parent()?);
        return main.is_file().then_some(main);
    }
    plain_project_root(paths, start).map(|root| paths.plain_clone_file(&root))
}

/// Finds the plain directory with a connection to a project, from a directory upward.
#[must_use]
pub fn plain_project_root(paths: &Paths, start: &Path) -> Option<PathBuf> {
    if gitdir::locate(start).is_some() {
        return None;
    }
    let start = gitdir::real_path(start).ok()?;
    find_plain_root(&start, std::env::home_dir().as_deref(), |dir| {
        paths.plain_clone_file(dir).is_file()
    })
}

/// Searches a plain directory and its parents. A connection of the home directory applies only to
/// the home directory, because all projects of a user are below it.
fn find_plain_root(
    start: &Path,
    home: Option<&Path>,
    is_connected: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let home = home.and_then(|home| gitdir::real_path(home).ok());
    for dir in start.ancestors() {
        if home.as_deref() == Some(dir) && dir != start {
            return None;
        }
        if is_connected(dir) {
            return Some(dir.to_path_buf());
        }
    }
    None
}

/// Tells if a directory is in a Git clone.
#[must_use]
pub fn in_git_clone(dir: &Path) -> bool {
    gitdir::locate(dir).is_some()
}

/// Returns the storage driver of the project in a directory. A directory with no project gives
/// `None`.
///
/// # Errors
///
/// Returns an error if the settings file cannot be read or is not valid.
pub fn project_driver_in(directory: &Path) -> Result<Option<String>> {
    let file = directory.join(PROJECT_FILE);
    if !file.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(&file).map_err(|err| LenkaError::io("Read lenka.toml", &err))?;
    Ok(Some(ProjectConfig::parse(&text)?.storage.driver))
}

/// Returns the default project name of a directory: the name of the Git worktree or the directory.
#[must_use]
pub fn default_name(dir: &Path) -> String {
    let base = gitdir::locate(dir).map_or_else(|| dir.to_path_buf(), |location| location.worktree);
    let raw = gitdir::real_path(&base)
        .unwrap_or(base)
        .file_name()
        .map_or_else(
            || "project".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let clean = clean.trim_start_matches('.');
    if clean.is_empty() {
        "project".to_owned()
    } else {
        clean.chars().take(64).collect()
    }
}

/// Returns the default data directory of a directory.
#[must_use]
pub fn default_data_dir(cwd: &Path) -> PathBuf {
    match gitdir::locate(cwd) {
        Some(location) => location.common_dir.join(GIT_DATA_DIR),
        None => cwd.join("lenka-data"),
    }
}

/// Creates a project, and connects the current directory to it.
///
/// # Errors
///
/// Returns an error if the name is taken, the directory already holds a project, or a write fails.
pub fn create_project(
    paths: &Paths,
    cwd: &Path,
    name: Option<&str>,
    location: &Location,
) -> Result<Created> {
    create_project_with(paths, cwd, name, location, Attach::Required)
}

/// Creates a project, with a choice about the connection to the current directory.
///
/// # Errors
///
/// Returns an error if the name is taken, the directory already holds a project, or a write fails.
pub fn create_project_with(
    paths: &Paths,
    cwd: &Path,
    name: Option<&str>,
    location: &Location,
    attach: Attach,
) -> Result<Created> {
    let name = name.map_or_else(|| default_name(cwd), str::to_owned);
    validate_project_name(&name)?;
    let directory = match location {
        Location::Default => default_data_dir(cwd),
        Location::Shared => paths.shared_project_dir(&name),
        Location::Directory(dir) => cwd.join(dir),
    };
    // Other processes start in other directories, so the entry holds an absolute path.
    let directory = std::path::absolute(&directory)
        .map_err(|err| LenkaError::io("Find the directory", &err))?;
    let directory = without_dot_parts(&directory);
    // The lock also covers the check for `lenka.toml`, so two processes cannot make two IDs.
    paths.with_user_lock(|| create_locked(paths, cwd, name, directory, attach))
}

fn create_locked(
    paths: &Paths,
    cwd: &Path,
    name: String,
    directory: PathBuf,
    attach: Attach,
) -> Result<Created> {
    let mut projects = ProjectsFile::load(paths)?;
    refuse_nested_project(&projects, &name, &directory)?;
    refuse_symlink(&directory)?;
    if let Some(entry) = projects.projects.get(&name)
        && entry.directory != directory
    {
        return Err(LenkaError::invalid_argument(format!(
            "The project name {name} is already in use at another directory. Select another name."
        )));
    }
    let settings_file = directory.join(PROJECT_FILE);
    let config = if settings_file.is_file() {
        let text = fs::read_to_string(&settings_file)
            .map_err(|err| LenkaError::io("Read lenka.toml", &err))?;
        ProjectConfig::parse(&text)?
    } else {
        if holds_other_files(&directory)? {
            return Err(LenkaError::invalid_argument(
                "This directory holds other files, but no lenka project. Select an empty directory or a new one.",
            ));
        }
        let config = ProjectConfig::new(&name);
        write_new_project(&directory, &config)?;
        config
    };
    projects.projects.insert(
        name.clone(),
        ProjectEntry {
            id: config.id,
            driver: config.storage.driver.clone(),
            directory: directory.clone(),
        },
    );
    projects.save(paths)?;
    let (clone_file, attach_warning) = match attach {
        Attach::Never => (None, None),
        Attach::Required => (Some(attach_to_clone(paths, cwd, &name)?), None),
        Attach::IfPossible => match attach_to_clone(paths, cwd, &name) {
            Ok(file) => (Some(file), None),
            Err(error) => (
                None,
                Some(format!(
                    "lenka created the project, but cannot connect {} to it. {}",
                    cwd.display(),
                    error.message()
                )),
            ),
        },
    };
    Ok(Created {
        name,
        directory,
        clone_file,
        attach_warning,
    })
}

/// Removes `.` and `..` from an absolute path, so that one directory has one form.
fn without_dot_parts(path: &Path) -> PathBuf {
    let mut clean = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other),
        }
    }
    clean
}

/// Refuses a data directory in another data directory, or around one.
fn refuse_nested_project(projects: &ProjectsFile, name: &str, directory: &Path) -> Result<()> {
    for (other, entry) in &projects.projects {
        let different = other != name && entry.directory != directory;
        if different
            && (directory.starts_with(&entry.directory) || entry.directory.starts_with(directory))
        {
            return Err(LenkaError::invalid_argument(format!(
                "This directory overlaps the data directory of the project {other}. Select another directory."
            )));
        }
    }
    Ok(())
}

/// Refuses a symlink before the first write, because all later commands refuse it.
fn refuse_symlink(directory: &Path) -> Result<()> {
    match fs::symlink_metadata(directory) {
        Ok(meta) if meta.file_type().is_symlink() => Err(LenkaError::invalid_argument(
            "The data directory must be a real directory, not a symlink.",
        )),
        _ => Ok(()),
    }
}

/// Tells if a directory has files that a stopped `lenka init` and the OS did not make. A retry can
/// then continue in that directory.
fn holds_other_files(directory: &Path) -> Result<bool> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(LenkaError::io("Read the data directory", &err)),
    };
    for entry in entries {
        let entry = entry.map_err(|err| LenkaError::io("Read the data directory", &err))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let allowed = match name.as_ref() {
            // These names can also be files of the person, and a create must not replace them.
            ".gitignore" => fs::read(entry.path()).is_ok_and(|text| text == GITIGNORE.as_bytes()),
            ".gitattributes" => {
                fs::read(entry.path()).is_ok_and(|text| text == GITATTRIBUTES.as_bytes())
            }
            ".runtime" | ".DS_Store" | "Thumbs.db" | "desktop.ini" => true,
            "tasks" | "artifacts" | "memory" => {
                fs::read_dir(entry.path()).is_ok_and(|mut inner| inner.next().is_none())
            }
            other => other.starts_with(crate::layout::TEMP_PREFIX),
        };
        if !allowed {
            return Ok(true);
        }
    }
    Ok(false)
}

fn write_new_project(directory: &Path, config: &ProjectConfig) -> Result<()> {
    for part in [
        "tasks",
        "artifacts",
        "memory",
        ".runtime/inbox",
        ".runtime/claims",
    ] {
        fs::create_dir_all(directory.join(part))
            .map_err(|err| LenkaError::io("Create the data directory", &err))?;
    }
    let files = [
        (".gitignore", GITIGNORE.to_owned()),
        (".gitattributes", GITATTRIBUTES.to_owned()),
        (PROJECT_FILE, config.to_text()?),
    ];
    for (name, text) in files {
        // `lenka.toml` comes last, and a rename makes it complete or absent.
        let temp = directory.join(format!(
            "{}init-{}",
            crate::layout::TEMP_PREFIX,
            std::process::id()
        ));
        // A flush comes first, because a power failure can keep the rename and lose the text.
        fs::File::create(&temp)
            .and_then(|mut file| {
                std::io::Write::write_all(&mut file, text.as_bytes())?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&temp, directory.join(name)))
            .map_err(|err| LenkaError::io("Write a project file", &err))?;
    }
    Ok(())
}

/// Registers an existing data directory in the user project list, for a fresh clone.
///
/// # Errors
///
/// Returns an error if the directory has no valid settings.
pub fn register_existing(
    paths: &Paths,
    cwd: &Path,
    name: Option<&str>,
    directory: &Path,
) -> Result<Created> {
    // A later process can run in another directory, so the list needs an absolute path.
    let directory = cwd.join(directory);
    let directory = directory.as_path();
    let text = fs::read_to_string(directory.join(PROJECT_FILE))
        .map_err(|err| LenkaError::io("Read lenka.toml", &err))?;
    let config = ProjectConfig::parse(&text)?;
    let name = name.map_or_else(|| config.name.clone(), str::to_owned);
    validate_project_name(&name)?;
    let clone_file = paths.with_user_lock(|| {
        let mut projects = ProjectsFile::load(paths)?;
        if let Some(entry) = projects.projects.get(&name)
            && entry.directory != directory
        {
            return Err(LenkaError::invalid_argument(format!(
                "The project name {name} is already in use at another directory. Select another name."
            )));
        }
        projects.projects.insert(
            name.clone(),
            ProjectEntry {
                id: config.id,
                driver: config.storage.driver.clone(),
                directory: directory.to_path_buf(),
            },
        );
        projects.save(paths)?;
        attach_to_clone(paths, cwd, &name)
    })?;
    Ok(Created {
        name,
        directory: directory.to_path_buf(),
        clone_file: Some(clone_file),
        attach_warning: None,
    })
}

/// Connects the clone of a directory to a project in the user list.
///
/// # Errors
///
/// Returns [`ErrorCode::ProjectNotAvailable`] if the project is not in the user list.
pub fn attach(paths: &Paths, cwd: &Path, name: &str) -> Result<PathBuf> {
    paths.with_user_lock(|| attach_locked(paths, cwd, name))
}

fn attach_locked(paths: &Paths, cwd: &Path, name: &str) -> Result<PathBuf> {
    let projects = ProjectsFile::load(paths)?;
    if !projects.projects.contains_key(name) {
        let names: Vec<&str> = projects.projects.keys().map(String::as_str).collect();
        return Err(LenkaError::new(
            ErrorCode::ProjectNotAvailable,
            format!(
                "The project {name} is not in the user list. Known projects: {}.",
                names.join(", ")
            ),
        ));
    }
    attach_to_clone(paths, cwd, name)
}

fn attach_to_clone(paths: &Paths, cwd: &Path, name: &str) -> Result<PathBuf> {
    let file = find_clone_file(paths, cwd).unwrap_or_else(|| clone_file_for(paths, cwd));
    let mut clone = CloneFile::load(&file)?.unwrap_or_default();
    clone.add(name);
    clone.save(&file)?;
    Ok(file)
}

/// How a process names itself in a call context.
#[derive(Debug, Clone)]
pub struct CallerSetup {
    /// The kind of actor.
    pub actor: Actor,
    /// The author label.
    pub label: String,
    /// A project from `--project` or `LENKA_PROJECT`.
    pub project: Option<String>,
    /// The optional agent name from `LENKA_AGENT`.
    pub agent: Option<String>,
}

/// Builds the call context of a process from its directory.
///
/// # Errors
///
/// Returns an error if the clone file is not valid.
pub fn caller_for(paths: &Paths, cwd: &Path, setup: CallerSetup) -> Result<Caller> {
    let clone_file = find_clone_file(paths, cwd);
    // A plain directory has one identity in all of its subdirectories, as a Git worktree has.
    let worktree = match (gitdir::locate(cwd), plain_project_root(paths, cwd)) {
        (Some(location), _) => location.worktree,
        (None, Some(root)) => root,
        (None, None) => gitdir::real_path(cwd).unwrap_or_else(|_| cwd.to_path_buf()),
    };
    let clone = match clone_file {
        Some(file) => CloneFile::load(&file)?,
        None => None,
    };
    let (scope, default_project) = match clone {
        Some(clone) => (ProjectScope::Listed(clone.projects), clone.default),
        None if setup.actor == Actor::Human => (ProjectScope::All, None),
        None => (ProjectScope::Listed(Vec::new()), None),
    };
    Ok(Caller {
        actor: setup.actor,
        label: setup.label,
        agent_key: match (setup.actor, setup.agent.as_deref()) {
            // A name selects an inbox, so a person with a name acts as that agent.
            (Actor::Human, None) => person_key(&worktree),
            (_, agent) => agent_key(&worktree, agent),
        },
        worktree,
        scope,
        default_project: setup.project.or(default_project),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file that older versions read in a plain directory.
    const OLD_PLAIN_CLONE_FILE: &str = ".lenka-clone.toml";

    #[test]
    fn a_connection_of_the_home_directory_stops_at_the_home_directory() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let home = gitdir::real_path(temp.path()).map_err(|err| LenkaError::io("path", &err))?;
        let below = home.join("code/deep");
        fs::create_dir_all(&below).map_err(|err| LenkaError::io("mkdir", &err))?;
        let code = home.join("code");
        let only_home = |dir: &Path| dir == home;
        assert_eq!(find_plain_root(&below, Some(&home), only_home), None);
        assert_eq!(
            find_plain_root(&home, Some(&home), only_home),
            Some(home.clone())
        );
        assert_eq!(find_plain_root(&below, None, only_home), Some(home.clone()));
        let home_and_code = |dir: &Path| dir == home || dir == code;
        assert_eq!(
            find_plain_root(&below, Some(&home), home_and_code),
            Some(code.clone())
        );
        Ok(())
    }

    /// A tar file, a garbage `.git` file, or a `.git` symlink must not give a project to an agent.
    #[test]
    fn a_file_in_a_plain_directory_selects_nothing() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let cwd = temp.path().join("work");
        fs::create_dir_all(&cwd).map_err(|err| LenkaError::io("mkdir", &err))?;
        create_project(
            &paths,
            &temp.path().join("other"),
            Some("secret"),
            &Location::Shared,
        )?;
        fs::write(cwd.join(OLD_PLAIN_CLONE_FILE), "projects = [\"secret\"]\n")
            .map_err(|err| LenkaError::io("write", &err))?;
        let check = |paths: &Paths| -> Result<()> {
            assert_eq!(find_clone_file(paths, &cwd), None);
            let caller = caller_for(paths, &cwd, setup(Actor::Agent, None))?;
            assert_eq!(caller.scope, ProjectScope::Listed(Vec::new()));
            Ok(())
        };
        check(&paths)?;
        fs::write(cwd.join(".git"), "not a Git file\n")
            .map_err(|err| LenkaError::io("write", &err))?;
        check(&paths)
    }

    /// `git init` in a plain project keeps the connection, and the directory itself must match.
    #[test]
    fn a_connection_stays_after_git_init_in_the_same_directory() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let cwd = temp.path().join("work");
        let sub = cwd.join("sub");
        fs::create_dir_all(&sub).map_err(|err| LenkaError::io("mkdir", &err))?;
        create_project(&paths, &cwd, Some("api"), &Location::Shared)?;
        for dir in [&cwd, &sub] {
            fs::create_dir_all(dir.join(".git")).map_err(|err| LenkaError::io("mkdir", &err))?;
            fs::write(dir.join(".git/HEAD"), "ref: refs/heads/main\n")
                .map_err(|err| LenkaError::io("write", &err))?;
        }
        assert!(find_clone_file(&paths, &cwd).is_some());
        assert_eq!(find_clone_file(&paths, &sub), None);
        Ok(())
    }

    fn setup(actor: Actor, agent: Option<&str>) -> CallerSetup {
        CallerSetup {
            actor,
            label: "x".to_owned(),
            project: None,
            agent: agent.map(str::to_owned),
        }
    }

    #[test]
    fn a_person_and_an_agent_in_one_directory_have_two_keys() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let person = caller_for(&paths, temp.path(), setup(Actor::Human, None))?;
        let agent = caller_for(&paths, temp.path(), setup(Actor::Agent, None))?;
        assert_ne!(person.agent_key, agent.agent_key);
        // A name selects one inbox, so a person can act as a named agent.
        let named_person = caller_for(&paths, temp.path(), setup(Actor::Human, Some("a")))?;
        let named_agent = caller_for(&paths, temp.path(), setup(Actor::Agent, Some("a")))?;
        assert_eq!(named_person.agent_key, named_agent.agent_key);
        Ok(())
    }

    #[test]
    fn a_plain_directory_has_one_identity_in_its_subdirectories() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let cwd = temp.path().join("work");
        let sub = cwd.join("sub");
        fs::create_dir_all(&sub).map_err(|err| LenkaError::io("mkdir", &err))?;
        create_project(&paths, &cwd, Some("api"), &Location::Shared)?;
        let root = caller_for(&paths, &cwd, setup(Actor::Agent, None))?;
        let below = caller_for(&paths, &sub, setup(Actor::Agent, None))?;
        assert_eq!(root.agent_key, below.agent_key);
        assert_eq!(root.worktree, below.worktree);
        Ok(())
    }

    /// Each process reads, changes, and saves the list, so a process with no lock loses entries.
    #[test]
    fn parallel_creates_keep_all_projects_and_one_id() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let cwd = temp.path().join("work");
        fs::create_dir_all(&cwd).map_err(|err| LenkaError::io("mkdir", &err))?;
        std::thread::scope(|scope| {
            for number in 0..12 {
                let (paths, cwd) = (&paths, &cwd);
                scope.spawn(move || {
                    create_project(paths, cwd, Some(&format!("p{number}")), &Location::Shared)
                        .expect("a project");
                    // One name in all threads must give one ID.
                    create_project(paths, cwd, Some("same"), &Location::Shared).expect("a project");
                });
            }
        });
        let list = ProjectsFile::load(&paths)?;
        assert_eq!(list.projects.len(), 13);
        let same = &list.projects["same"];
        let text = fs::read_to_string(same.directory.join("lenka.toml"))
            .map_err(|err| LenkaError::io("read", &err))?;
        assert_eq!(ProjectConfig::parse(&text)?.id, same.id);
        let clone = CloneFile::load(&clone_file_for(&paths, &cwd))?
            .ok_or_else(|| LenkaError::not_found("clone"))?;
        assert_eq!(clone.projects.len(), 13);
        Ok(())
    }

    #[test]
    fn a_registered_name_keeps_its_directory() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        for dir in [&first, &second] {
            fs::create_dir_all(dir).map_err(|err| LenkaError::io("mkdir", &err))?;
            let config = ProjectConfig::new("api").to_text()?;
            fs::write(dir.join(PROJECT_FILE), config)
                .map_err(|err| LenkaError::io("write", &err))?;
        }
        register_existing(&paths, temp.path(), None, &first)?;
        let refused = register_existing(&paths, temp.path(), None, &second);
        assert_eq!(
            refused.err().map(|error| error.code()),
            Some(ErrorCode::InvalidArgument)
        );
        assert_eq!(ProjectsFile::load(&paths)?.projects["api"].directory, first);
        register_existing(&paths, temp.path(), None, &first)?;
        Ok(())
    }

    #[test]
    fn a_create_keeps_git_files_of_the_person() -> Result<()> {
        for name in [".gitignore", ".gitattributes"] {
            let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
            let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
            let target = temp.path().join("target");
            fs::create_dir_all(&target).map_err(|err| LenkaError::io("mkdir", &err))?;
            fs::write(target.join(name), "my rules\n")
                .map_err(|err| LenkaError::io("write", &err))?;
            let location = Location::Directory(target.clone());
            assert!(create_project(&paths, temp.path(), Some("api"), &location).is_err());
            let kept = fs::read_to_string(target.join(name))
                .map_err(|err| LenkaError::io("read", &err))?;
            assert_eq!(kept, "my rules\n");
        }
        Ok(())
    }

    #[test]
    fn a_relative_directory_of_existing_data_becomes_absolute() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let target = temp.path().join("store");
        let location = Location::Directory(target);
        create_project(&paths, temp.path(), Some("first"), &location)?;
        let other = Paths::new(temp.path().join("config-2"), temp.path().join("data-2"));
        register_existing(&other, temp.path(), Some("api"), Path::new("store"))?;
        assert!(
            ProjectsFile::load(&other)?.projects["api"]
                .directory
                .is_absolute()
        );
        Ok(())
    }

    #[test]
    fn a_stopped_create_can_run_again() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let target = temp.path().join("target");
        for part in ["tasks", "artifacts", "memory", ".runtime/inbox"] {
            fs::create_dir_all(target.join(part)).map_err(|err| LenkaError::io("mkdir", &err))?;
        }
        for name in [".tmp-init-1", ".DS_Store"] {
            fs::write(target.join(name), "x").map_err(|err| LenkaError::io("write", &err))?;
        }
        fs::write(target.join(".gitignore"), GITIGNORE)
            .map_err(|err| LenkaError::io("write", &err))?;
        let created = create_project(
            &paths,
            temp.path(),
            Some("api"),
            &Location::Directory(target),
        )?;
        assert!(created.directory.join("lenka.toml").is_file());
        Ok(())
    }

    #[test]
    fn a_directory_with_data_of_another_kind_is_refused_at_each_location() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let cwd = temp.path().join("work");
        fs::create_dir_all(cwd.join("lenka-data/tasks/old"))
            .map_err(|err| LenkaError::io("mkdir", &err))?;
        let refused = create_project(&paths, &cwd, Some("api"), &Location::Default);
        assert_eq!(
            refused.err().map(|error| error.code()),
            Some(ErrorCode::InvalidArgument)
        );
        let other = temp.path().join("other");
        fs::create_dir_all(&other).map_err(|err| LenkaError::io("mkdir", &err))?;
        fs::write(other.join("notes.txt"), "x").map_err(|err| LenkaError::io("write", &err))?;
        let refused = create_project(&paths, &cwd, Some("b"), &Location::Directory(other));
        assert_eq!(
            refused.err().map(|error| error.code()),
            Some(ErrorCode::InvalidArgument)
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_as_the_data_directory_is_refused_before_a_write() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let real = temp.path().join("real");
        fs::create_dir_all(&real).map_err(|err| LenkaError::io("mkdir", &err))?;
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).map_err(|err| LenkaError::io("link", &err))?;
        let refused = create_project(&paths, temp.path(), Some("api"), &Location::Directory(link));
        assert_eq!(
            refused.err().map(|error| error.code()),
            Some(ErrorCode::InvalidArgument)
        );
        assert_eq!(
            fs::read_dir(&real)
                .map_err(|err| LenkaError::io("read", &err))?
                .count(),
            0
        );
        Ok(())
    }

    #[test]
    fn a_path_has_one_form_and_a_project_cannot_nest_in_another() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let cwd = temp.path().to_path_buf();
        let plain = Location::Directory(PathBuf::from("a/../data"));
        let first = create_project(&paths, &cwd, Some("n"), &plain)?;
        assert_eq!(
            first.directory,
            std::path::absolute(cwd.join("data")).map_err(|err| LenkaError::io("path", &err))?
        );
        create_project(
            &paths,
            &cwd,
            Some("n"),
            &Location::Directory(PathBuf::from("data")),
        )?;
        let inner = Location::Directory(PathBuf::from("data/tasks/inner"));
        let refused = create_project(&paths, &cwd, Some("inner"), &inner);
        assert_eq!(
            refused.err().map(|error| error.code()),
            Some(ErrorCode::InvalidArgument)
        );
        Ok(())
    }

    #[test]
    fn create_attach_and_caller_outside_git() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let cwd = temp.path().join("work");
        fs::create_dir_all(&cwd).map_err(|err| LenkaError::io("mkdir", &err))?;

        let created = create_project(&paths, &cwd, Some("api"), &Location::Shared)?;
        assert!(created.directory.join("lenka.toml").is_file());

        // A second create with the same name and directory is safe, and keeps the same ID.
        let first_id = ProjectsFile::load(&paths)?.projects["api"].id;
        create_project(&paths, &cwd, Some("api"), &Location::Shared)?;
        assert_eq!(ProjectsFile::load(&paths)?.projects["api"].id, first_id);

        let caller = caller_for(
            &paths,
            &cwd,
            CallerSetup {
                actor: Actor::Agent,
                label: "agent:test".to_owned(),
                project: None,
                agent: None,
            },
        )?;
        assert_eq!(caller.default_project.as_deref(), Some("api"));
        assert_eq!(caller.scope, ProjectScope::Listed(vec!["api".to_owned()]));

        let other = temp.path().join("other");
        fs::create_dir_all(&other).map_err(|err| LenkaError::io("mkdir", &err))?;
        attach(&paths, &other, "api")?;
        assert!(attach(&paths, &other, "missing").is_err());
        Ok(())
    }

    #[test]
    fn a_clone_file_in_a_checkout_selects_nothing() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join(".git")).map_err(|err| LenkaError::io("mkdir", &err))?;
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n")
            .map_err(|err| LenkaError::io("write", &err))?;
        fs::write(repo.join(OLD_PLAIN_CLONE_FILE), "projects = [\"secret\"]\n")
            .map_err(|err| LenkaError::io("write", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        assert_eq!(find_clone_file(&paths, &repo), None);
        Ok(())
    }

    #[test]
    fn a_relative_directory_becomes_an_absolute_entry() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let cwd = temp.path().join("work");
        fs::create_dir_all(&cwd).map_err(|err| LenkaError::io("mkdir", &err))?;
        let location = Location::Directory(PathBuf::from("relative/data"));
        let created = create_project(&paths, &cwd, Some("api"), &location)?;
        assert_eq!(created.directory, cwd.join("relative/data"));
        let entry = &ProjectsFile::load(&paths)?.projects["api"];
        assert!(entry.directory.is_absolute());
        assert!(entry.directory.join("lenka.toml").is_file());
        Ok(())
    }

    #[test]
    fn a_selected_directory_with_other_files_stays_clean() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let cwd = temp.path().join("work");
        let folder = temp.path().join("documents");
        fs::create_dir_all(&cwd).map_err(|err| LenkaError::io("mkdir", &err))?;
        fs::create_dir_all(&folder).map_err(|err| LenkaError::io("mkdir", &err))?;
        fs::write(folder.join("notes.txt"), "text").map_err(|err| LenkaError::io("write", &err))?;

        let location = Location::Directory(folder.clone());
        let error = create_project(&paths, &cwd, Some("api"), &location)
            .err()
            .ok_or_else(|| LenkaError::invalid_argument("the call must fail"))?;
        assert_eq!(error.code(), ErrorCode::InvalidArgument);
        assert!(!folder.join("lenka.toml").exists());
        assert!(!folder.join("tasks").exists());
        assert!(ProjectsFile::load(&paths)?.projects.is_empty());

        // An empty directory is valid, and a second name for the same project connects to it.
        fs::remove_file(folder.join("notes.txt")).map_err(|err| LenkaError::io("rm", &err))?;
        create_project(&paths, &cwd, Some("api"), &location)?;
        create_project(&paths, &cwd, Some("api-2"), &location)?;
        assert_eq!(ProjectsFile::load(&paths)?.projects.len(), 2);
        Ok(())
    }

    #[test]
    fn an_agent_outside_a_clone_has_no_projects() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
        let paths = Paths::new(temp.path().join("config"), temp.path().join("data"));
        let caller = caller_for(
            &paths,
            temp.path(),
            CallerSetup {
                actor: Actor::Agent,
                label: "agent:test".to_owned(),
                project: None,
                agent: None,
            },
        )?;
        assert_eq!(caller.scope, ProjectScope::Listed(Vec::new()));
        Ok(())
    }
}
