// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Project settings, the user project list, and the clone file.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use lenka_plugin_api::{Actor, ErrorCode, LenkaError, ProjectId, Result};
use serde::{Deserialize, Serialize};

/// The current data format. A process reads the two previous formats.
pub const FORMAT_VERSION: u32 = 1;

/// The name of the clone file in the Git common directory.
pub const CLONE_FILE: &str = "lenka-clone.toml";

/// The name of the clone file outside Git, in a project directory or a parent of it.
pub const PLAIN_CLONE_FILE: &str = ".lenka-clone.toml";

/// The name of the project settings file.
pub const PROJECT_FILE: &str = "lenka.toml";

/// One stage of a task type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage {
    /// The stage name.
    pub name: String,
    /// Who can do the stage.
    pub actor: Actor,
    /// Tells if completion of this stage releases dependent tasks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unblocks: bool,
}

/// One task type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeDef {
    /// The stages in order. A type has at least one stage.
    pub stages: Vec<Stage>,
}

/// The limits and defaults of a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// The default claim timeout in minutes.
    pub claim_timeout_minutes: u32,
    /// The count of material agent changes in one hour that pauses a task.
    pub loop_limit: u32,
    /// Tells if agents can use the delete tools.
    pub agent_delete: bool,
    /// Tells if a write flushes each file. It is safer on some file systems, and much slower.
    pub strict_flush: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            claim_timeout_minutes: 30,
            loop_limit: 5,
            agent_delete: true,
            strict_flush: false,
        }
    }
}

/// The policy of links between projects. The default is no links.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LinkPolicy {
    /// The projects that this project can link to.
    pub link_to: Vec<String>,
    /// The projects that can link to this project. `*` means any.
    pub accept_from: Vec<String>,
}

impl LinkPolicy {
    /// Tells if this project can link to the target.
    #[must_use]
    pub fn allows_link_to(&self, target: ProjectId) -> bool {
        let target = target.to_string();
        self.link_to.contains(&target)
    }

    /// Tells if this project accepts a link from the source.
    #[must_use]
    pub fn accepts_from(&self, source: ProjectId) -> bool {
        let source = source.to_string();
        self.accept_from
            .iter()
            .any(|item| item == "*" || *item == source)
    }
}

/// The plugin choices of a project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageSettings {
    /// The storage driver name.
    pub driver: String,
    /// The artifact store name.
    pub artifact_store: String,
}

impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            driver: "local".to_owned(),
            artifact_store: "fs".to_owned(),
        }
    }
}

/// The contents of `lenka.toml`. This file holds no secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// The stable project ID.
    pub id: ProjectId,
    /// The display name.
    pub name: String,
    /// The data format.
    pub format_version: u32,
    /// The plugin choices.
    #[serde(default)]
    pub storage: StorageSettings,
    /// The limits and defaults.
    #[serde(default)]
    pub limits: Limits,
    /// The link policy.
    #[serde(default)]
    pub links: LinkPolicy,
    /// The task types.
    pub types: BTreeMap<String, TypeDef>,
}

impl ProjectConfig {
    /// Creates the settings of a new project, with the template types.
    #[must_use]
    pub fn new(name: &str) -> Self {
        let work = TypeDef {
            stages: vec![Stage {
                name: "work".to_owned(),
                actor: Actor::Any,
                unblocks: false,
            }],
        };
        let types = ["epic", "story", "task", "bug"]
            .into_iter()
            .map(|name| (name.to_owned(), work.clone()))
            .collect();
        Self {
            id: ProjectId::generate(),
            name: name.to_owned(),
            format_version: FORMAT_VERSION,
            storage: StorageSettings::default(),
            limits: Limits::default(),
            links: LinkPolicy::default(),
            types,
        }
    }

