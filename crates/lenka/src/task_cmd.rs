// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The task commands. Each one maps to the same tool or service that MCP uses, with the actor
//! `human`. The person-only options use the typed service, because they have no MCP form.

use anyhow::Context;
use clap::{ArgAction, Subcommand};
use lenka_core::{Host, SecretPolicy};
use lenka_plugin_api::{
    ArtifactId, Day, DocPath, EntryId, ErrorCode, LenkaError, Result as LenkaResult,
};
use lenka_tasks::{
    CheckItem, ClaimRef, EntryKind, Finish, Link, NewEntry, NewTask, Priority, Status, TaskPatch,
    TaskService, service_of, with_notes,
};
use serde_json::{Value, json};

use crate::cli::RunContext;
use crate::output::{print_fields, print_task_rows, text};

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// Store a new task.
    Create {
        /// The title.
        title: String,
        /// The task type.
        #[arg(long = "type", value_name = "TYPE")]
        kind: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        description: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        requirements: Option<String>,
        /// The acceptance criteria, as `- [ ]` lines.
        #[arg(long, allow_hyphen_values = true)]
        acceptance: Option<String>,
        #[arg(long, allow_hyphen_values = true)]
        plan: Option<String>,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long = "blocked-by", value_name = "TASK")]
        blocked_by: Vec<String>,
        #[arg(long, value_name = "TASK")]
        relates: Vec<String>,
        /// The priority: low, normal, high, or urgent.
        #[arg(long, value_parser = parse_priority)]
        priority: Option<Priority>,
        #[arg(long = "label")]
        labels: Vec<String>,
        /// The first status: backlog or todo.
        #[arg(long, value_parser = parse_status)]
        status: Option<Status>,
        /// The planned first day, as YYYY-MM-DD.
        #[arg(long)]
        start: Option<String>,
        /// The planned last day, as YYYY-MM-DD.
        #[arg(long)]
        due: Option<String>,
        /// Store the text even if it matches a secret pattern.
        #[arg(long)]
        allow_secret: bool,
    },
    /// Change text or fields, add a plan, or link a task or an artifact.
    Update(Box<UpdateArgs>),
    /// Read a task.
    Show {
        task: String,
        /// Add parts: plan, entries, artifacts, or versions.
        #[arg(long, value_delimiter = ',')]
        include: Vec<String>,
        /// Read an old version.
        #[arg(long)]
        version: Option<u64>,
    },
    /// Filter and search tasks.
    List {
        #[arg(long)]
        status: Vec<String>,
        #[arg(long = "type", value_name = "TYPE")]
        kind: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        parent: Option<String>,
        /// Search tasks, plans, and decisions.
        #[arg(long, allow_hyphen_values = true)]
        text: Option<String>,
        /// Show the work to do next, in order.
        #[arg(long)]
        ready: bool,
        /// Show only blocked tasks.
        #[arg(long)]
        blocked: bool,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show blockers, dependents, parent, children, links, and similar tasks.
    Related { task: String },
    /// Take a task, or the next ready task.
    Claim {
        task: Option<String>,
        /// The claim handle, to prolong a claim.
        #[arg(long)]
        claim: Option<String>,
        /// The timeout in minutes.
        #[arg(long)]
        minutes: Option<u32>,
    },
    /// Finish the current stage with evidence, or return to an earlier stage.
    Advance {
        task: String,
        #[arg(long, allow_hyphen_values = true)]
        evidence: String,
        #[arg(long)]
        claim: Option<String>,
        /// Return to this earlier stage, with the reason in the evidence.
        #[arg(long, value_name = "STAGE")]
        back_to: Option<String>,
        /// Override a claim of another holder.
        #[arg(long = "override")]
        force: bool,
    },
    /// Record a decision, a comment, or a proposal.
    Entry {
        task: String,
        /// The kind: decision, comment, or proposal.
        #[arg(long, value_parser = parse_entry_kind)]
        kind: EntryKind,
        #[arg(long, allow_hyphen_values = true)]
        text: String,
        /// An earlier decision that this entry replaces.
        #[arg(long)]
        replaces: Option<String>,
        /// Store the text even if it matches a secret pattern.
        #[arg(long)]
        allow_secret: bool,
    },
    /// End the claim, and stop automatic pickup.
    Pause {
        task: String,
        #[arg(long)]
        claim: Option<String>,
    },
    /// Finish a task with evidence, or release it.
    Finish {
        task: String,
        /// The outcome: done or released.
        #[arg(long, default_value = "done", value_parser = ["done", "released"])]
        outcome: String,
        #[arg(long, allow_hyphen_values = true)]
        evidence: Option<String>,
        /// The reason to finish with an unchecked criterion.
        #[arg(long)]
        unchecked_reason: Option<String>,
        #[arg(long)]
        claim: Option<String>,
        /// Override a claim of another holder.
        #[arg(long = "override")]
        force: bool,
    },
    /// Delete a task with no children and no dependent tasks.
    Delete { task: String },
    /// Set a status directly.
    Status {
        task: String,
        #[arg(value_parser = parse_status)]
        status: Status,
        /// Override a claim of another holder.
        #[arg(long = "override")]
        force: bool,
    },
}

