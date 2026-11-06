// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The shared memory commands.

use clap::Subcommand;
use lenka_core::Host;
use serde_json::{Value, json};

use crate::cli::RunContext;
use crate::output::{print_fields, text};

#[derive(Debug, Subcommand)]
pub enum MemoryCommand {
    /// Store a fact for all agents of the project.
    Add {
        #[arg(allow_hyphen_values = true)]
        text: String,
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// The related task.
        #[arg(long)]
        task: Option<String>,
    },
    /// Find memory by words or a tag.
    Search {
        text: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Delete a memory record.
    Delete { id: String },
}

fn print_found(value: &Value) {
    let rows = value.get("memories").and_then(Value::as_array);
    let Some(rows) = rows.filter(|rows| !rows.is_empty()) else {
        say!("No memory records.");
        return;
    };
    for row in rows {
        say!(
            "{}  {}  {}",
            text(row, "id"),
            text(row, "at"),
            text(&row["untrusted_data"], "author")
        );
        say!("{}", text(&row["untrusted_data"], "text"));
    }
}

/// Runs a memory command.
pub fn run(context: &RunContext, host: &Host, command: MemoryCommand) -> anyhow::Result<()> {
    let caller = context.person()?;
    match command {
        MemoryCommand::Add { text, tags, task } => {
            let mut args = json!({"text": text, "tags": tags});
            if let Some(task) = task {
                args["task"] = json!(task);
            }
            let result = host.call_tool(&caller, "memory.add", args)?;
            context.print(&result, print_fields);
        }
        MemoryCommand::Search { text, tag, limit } => {
            let mut args = json!({});
            if let Some(text) = text {
                args["text"] = json!(text);
            }
            if let Some(tag) = tag {
                args["tag"] = json!(tag);
            }
            if let Some(limit) = limit {
                args["limit"] = json!(limit);
            }
            let result = host.call_tool(&caller, "memory.search", args)?;
            context.print(&result, print_found);
        }
        MemoryCommand::Delete { id } => {
            let result = host.call_tool(&caller, "memory.delete", json!({"id": id}))?;
            context.print(&result, print_fields);
        }
    }
    Ok(())
}