    /// Returns the names of the sections that differ from other settings, for a report to a person.
    #[must_use]
    pub fn changed_sections(&self, other: &Self) -> Vec<&'static str> {
        [
            ("name", self.name == other.name),
            ("limits", self.limits == other.limits),
            ("links", self.links == other.links),
            ("types", self.types == other.types),
        ]
        .into_iter()
        .filter(|(_, same)| !same)
        .map(|(name, _)| name)
        .collect()
    }

    /// Parses and validates the file text.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidFile`] for bad text, and [`ErrorCode::FormatTooNew`] for a
    /// format that this process does not know.
    pub fn parse(text: &str) -> Result<Self> {
        let config: Self = toml::from_str(text)
            .map_err(|err| LenkaError::invalid_file(format!("lenka.toml: {}", err.message())))?;
        if config.format_version > FORMAT_VERSION {
            return Err(LenkaError::new(
                ErrorCode::FormatTooNew,
                format!(
                    "The data format is {}, and this lenka knows {}. Update lenka.",
                    config.format_version, FORMAT_VERSION
                ),
            ));
        }
        config.validate()?;
        Ok(config)
    }

    /// Checks the types and the stages.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidFile`] if a type has no stages or a duplicate stage.
    pub fn validate(&self) -> Result<()> {
        if self.types.is_empty() {
            return Err(LenkaError::invalid_file("lenka.toml has no task types."));
        }
        for (name, def) in &self.types {
            let names = std::iter::once(name).chain(def.stages.iter().map(|stage| &stage.name));
            for checked in names {
                if !crate::input::is_safe_name(checked) {
                    return Err(LenkaError::invalid_file(format!(
                        "lenka.toml has a type or a stage with a name that is not valid. A name has at most {} bytes of letters, digits, `-`, and `_`.",
                        crate::input::MAX_NAME_BYTES
                    )));
                }
            }
            if def.stages.is_empty() {
                return Err(LenkaError::invalid_file(format!(
                    "The type {name} in lenka.toml has no stages."
                )));
            }
            for (index, stage) in def.stages.iter().enumerate() {
                if def.stages[..index].iter().any(|s| s.name == stage.name) {
                    return Err(LenkaError::invalid_file(format!(
                        "The type {name} in lenka.toml has the stage {} two times.",
                        stage.name
                    )));
                }
            }
        }
        Ok(())
    }

    /// Formats the file text.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidFile`] if the settings cannot be formatted.
    pub fn to_text(&self) -> Result<String> {
        toml::to_string_pretty(self)
            .map_err(|err| LenkaError::invalid_file(format!("lenka.toml: {err}")))
    }

    /// Formats the settings, and keeps the tables of an old file that this type does not know.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::InvalidFile`] if the settings cannot be formatted or the old text is
    /// not valid TOML.
    pub fn to_text_over(&self, old: &str) -> Result<String> {
        let invalid =
            |err: &dyn std::fmt::Display| LenkaError::invalid_file(format!("lenka.toml: {err}"));
        let text = self.to_text()?;
        let mut table: toml::Table = text.parse().map_err(|err| invalid(&err))?;
        let old: toml::Table = old.parse().map_err(|err| invalid(&err))?;
        let mut kept = false;
        for (key, value) in old {
            if !table.contains_key(&key) {
                table.insert(key, value);
                kept = true;
            }
        }
        if !kept {
            return Ok(text);
        }
        toml::to_string_pretty(&table).map_err(|err| invalid(&err))
    }
}

/// One project in the user project list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectEntry {
    /// The stable project ID.
    pub id: ProjectId,
    /// The storage driver name.
    pub driver: String,
    /// The data directory.
    pub directory: PathBuf,
}

/// The user project list. This file holds no secrets.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProjectsFile {
    /// The projects by name.
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectEntry>,
}

impl ProjectsFile {
    /// Reads the list. A missing file gives an empty list.
    ///
    /// # Errors
    ///
    /// Returns an error if the file is not valid.
    pub fn load(paths: &Paths) -> Result<Self> {
        let file = paths.projects_file();
        match fs::read_to_string(&file) {
            Ok(text) => toml::from_str(&text).map_err(|err| {
                LenkaError::invalid_file(format!("projects.toml: {}", err.message()))
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(LenkaError::io("Read projects.toml", &err)),
        }
    }

    /// Writes the list.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save(&self, paths: &Paths) -> Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|err| LenkaError::invalid_file(format!("projects.toml: {err}")))?;
        write_file(&paths.projects_file(), &text)
    }

    /// Finds a project by its ID.
    #[must_use]
    pub fn by_id(&self, id: ProjectId) -> Option<(&String, &ProjectEntry)> {
        self.projects.iter().find(|(_, entry)| entry.id == id)
    }
}

/// The projects that a clone can use. Git does not track this file.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CloneFile {
    /// The project names.
    #[serde(default)]
    pub projects: Vec<String>,
    /// The default project name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

impl CloneFile {
    /// Reads a clone file. A missing file gives `None`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file is not valid.
    pub fn load(file: &Path) -> Result<Option<Self>> {
        match fs::read_to_string(file) {
            Ok(text) => toml::from_str(&text).map(Some).map_err(|err| {
                LenkaError::invalid_file(format!("{CLONE_FILE}: {}", err.message()))
            }),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(LenkaError::io("Read the clone file", &err)),
        }
    }

