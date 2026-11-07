// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The plugin host. All features are plugins, and the host holds the contracts.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use lenka_plugin_api::{
    ArtifactStore, Caller, ErrorCode, LenkaError, ProjectId, ProjectScope, Result, StorageDriver,
    ToolDef,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::clock::{Clock, SystemClock};
use crate::notices::ProjectEvent;
use crate::project::Project;
use crate::settings::{Paths, ProjectEntry, ProjectsFile};

/// Static facts about a plugin, for the admin view.
#[derive(Debug, Clone)]
pub struct PluginInfo {
    /// The plugin name. Cargo features and `plugins.toml` use this name.
    pub name: &'static str,
    /// A short description.
    pub description: &'static str,
    /// Tells if the plugin changes network or security behavior. Such a plugin is off by default.
    pub sensitive: bool,
}

/// A feature of lenka. A plugin registers its contributions when the host starts.
pub trait Plugin: Send + Sync + Debug {
    /// Returns the facts about this plugin.
    fn info(&self) -> PluginInfo;

    /// Registers the contributions of this plugin.
    fn register(&self, registry: &mut Registry);
}

/// Creates a storage driver for a data directory.
pub type DriverFactory = fn(&Path) -> Result<Arc<dyn StorageDriver>>;
/// Creates an artifact store for a data directory.
pub type StoreFactory = fn(&Path) -> Result<Arc<dyn ArtifactStore>>;

/// The contributions of the plugins.
#[derive(Debug, Default)]
pub struct Registry {
    drivers: HashMap<&'static str, DriverFactory>,
    stores: HashMap<&'static str, StoreFactory>,
    tools: Vec<Arc<dyn ToolProvider>>,
    hooks: Vec<Arc<dyn EventHook>>,
}

impl Registry {
    /// Adds a storage driver.
    pub fn driver(&mut self, name: &'static str, factory: DriverFactory) {
        self.drivers.insert(name, factory);
    }

    /// Adds an artifact store.
    pub fn artifact_store(&mut self, name: &'static str, factory: StoreFactory) {
        self.stores.insert(name, factory);
    }

    /// Adds a tool provider.
    pub fn tools(&mut self, provider: Arc<dyn ToolProvider>) {
        self.tools.push(provider);
    }

    /// Adds an event hook.
    pub fn hook(&mut self, hook: Arc<dyn EventHook>) {
        self.hooks.push(hook);
    }
}

/// The context of a tool call.
#[derive(Debug, Clone, Copy)]
pub struct ToolContext<'a> {
    /// The host.
    pub host: &'a Host,
    /// The caller.
    pub caller: &'a Caller,
}

/// A plugin part that provides tools.
pub trait ToolProvider: Send + Sync + Debug {
    /// Returns the tool definitions.
    fn tools(&self) -> Vec<ToolDef>;

    /// Runs a tool.
    ///
    /// # Errors
    ///
    /// Returns an error with a stable code and a message that states the next action.
    fn call(&self, ctx: ToolContext<'_>, name: &str, args: Value) -> Result<Value>;
}

/// A plugin part that reacts to events after a write completes.
pub trait EventHook: Send + Sync + Debug {
    /// Handles an event.
    ///
    /// # Errors
    ///
    /// Returns an error, which the caller shows as a warning.
    fn on_event(&self, project: &Project, event: &ProjectEvent) -> Result<()>;
}

/// The contents of `plugins.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PluginSettings {
    /// Plugins to turn off.
    disabled: Vec<String>,
    /// Sensitive plugins to turn on.
    enabled: Vec<String>,
}

/// Builds a host from plugins.
#[derive(Debug, Default)]
pub struct Builder {
    plugins: Vec<Arc<dyn Plugin>>,
    paths: Option<Paths>,
    clock: Option<Arc<dyn Clock>>,
}

impl Builder {
    /// Creates an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a plugin.
    #[must_use]
    pub fn plugin(mut self, plugin: impl Plugin + 'static) -> Self {
        self.plugins.push(Arc::new(plugin));
        self
    }

    /// Sets explicit user directories, instead of the environment.
    #[must_use]
    pub fn paths(mut self, paths: Paths) -> Self {
        self.paths = Some(paths);
        self
    }