#[derive(Debug, clap::Args)]
pub struct UpdateArgs {
    task: String,
    /// The version that you read.
    #[arg(long)]
    version: u64,
    /// The reason for a change to text or plan.
    #[arg(long, allow_hyphen_values = true)]
    reason: Option<String>,
    #[arg(long)]
    claim: Option<String>,
    #[arg(long)]
    title: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    description: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    requirements: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    acceptance: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    plan: Option<String>,
    #[arg(long, value_parser = parse_priority)]
    priority: Option<Priority>,
    #[arg(long = "label-add")]
    labels_add: Vec<String>,
    #[arg(long = "label-remove")]
    labels_remove: Vec<String>,
    /// The parent task. An empty value removes the parent.
    #[arg(long)]
    parent: Option<String>,
    #[arg(long)]
    blocked_by_add: Vec<String>,
    #[arg(long)]
    blocked_by_remove: Vec<String>,
    #[arg(long)]
    relates_add: Vec<String>,
    #[arg(long)]
    relates_remove: Vec<String>,
    #[arg(long = "artifact-add")]
    artifacts_add: Vec<String>,
    #[arg(long = "artifact-remove")]
    artifacts_remove: Vec<String>,
    /// Check a criterion by its position.
    #[arg(long)]
    check: Vec<usize>,
    /// Uncheck a criterion by its position.
    #[arg(long)]
    uncheck: Vec<usize>,
    /// The planned first day. An empty value removes it.
    #[arg(long)]
    start: Option<String>,
    /// The planned last day. An empty value removes it.
    #[arg(long)]
    due: Option<String>,
    /// Change the task type.
    #[arg(long = "type", value_name = "TYPE")]
    kind: Option<String>,
    /// Set the coordinator rule of this task tree.
    #[arg(long, action = ArgAction::Set)]
    coordinated: Option<bool>,
    /// Let an agent continue the loop on this task until the task is done. This also ends a stop
    /// by the loop limit.
    #[arg(long, action = ArgAction::Set)]
    loop_until_done: Option<bool>,
    /// Store the text even if it matches a secret pattern.
    #[arg(long)]
    allow_secret: bool,
    /// Override a claim of another holder.
    #[arg(long = "override")]
    force: bool,
}

fn parse_priority(raw: &str) -> Result<Priority, String> {
    Priority::ALL
        .into_iter()
        .find(|item| item.as_str() == raw)
        .ok_or_else(|| "A priority is low, normal, high, or urgent.".to_owned())
}

fn parse_status(raw: &str) -> Result<Status, String> {
    Status::parse(raw).map_err(|error| error.message().to_owned())
}

fn parse_entry_kind(raw: &str) -> Result<EntryKind, String> {
    [EntryKind::Decision, EntryKind::Comment, EntryKind::Proposal]
        .into_iter()
        .find(|item| item.as_str() == raw)
        .ok_or_else(|| "An entry kind is decision, comment, or proposal.".to_owned())
}

fn policy(allow_secret: bool) -> SecretPolicy {
    if allow_secret {
        SecretPolicy::PersonOverride
    } else {
        SecretPolicy::Enforce
    }
}