    /// Writes a clone file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be written.
    pub fn save(&self, file: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self)
            .map_err(|err| LenkaError::invalid_file(format!("{CLONE_FILE}: {err}")))?;
        write_file(file, &text)
    }

    /// Adds a project. The first project becomes the default.
    pub fn add(&mut self, name: &str) {
        if !self.projects.iter().any(|item| item == name) {
            self.projects.push(name.to_owned());
        }
        if self.default.is_none() {
            self.default = Some(name.to_owned());
        }
    }
}

fn write_file(file: &Path, text: &str) -> Result<()> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|err| LenkaError::io("Create a directory", &err))?;
    }
    let temp = file.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&temp, text).map_err(|err| LenkaError::io("Write a settings file", &err))?;
    fs::rename(&temp, file).map_err(|err| LenkaError::io("Replace a settings file", &err))
}

/// The user-level directories of lenka.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    config_dir: PathBuf,
    data_dir: PathBuf,
}

impl Paths {
    /// Finds the directories from the environment and the OS standard locations.
    /// `LENKA_CONFIG_DIR` and `LENKA_DATA_DIR` override the locations.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::Io`] if the OS has no standard location.
    pub fn from_env() -> Result<Self> {
        let config_dir = std::env::var_os("LENKA_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::config_dir().map(|dir| dir.join("lenka")));
        let data_dir = std::env::var_os("LENKA_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|dir| dir.join("lenka")));
        match (config_dir, data_dir) {
            (Some(config_dir), Some(data_dir)) => Ok(Self {
                config_dir,
                data_dir,
            }),
            _ => Err(LenkaError::new(
                ErrorCode::Io,
                "The OS has no standard user directory. Set LENKA_CONFIG_DIR and LENKA_DATA_DIR.",
            )),
        }
    }

    /// Creates the paths from explicit directories.
    #[must_use]
    pub fn new(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            config_dir,
            data_dir,
        }
    }

    /// Returns the path of the user project list.
    #[must_use]
    pub fn projects_file(&self) -> PathBuf {
        self.config_dir.join("projects.toml")
    }

    /// Returns the path of the plugin settings.
    #[must_use]
    pub fn plugins_file(&self) -> PathBuf {
        self.config_dir.join("plugins.toml")
    }

    /// Returns the default directory of a shared project.
    #[must_use]
    pub fn shared_project_dir(&self, name: &str) -> PathBuf {
        self.data_dir.join("projects").join(name)
    }
}

/// Validates a project name. A name is used as a file name, and as a TOML key.
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidArgument`] if the name is not valid.
pub fn validate_project_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(LenkaError::invalid_argument(
            "A project name can hold only letters, digits, '.', '_', and '-'.",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip() -> Result<()> {
        let config = ProjectConfig::new("api");
        let parsed = ProjectConfig::parse(&config.to_text()?)?;
        assert_eq!(config, parsed);
        assert_eq!(parsed.types["task"].stages[0].name, "work");
        Ok(())
    }

    #[test]
    fn a_newer_format_is_refused() -> Result<()> {
        let mut config = ProjectConfig::new("api");
        config.format_version = FORMAT_VERSION + 1;
        let Err(error) = ProjectConfig::parse(&config.to_text()?) else {
            panic!("a newer format must fail");
        };
        assert_eq!(error.code(), ErrorCode::FormatTooNew);
        Ok(())
    }

    #[test]
    fn a_type_with_a_duplicate_stage_is_invalid() -> Result<()> {
        let mut config = ProjectConfig::new("api");
        let stage = config.types["task"].stages[0].clone();
        if let Some(def) = config.types.get_mut("task") {
            def.stages.push(stage);
        }
        assert!(ProjectConfig::parse(&config.to_text()?).is_err());
        Ok(())
    }

    #[test]
    fn link_policy_needs_explicit_values() {
        let id = ProjectId::generate();
        let mut policy = LinkPolicy::default();
        assert!(!policy.allows_link_to(id));
        assert!(!policy.accepts_from(id));
        policy.accept_from.push("*".to_owned());
        assert!(policy.accepts_from(id));
        policy.link_to.push(id.to_string());
        assert!(policy.allows_link_to(id));
    }
}
