// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The `lenka` application: the default plugin list and the command line.
//!
//! A third-party build is a small wrapper crate. It extends the default plugin list, and it runs
//! the same command line:
//!
//! ```no_run
//! fn main() -> std::process::ExitCode {
//!     lenka::run(lenka::default_builder)
//! }
//! ```

mod cli;
#[cfg(feature = "tasks")]
mod doctor;
mod hook;
mod output;

#[cfg(feature = "artifacts")]
mod artifact_cmd;
#[cfg(feature = "memory")]
mod memory_cmd;
#[cfg(feature = "tasks")]
mod task_cmd;

use std::process::ExitCode;

use lenka_core::Builder;

/// Returns a builder with the storage plugins and the plugins of the enabled Cargo features.
#[must_use]
pub fn default_builder() -> Builder {
    let builder = Builder::new()
        .plugin(lenka_driver_local::plugin())
        .plugin(lenka_artifact_fs::plugin());
    #[cfg(feature = "tasks")]
    let builder = builder.plugin(lenka_tasks::plugin());
    #[cfg(feature = "artifacts")]
    let builder = builder.plugin(lenka_artifacts::plugin());
    #[cfg(feature = "memory")]
    let builder = builder.plugin(lenka_memory::plugin());
    #[cfg(feature = "git")]
    let builder = builder.plugin(lenka_git::plugin());
    #[cfg(feature = "mcp")]
    let builder = builder.plugin(lenka_mcp::plugin());
    builder
}

/// Runs the command line with a plugin list. The client hook runs first, and it does not build the
/// plugin list, because it has a time limit of 10 ms.
#[must_use]
pub fn run(plugins: impl FnOnce() -> Builder) -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    if let (Some(first), None) = (args.next(), args.next())
        && first == "hook"
    {
        hook::run();
        return ExitCode::SUCCESS;
    }
    cli::main(plugins)
}