fn claim_ref(handle: Option<String>, force: bool) -> ClaimRef {
    match handle {
        _ if force => ClaimRef::Override,
        Some(handle) => ClaimRef::Handle(handle),
        None => ClaimRef::Implicit,
    }
}

fn links(service: &TaskService, items: &[String]) -> LenkaResult<Vec<Link>> {
    items
        .iter()
        .map(|item| service.resolve_link(item))
        .collect()
}

fn optional_day(raw: Option<&str>) -> LenkaResult<Option<Option<Day>>> {
    match raw {
        None => Ok(None),
        Some("") => Ok(Some(None)),
        Some(value) => Day::parse(value).map(|day| Some(Some(day))),
    }
}

fn resolve_artifact(service: &TaskService, raw: &str) -> LenkaResult<ArtifactId> {
    if let Ok(id) = ArtifactId::parse(raw) {
        return Ok(id);
    }
    let names = service
        .project()
        .driver()
        .list(&DocPath::new("artifacts")?)?;
    let mut found = names.iter().filter(|name| name.ends_with(raw));
    match (found.next(), found.next()) {
        (Some(name), None) => ArtifactId::parse(name),
        (Some(_), Some(_)) => Err(LenkaError::new(
            ErrorCode::AmbiguousId,
            format!("The short ID {raw} matches more than one artifact. Use the full ID."),
        )),
        _ => Err(LenkaError::not_found(format!(
            "No artifact matches the ID {raw}."
        ))),
    }
}

fn resolve_entry(
    service: &TaskService,
    task: lenka_plugin_api::TaskId,
    raw: &str,
) -> LenkaResult<EntryId> {
    if let Ok(id) = EntryId::parse(raw) {
        return Ok(id);
    }
    let detail = service.detail(task, &["entries"])?;
    detail
        .entries
        .iter()
        .find(|entry| entry.id.matches_short(raw))
        .map(|entry| entry.id)
        .ok_or_else(|| LenkaError::not_found(format!("The task has no entry {raw}.")))
}

/// Runs a command that maps to a tool with simple arguments.
fn call_tool(
    context: &RunContext,
    host: &Host,
    tool: &str,
    args: &Value,
    human: impl FnOnce(&Value),
) -> anyhow::Result<()> {
    let caller = context.person()?;
    let result = host.call_tool(&caller, tool, args.clone())?;
    context.print(&result, human);
    Ok(())
}

fn print_rows(value: &Value) {
    let empty = Vec::new();
    print_task_rows(
        value
            .get("tasks")
            .and_then(Value::as_array)
            .unwrap_or(&empty),
    );
}

fn print_related(value: &Value) {
    let Some(map) = value.as_object() else {
        return;
    };
    for (key, item) in map {
        match item {
            Value::Array(rows) if !rows.is_empty() => {
                println!("{key}:");
                print_task_rows(rows);
            }
            Value::Object(_) if key == "parent" => {
                println!("{key}:");
                print_task_rows(std::slice::from_ref(item));
            }
            _ => {}
        }
    }
    if let Some(links) = value["untrusted_data"]["soft_links"]
        .as_array()
        .filter(|links| !links.is_empty())
    {
        println!("soft_links: {}", Value::Array(links.clone()));
    }
}

fn print_detail(value: &Value) {
    let task = &value["task"];
    let data = &value["untrusted_data"];
    if task.is_object() && data.is_object() {
        println!(
            "{}  {}  stage {}  version {}  {}",
            text(task, "id"),
            text(task, "status"),
            text(task, "stage"),
            task["version"],
            text(task, "type"),
        );
        println!("{}  ({})", text(data, "title"), text(data, "author"));
        if task["loop_stopped"] == true {
            println!(
                "The loop limit stopped this task. An agent cannot claim it until a person acts."
            );
        }
        if task["loop_until_done"] == true {
            println!("An agent can continue the loop on this task until the task is done.");
        }
        for key in ["claim", "active_blockers", "warnings", "soft_links"] {
            if !data[key].is_null() {
                println!("{key}: {}", data[key]);
            }
        }
        for key in ["description", "requirements", "plan"] {
            if let Some(body) = data[key].as_str().filter(|body| !body.is_empty()) {
                println!("\n{key}:\n{}", body.trim_end());
            }
        }
        if let Some(rows) = data["acceptance"].as_array() {
            println!("\nacceptance: {}", Value::Array(rows.clone()));
        } else if let Some(body) = data["acceptance"].as_str().filter(|body| !body.is_empty()) {
            println!("\nacceptance:\n{}", body.trim_end());
        }
        if let Some(rows) = data["entries"].as_array() {
            for row in rows {
                println!(
                    "\n{} {} ({}):\n{}",
                    text(row, "kind"),
                    text(row, "id"),
                    text(row, "author"),
                    text(row, "text").trim_end(),
                );
            }
        }
        for key in ["artifacts", "versions"] {
            if !value[key].is_null() {
                println!("\n{key}: {}", value[key]);
            }
        }
    } else {
        print_fields(value);
    }
}

