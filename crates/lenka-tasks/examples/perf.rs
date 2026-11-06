// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Measures the engine against the limits in the design.
//!
//! ```sh
//! cargo run --release -p lenka-tasks --example perf -- gen <dir> <tasks>
//! cargo run --release -p lenka-tasks --example perf -- run <dir>   # Repeat this for a warm start.
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lenka_core::init::{Location, create_project};
use lenka_core::{Builder, Host, Paths};
use lenka_plugin_api::{Actor, Caller, LenkaError, ProjectScope, Result, TaskId, Timestamp};
use lenka_tasks::{
    ListFilter, State, Status, TaskFront, TaskPatch, TaskService, markdown, service_of,
};

fn host(dir: &Path) -> Result<Host> {
    Builder::new()
        .plugin(lenka_driver_local::plugin())
        .plugin(lenka_artifact_fs::plugin())
        .plugin(lenka_tasks::plugin())
        .paths(paths(dir))
        .build()
}

fn paths(dir: &Path) -> Paths {
    Paths::new(dir.join("config"), dir.join("data"))
}

fn caller() -> Caller {
    Caller {
        actor: Actor::Human,
        label: "human:perf".to_owned(),
        worktree: PathBuf::from("/perf"),
        agent_key: lenka_core::notices::agent_key(Path::new("/perf"), None),
        scope: ProjectScope::All,
        default_project: None,
    }
}

/// Writes the files directly, because a run through the API flushes each file.
fn generate(dir: &Path, count: usize) -> Result<()> {
    let io = |err: std::io::Error| LenkaError::io("generate", &err);
    let work = dir.join("work");
    fs::create_dir_all(&work).map_err(io)?;
    let created = create_project(&paths(dir), &work, Some("perf"), &Location::Shared)?;
    let filler =
        "Lenka keeps tasks for agents, and this text stands for a normal description. ".repeat(25);
    let mut previous: Option<TaskId> = None;
    for number in 0..count {
        let id = TaskId::generate();
        let root = created.directory.join("tasks").join(id.to_string());
        let version_dir = root.join("versions/000001");
        fs::create_dir_all(&version_dir).map_err(io)?;
        let blocked_by = match previous {
            Some(target) if number % 3 == 0 => vec![lenka_tasks::Link::Local(target)],
            _ => Vec::new(),
        };
        let front = TaskFront {
            id,
            kind: "task".to_owned(),
            title: format!("Task number {number} of the benchmark"),
            parent: None,
            blocked_by,
            relates: Vec::new(),
            artifacts: Vec::new(),
            priority: lenka_tasks::Priority::Normal,
            labels: vec![format!("group-{}", number % 20)],
            created_by: "agent:perf".to_owned(),
            created_at: Timestamp::from_unix_ms(1_793_000_000_000),
            start: None,
            due: None,
            coordinated: false,
        };
        let text = markdown::join_front(
            &front,
            &markdown::new_body(&filler, "- Fast", "- [ ] Passes"),
        )?;
        let state = State {
            version: 1,
            status: if number % 4 == 0 {
                Status::Done
            } else {
                Status::Todo
            },
            stage: "work".to_owned(),
            unblocked: false,
            loop_stopped: false,
            loop_until_done: false,
        };
        let state_text =
            toml::to_string(&state).map_err(|err| LenkaError::invalid_file(err.to_string()))?;
        fs::write(root.join("task.md"), &text).map_err(io)?;
        fs::write(version_dir.join("task.md"), &text).map_err(io)?;
        fs::write(
            version_dir.join("meta.toml"),
            "version = 1\nauthor = \"agent:perf\"\nat = \"2026-11-06T12:00:00Z\"\n",
        )
        .map_err(io)?;
        fs::write(root.join("state.toml"), state_text).map_err(io)?;
        if number % 5 == 0 {
            fs::write(root.join("plan.md"), &filler).map_err(io)?;
            fs::write(version_dir.join("plan.md"), &filler).map_err(io)?;
        }
        if number % 2 == 0 {
            previous = Some(id);
        }
    }
    println!("generated {count} tasks in {}", created.directory.display());
    Ok(())
}

