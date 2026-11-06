// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The artifact commands.

use clap::Subcommand;
use lenka_artifacts::artifacts_of;
use lenka_core::Host;
use serde_json::{Value, json};

use crate::cli::RunContext;
use crate::output::{print_fields, text};

#[derive(Debug, Subcommand)]
pub enum ArtifactCommand {
    /// Add an artifact from a file.
    Create {
        path: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        media_type: Option<String>,
    },
    /// Add a new version from a file.
    Update {
        artifact: String,
        path: String,
        /// The version that you read.
        #[arg(long)]
        version: u64,
        #[arg(long, allow_hyphen_values = true)]
        reason: String,
        /// `keep` keeps the old version, and `replace` deletes it.
        #[arg(long, value_parser = ["keep", "replace"])]
        mode: Option<String>,
        #[arg(long)]
        claim: Option<String>,
    },
    /// Delete an artifact that no task links to.
    Delete { artifact: String },
    /// List the artifacts of the project.
    List,
}

fn without_nulls(mut args: Value) -> Value {
    if let Some(map) = args.as_object_mut() {
        map.retain(|_, item| !item.is_null());
    }
    args
}

/// Runs an artifact command.
pub fn run(context: &RunContext, host: &Host, command: ArtifactCommand) -> anyhow::Result<()> {
    let caller = context.person()?;
    let (tool, args) = match command {
        ArtifactCommand::Create {
            path,
            name,
            media_type,
        } => (
            "artifact.create",
            json!({"path": path, "name": name, "media_type": media_type}),
        ),
        ArtifactCommand::Update {
            artifact,
            path,
            version,
            reason,
            mode,
            claim,
        } => (
            "artifact.update",
            json!({
                "artifact": artifact, "path": path, "version": version, "reason": reason,
                "mode": mode, "claim": claim,
            }),
        ),
        ArtifactCommand::Delete { artifact } => ("artifact.delete", json!({"artifact": artifact})),
        ArtifactCommand::List => {
            let project = host.project_for(&caller, None)?;
            let records = artifacts_of(&project)?.list()?;
            let rows: Vec<Value> = records
                .iter()
                .map(|record| {
                    json!({
                        "artifact": record.id.short(),
                        "full_id": record.id.to_string(),
                        "untrusted_data": {"name": record.name, "media_type": record.media_type},
                        "version": record.version,
                        "size": record.current().size,
                    })
                })
                .collect();
            let value = json!({"artifacts": rows});
            context.print(&value, |_| {
                if rows.is_empty() {
                    say!("No artifacts.");
                }
                for row in &rows {
                    say!(
                        "{}  version {}  {} bytes  {}",
                        text(row, "artifact"),
                        row["version"],
                        row["size"],
                        text(&row["untrusted_data"], "name"),
                    );
                }
            });
            return Ok(());
        }
    };
    let result = host.call_tool(&caller, tool, without_nulls(args))?;
    context.print(&result, print_fields);
    Ok(())
}