/// Runs a task command.
pub fn run(context: &RunContext, host: &Host, command: TaskCommand) -> anyhow::Result<()> {
    match command {
        TaskCommand::Show {
            task,
            include,
            version,
        } => {
            let args = json!({"task": task, "include": include, "version": version});
            call_tool(
                context,
                host,
                "task.show",
                &without_nulls(args),
                print_detail,
            )
        }
        TaskCommand::List {
            status,
            kind,
            label,
            parent,
            text,
            ready,
            blocked,
            limit,
        } => {
            let args = json!({
                "status": status, "type": kind, "label": label, "parent": parent,
                "text": text, "ready": ready, "blocked": blocked, "limit": limit,
            });
            call_tool(context, host, "task.list", &without_nulls(args), print_rows)
        }
        TaskCommand::Related { task } => call_tool(
            context,
            host,
            "task.related",
            &json!({"task": task}),
            print_related,
        ),
        TaskCommand::Claim {
            task,
            claim,
            minutes,
        } => {
            let args = json!({"task": task, "claim": claim, "minutes": minutes});
            call_tool(
                context,
                host,
                "task.claim",
                &without_nulls(args),
                print_fields,
            )
        }
        TaskCommand::Pause { task, claim } => {
            let args = json!({"task": task, "claim": claim});
            call_tool(
                context,
                host,
                "task.pause",
                &without_nulls(args),
                print_fields,
            )
        }
        TaskCommand::Delete { task } => call_tool(
            context,
            host,
            "task.delete",
            &json!({"task": task}),
            print_fields,
        ),
        typed => run_typed(context, host, typed),
    }
}

/// Removes empty fields from tool arguments, because a tool reads a null as a wrong type.
fn without_nulls(mut args: Value) -> Value {
    if let Some(map) = args.as_object_mut() {
        map.retain(|_, item| !item.is_null());
    }
    args
}