fn percentile(values: &mut [Duration], part: f64) -> Duration {
    values.sort_unstable();
    let index = ((values.len() as f64 - 1.0) * part).round() as usize;
    values[index]
}

fn rss_mib() -> f64 {
    let Ok(text) = fs::read_to_string("/proc/self/status") else {
        return 0.0;
    };
    text.lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next()?.parse::<f64>().ok())
        .map_or(0.0, |kib| kib / 1024.0)
}

fn sample(rounds: usize, mut work: impl FnMut(usize) -> Result<()>) -> Result<Vec<Duration>> {
    let mut times = Vec::with_capacity(rounds);
    for round in 0..rounds {
        let start = Instant::now();
        work(round)?;
        times.push(start.elapsed());
    }
    Ok(times)
}

fn run(dir: &Path) -> Result<()> {
    let who = caller();
    let start = Instant::now();
    let host = host(dir)?;
    let project = host.open_by_name("perf")?;
    let service: Arc<TaskService> = service_of(&project)?;
    let filter = ListFilter {
        statuses: vec![Status::Todo],
        limit: 50,
        ..ListFilter::default()
    };
    let first = service.list(&who, &filter)?;
    println!(
        "first answer in a new process: {:?} ({} rows)",
        start.elapsed(),
        first.len()
    );
    println!("memory after the first answer: {:.1} MiB", rss_mib());

    let mut times = sample(400, |_| service.list(&who, &filter).map(|_| ()))?;
    println!(
        "task.list status filter: p50 {:?}, p95 {:?}",
        percentile(&mut times, 0.5),
        percentile(&mut times, 0.95)
    );
    let ready = ListFilter {
        ready: true,
        limit: 50,
        ..ListFilter::default()
    };
    let mut times = sample(100, |_| service.list(&who, &ready).map(|_| ()))?;
    println!(
        "task.list ready: p50 {:?}, p95 {:?}",
        percentile(&mut times, 0.5),
        percentile(&mut times, 0.95)
    );
    let text = ListFilter {
        text: Some("no such words here".to_owned()),
        ..ListFilter::default()
    };
    let mut times = sample(3, |_| service.list(&who, &text).map(|_| ()))?;
    println!(
        "task.list text search on the disk: p50 {:?}",
        percentile(&mut times, 0.5)
    );

    let rows = service.board_rows()?;
    let targets: Vec<(TaskId, u64)> = rows
        .iter()
        // A closed task cannot change.
        .filter(|row| !row.meta.state.status.is_closed())
        .take(200)
        .map(|row| (row.meta.id(), row.meta.state.version))
        .collect();
    let mut times = sample(targets.len(), |round| {
        let (id, version) = targets[round];
        service
            .update(
                &who,
                id,
                TaskPatch {
                    expected_version: version,
                    description: Some(format!("New text at run {}", Timestamp::now().unix_ms())),
                    reason: Some("benchmark".to_owned()),
                    ..TaskPatch::default()
                },
            )
            .map(|_| ())
    })?;
    println!(
        "write with a version check and a flush: p50 {:?}, p95 {:?}",
        percentile(&mut times, 0.5),
        percentile(&mut times, 0.95)
    );
    let start = Instant::now();
    let count = service.reindex(&who)?;
    println!("full rebuild of {count} tasks: {:?}", start.elapsed());
    println!("memory at the end: {:.1} MiB", rss_mib());
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("gen") => {
            let count = args
                .get(3)
                .and_then(|text| text.parse().ok())
                .unwrap_or(10_000);
            generate(Path::new(&args[2]), count)
        }
        Some("run") => run(Path::new(&args[2])),
        _ => Err(LenkaError::invalid_argument(
            "Use: perf gen <dir> <tasks>, or perf run <dir>.",
        )),
    }
}
