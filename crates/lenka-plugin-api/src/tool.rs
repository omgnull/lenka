// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

use serde_json::Value;

/// The definition of a tool that a plugin provides.
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// The stable tool name, such as `task.create`.
    pub name: &'static str,
    /// A short description for the model.
    pub description: &'static str,
    /// The JSON schema of the arguments.
    pub input_schema: Value,
    /// Tells if the tool changes no data.
    pub read_only: bool,
}