fn run_typed(context: &RunContext, host: &Host, command: TaskCommand) -> anyhow::Result<()> {
    let caller = context.person()?;
    let project = host.project_for(&caller, None)?;
    let service = service_of(&project)?;
    let result = match command {
        TaskCommand::Create {
            title,
            kind,
            description,
            requirements,
            acceptance,
            plan,
            parent,
            blocked_by,
            relates,
            priority,
            labels,
            status,
            start,
            due,
            allow_secret,
        } => {
            let new = NewTask {
                kind: kind.unwrap_or_default(),
                title,
                description: description.unwrap_or_default(),
                requirements: requirements.unwrap_or_default(),
                acceptance: acceptance.unwrap_or_default(),
                plan,
                parent: parent.map(|raw| service.resolve(&raw)).transpose()?,
                blocked_by: links(&service, &blocked_by)?,
                relates: links(&service, &relates)?,
                artifacts: Vec::new(),
                priority: priority.unwrap_or_default(),
                labels,
                status,
                start: start.as_deref().map(Day::parse).transpose()?,
                due: due.as_deref().map(Day::parse).transpose()?,
                secret_policy: policy(allow_secret),
            };
            let meta = service.create(&caller, new)?;
            json!({
                "task": meta.id().short(),
                "full_id": meta.id().to_string(),
                "status": meta.state.status,
                "stage": meta.state.stage,
                "version": meta.state.version,
            })
        }
        TaskCommand::Update(args) => update(&service, &caller, *args)?,
        TaskCommand::Advance {
            task,
            evidence,
            claim,
            back_to,
            force,
        } => {
            let id = service.resolve(&task)?;
            let change = service.advance(
                &caller,
                id,
                &claim_ref(claim, force),
                &evidence,
                back_to.as_deref(),
            )?;
            lenka_tasks::change_json(id, &change)
        }
        TaskCommand::Entry {
            task,
            kind,
            text,
            replaces,
            allow_secret,
        } => {
            let id = service.resolve(&task)?;
            let replaces = replaces
                .as_deref()
                .map(|raw| resolve_entry(&service, id, raw))
                .transpose()?;
            let entry = service.add_entry(
                &caller,
                id,
                NewEntry {
                    kind,
                    text,
                    replaces,
                    secret_policy: policy(allow_secret),
                },
            )?;
            json!({"task": id.short(), "entry": entry.id.short()})
        }
        TaskCommand::Finish {
            task,
            outcome,
            evidence,
            unchecked_reason,
            claim,
            force,
        } => {
            let id = service.resolve(&task)?;
            let finish = if outcome == "released" {
                Finish::Released
            } else {
                Finish::Done {
                    evidence: evidence.unwrap_or_default(),
                    unchecked_reason,
                }
            };
            let change = service.finish(&caller, id, &claim_ref(claim, force), &finish)?;
            lenka_tasks::change_json(id, &change)
        }
        TaskCommand::Status {
            task,
            status,
            force,
        } => {
            let id = service.resolve(&task)?;
            let state = service.set_status(&caller, id, status, &claim_ref(None, force))?;
            state_json(id, &state)
        }
        other => unreachable!("run() handles the command {other:?}"),
    };
    context.print(&with_notes(result, &service, &caller), print_fields);
    Ok(())
}

fn state_json(id: lenka_plugin_api::TaskId, state: &lenka_tasks::State) -> Value {
    json!({
        "task": id.short(),
        "status": state.status,
        "stage": state.stage,
        "version": state.version,
    })
}

fn update(
    service: &TaskService,
    caller: &lenka_plugin_api::Caller,
    args: UpdateArgs,
) -> anyhow::Result<Value> {
    let id = service.resolve(&args.task)?;
    let mut check: Vec<CheckItem> = args
        .check
        .iter()
        .map(|index| CheckItem {
            index: *index,
            done: true,
        })
        .collect();
    check.extend(args.uncheck.iter().map(|index| CheckItem {
        index: *index,
        done: false,
    }));
    let artifacts = |items: &[String]| -> LenkaResult<Vec<ArtifactId>> {
        items
            .iter()
            .map(|raw| resolve_artifact(service, raw))
            .collect()
    };
    let patch = TaskPatch {
        expected_version: args.version,
        reason: args.reason,
        claim: claim_ref(args.claim, args.force),
        title: args.title,
        description: args.description,
        requirements: args.requirements,
        acceptance: args.acceptance,
        plan: args.plan,
        priority: args.priority,
        labels_add: args.labels_add,
        labels_remove: args.labels_remove,
        parent: match args.parent.as_deref() {
            None => None,
            Some("") => Some(None),
            Some(raw) => Some(Some(service.resolve(raw).context("Find the parent task")?)),
        },
        blocked_by_add: links(service, &args.blocked_by_add)?,
        blocked_by_remove: links(service, &args.blocked_by_remove)?,
        relates_add: links(service, &args.relates_add)?,
        relates_remove: links(service, &args.relates_remove)?,
        artifacts_add: artifacts(&args.artifacts_add)?,
        artifacts_remove: artifacts(&args.artifacts_remove)?,
        check,
        start: optional_day(args.start.as_deref())?,
        due: optional_day(args.due.as_deref())?,
        kind: args.kind,
        coordinated: args.coordinated,
        loop_until_done: args.loop_until_done,
        secret_policy: policy(args.allow_secret),
    };
    let meta = service.update(caller, id, patch)?;
    Ok(json!({
        "task": meta.id().short(),
        "version": meta.state.version,
        "status": meta.state.status,
        "stage": meta.state.stage,
    }))
}