    /// Sets the clock.
    #[must_use]
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Builds the host. `plugins.toml` selects the plugins that run.
    ///
    /// # Errors
    ///
    /// Returns an error if the user directories or `plugins.toml` are not valid.
    pub fn build(self) -> Result<Host> {
        let paths = match self.paths {
            Some(paths) => paths,
            None => Paths::from_env()?,
        };
        let settings = read_plugin_settings(&paths)?;
        let mut registry = Registry::default();
        let mut states = Vec::new();
        for plugin in &self.plugins {
            let info = plugin.info();
            // A plugin in both lists stays off, because that is the safe result.
            let off = settings.disabled.iter().any(|name| name == info.name);
            let on =
                !off && (!info.sensitive || settings.enabled.iter().any(|name| name == info.name));
            if on {
                plugin.register(&mut registry);
            }
            states.push(PluginState { info, enabled: on });
        }
        let peers = Arc::new(Peers {
            paths: paths.clone(),
            drivers: registry.drivers.clone(),
        });
        Ok(Host {
            paths,
            clock: self.clock.unwrap_or_else(|| Arc::new(SystemClock)),
            registry,
            peers,
            plugins: states,
            open: Mutex::new(HashMap::new()),
        })
    }
}

fn read_plugin_settings(paths: &Paths) -> Result<PluginSettings> {
    match std::fs::read_to_string(paths.plugins_file()) {
        Ok(text) => toml::from_str(&text)
            .map_err(|err| LenkaError::invalid_file(format!("plugins.toml: {}", err.message()))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(PluginSettings::default()),
        Err(err) => Err(LenkaError::io("Read plugins.toml", &err)),
    }
}

/// A plugin and its state, for the admin view.
#[derive(Debug, Clone)]
pub struct PluginState {
    /// The facts about the plugin.
    pub info: PluginInfo,
    /// Tells if the plugin runs.
    pub enabled: bool,
}

/// The result of a lookup for another project on this machine.
#[derive(Debug, Clone)]
pub enum PeerLookup {
    /// The list has no such project, or its directory is missing.
    Unavailable,
    /// The project storage.
    Found(Arc<dyn StorageDriver>),
}

/// Finds other projects, for the visible data of a soft link.
#[derive(Debug)]
pub struct Peers {
    paths: Paths,
    drivers: HashMap<&'static str, DriverFactory>,
}

impl Peers {
    /// Opens the storage of another project, with no regard to a caller scope. Only the visible
    /// data of a soft link can use this.
    ///
    /// # Errors
    ///
    /// Returns an error if the user project list is not valid.
    pub fn lookup(&self, id: ProjectId) -> Result<PeerLookup> {
        let file = ProjectsFile::load(&self.paths)?;
        let Some((_, entry)) = file.by_id(id) else {
            return Ok(PeerLookup::Unavailable);
        };
        let Some(factory) = self.drivers.get(entry.driver.as_str()) else {
            return Ok(PeerLookup::Unavailable);
        };
        Ok(factory(&entry.directory).map_or(PeerLookup::Unavailable, PeerLookup::Found))
    }
}

/// The running host.
#[derive(Debug)]
pub struct Host {
    paths: Paths,
    clock: Arc<dyn Clock>,
    registry: Registry,
    peers: Arc<Peers>,
    plugins: Vec<PluginState>,
    open: Mutex<HashMap<String, Arc<Project>>>,
}

impl Host {
    /// Returns the user directories.
    #[must_use]
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// Returns the names of the storage drivers in this build, in a fixed order.
    #[must_use]
    pub fn driver_names(&self) -> Vec<&'static str> {
        let mut names: Vec<_> = self.registry.drivers.keys().copied().collect();
        names.sort_unstable();
        names
    }

    /// Returns all compiled plugins with their state.
    #[must_use]
    pub fn plugins(&self) -> &[PluginState] {
        &self.plugins
    }

    /// Returns the definitions of all tools of the enabled plugins.
    #[must_use]
    pub fn tools(&self) -> Vec<ToolDef> {
        self.registry
            .tools
            .iter()
            .flat_map(|provider| provider.tools())
            .collect()
    }

    /// Runs a tool by name.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::NotFound`] for an unknown tool, or the error of the tool.
    pub fn call_tool(&self, caller: &Caller, name: &str, args: Value) -> Result<Value> {
        let ctx = ToolContext { host: self, caller };
        for provider in &self.registry.tools {
            if provider.tools().iter().any(|tool| tool.name == name) {
                return provider.call(ctx, name, args);
            }
        }
        Err(LenkaError::not_found(format!(
            "The tool {name} does not exist, or its plugin is off."
        )))
    }

    /// Returns the projects that a caller can use, sorted by name.
    ///
    /// # Errors
    ///
    /// Returns an error if the project list is not valid.
    pub fn available_projects(&self, caller: &Caller) -> Result<BTreeMap<String, ProjectEntry>> {
        let file = ProjectsFile::load(&self.paths)?;
        Ok(match &caller.scope {
            ProjectScope::All => file.projects,
            ProjectScope::Listed(names) => file
                .projects
                .into_iter()
                .filter(|(name, _)| names.iter().any(|item| item == name))
                .collect(),
        })
    }

    /// Selects a project in this order: the parameter, then the caller default.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ProjectNotAvailable`] with the available projects.
    pub fn project_for(&self, caller: &Caller, requested: Option<&str>) -> Result<Arc<Project>> {
        let available = self.available_projects(caller)?;
        let chosen = requested.or(caller.default_project.as_deref());
        let list = || {
            if available.is_empty() {
                "This clone lists no projects. Run `lenka init`, or `lenka project attach <name>`."
                    .to_owned()
            } else {
                let names: Vec<&str> = available.keys().map(String::as_str).collect();
                format!("Available projects: {}.", names.join(", "))
            }
        };
        let Some(chosen) = chosen else {
            if available.len() == 1
                && let Some((name, _)) = available.iter().next()
            {
                return self.open_by_name(name);
            }
            return Err(LenkaError::new(
                ErrorCode::ProjectNotAvailable,
                format!("Name a project. {}", list()),
            ));
        };
        if !available.contains_key(chosen) {
            return Err(LenkaError::new(
                ErrorCode::ProjectNotAvailable,
                format!("The project {chosen} is not available. {}", list()),
            ));
        }
        self.open_by_name(chosen)
    }

    /// Opens a project from the user list. This function does not check a caller scope.
    ///
    /// # Errors
    ///
    /// Returns an error if the project is missing or cannot open.
    pub fn open_by_name(&self, name: &str) -> Result<Arc<Project>> {
        if let Some(found) = lock(&self.open).get(name) {
            return Ok(Arc::clone(found));
        }
        let file = ProjectsFile::load(&self.paths)?;
        let entry = file.projects.get(name).ok_or_else(|| {
            LenkaError::new(
                ErrorCode::ProjectNotAvailable,
                format!("The project {name} is not in the user project list."),
            )
        })?;
        let project = self.open_entry(entry)?;
        lock(&self.open).insert(name.to_owned(), Arc::clone(&project));
        Ok(project)
    }

    /// Opens a project from a list entry.
    ///
    /// # Errors
    ///
    /// Returns an error if a plugin is off or the data directory is not valid.
    pub fn open_entry(&self, entry: &ProjectEntry) -> Result<Arc<Project>> {
        let driver_factory = self
            .registry
            .drivers
            .get(entry.driver.as_str())
            .ok_or_else(|| {
                LenkaError::invalid_argument(format!(
                    "The storage driver {} is not available in this build.",
                    entry.driver
                ))
            })?;
        let driver = driver_factory(&entry.directory)?;
        // The settings choose the store, so the first read needs only the driver.
        let store_name = store_name_of(driver.as_ref())?;
        let store_factory = self
            .registry
            .stores
            .get(store_name.as_str())
            .ok_or_else(|| {
                LenkaError::invalid_argument(format!(
                    "The artifact store {store_name} is not available in this build."
                ))
            })?;
        let store = store_factory(&entry.directory)?;
        let project = Project::open(
            driver,
            store,
            Arc::clone(&self.clock),
            self.registry.hooks.clone(),
            Arc::clone(&self.peers),
        )?;
        if project.id() != entry.id {
            return Err(LenkaError::new(
                ErrorCode::ProjectNotAvailable,
                format!(
                    "The directory {} holds a project with another ID than the project list. Restore the `id` value in lenka.toml.",
                    entry.directory.display()
                ),
            ));
        }
        Ok(project)
    }

    /// Finds the list entry of a project ID, with no regard to a caller scope. Only the visible
    /// data of a soft link uses this.
    ///
    /// # Errors
    ///
    /// Returns an error if the project list is not valid.
    pub fn entry_by_id(&self, id: ProjectId) -> Result<Option<ProjectEntry>> {
        let file = ProjectsFile::load(&self.paths)?;
        Ok(file.by_id(id).map(|(_, entry)| entry.clone()))
    }
}

fn store_name_of(driver: &dyn StorageDriver) -> Result<String> {
    let config = lenka_plugin_api::DocPath::new(crate::settings::PROJECT_FILE)?;
    let bytes = driver
        .read(&config)?
        .ok_or_else(|| LenkaError::not_found("The project directory has no lenka.toml."))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| LenkaError::invalid_file("lenka.toml is not valid UTF-8."))?;
    Ok(crate::settings::ProjectConfig::parse(&text)?
        .storage
        .artifact_store)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Reads the optional `project` argument of a tool.
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidArgument`] if the value is not a string. A wrong type must not
/// select the default project.
pub fn project_argument(args: &serde_json::Value) -> Result<Option<&str>> {
    match args.get("project") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(name)) => Ok(Some(name)),
        Some(_) => Err(LenkaError::invalid_argument(
            "The argument `project` must be a string.",
        )),
    }
}
