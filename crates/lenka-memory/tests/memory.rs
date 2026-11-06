// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Behavior tests of shared memory: tools, search order, secrets, and delete rules.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use lenka_core::init::{Location, create_project};
use lenka_core::{Builder, FixedClock, Host, Paths, ProjectConfig};
use lenka_memory::{MemoryQuery, NewMemory, memory_of};
use lenka_plugin_api::{Actor, Caller, ErrorCode, LenkaError, ProjectScope, Result};
use serde_json::{Value, json};

struct Fixture {
    _temp: tempfile::TempDir,
    data: std::path::PathBuf,
    clock: Arc<FixedClock>,
    host: Host,
}

fn fixture() -> Result<Fixture> {
    let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
    let root = temp.path();
    let paths = Paths::new(root.join("config"), root.join("data"));
    let work = root.join("work");
    fs::create_dir_all(&work).map_err(|err| LenkaError::io("mkdir", &err))?;
    let created = create_project(&paths, &work, Some("api"), &Location::Shared)?;
    let clock = Arc::new(FixedClock::new(1_793_000_000_000));
    let host = Builder::new()
        .plugin(lenka_driver_local::plugin())
        .plugin(lenka_artifact_fs::plugin())
        .plugin(lenka_memory::plugin())
        .paths(paths)
        .clock(Arc::clone(&clock) as Arc<dyn lenka_core::Clock>)
        .build()?;
    Ok(Fixture {
        data: created.directory,
        _temp: temp,
        clock,
        host,
    })
}

fn caller(actor: Actor) -> Caller {
    let label = if actor == Actor::Agent {
        "agent:a"
    } else {
        "human:me"
    };
    Caller {
        actor,
        label: label.to_owned(),
        worktree: "/worktrees/a".into(),
        agent_key: lenka_core::notices::agent_key(Path::new("/worktrees/a"), None),
        scope: ProjectScope::All,
        default_project: Some("api".to_owned()),
    }
}

fn add(fixture: &Fixture, text: &str, tags: &[&str]) -> Result<String> {
    let result = fixture.host.call_tool(
        &caller(Actor::Agent),
        "memory.add",
        json!({"text": text, "tags": tags}),
    )?;
    fixture.clock.advance_ms(1000);
    Ok(result["memory"].as_str().unwrap_or_default().to_owned())
}

fn search(fixture: &Fixture, args: Value) -> Result<Vec<String>> {
    let result = fixture
        .host
        .call_tool(&caller(Actor::Agent), "memory.search", args)?;
    Ok(result["memories"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row["untrusted_data"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect()
        })
        .unwrap_or_default())
}

#[test]
fn search_ranks_by_word_overlap_then_by_newest() -> Result<()> {
    let fixture = fixture()?;
    add(&fixture, "The redis cache runs on port 6379.", &[])?;
    add(&fixture, "The cache has a limit.", &["ops"])?;
    add(&fixture, "The redis cache needs auth.", &[])?;
    add(&fixture, "Nothing here matches.", &[])?;

    let found = search(&fixture, json!({"text": "Redis CACHE"}))?;
    assert_eq!(
        found,
        vec![
            "The redis cache needs auth.",
            "The redis cache runs on port 6379.",
            "The cache has a limit.",
        ]
    );

    assert_eq!(
        search(&fixture, json!({"tag": "ops"}))?,
        vec!["The cache has a limit."]
    );
    assert_eq!(
        search(&fixture, json!({"text": "ops"}))?,
        vec!["The cache has a limit."]
    );
    assert!(search(&fixture, json!({"text": "absent"}))?.is_empty());

    let newest = search(&fixture, json!({"limit": 1}))?;
    assert_eq!(newest, vec!["Nothing here matches."]);
    Ok(())
}

#[test]
fn a_record_is_one_file_with_front_matter_and_a_feed_line() -> Result<()> {
    let fixture = fixture()?;
    add(&fixture, "The text.", &["a"])?;
    let dir = fixture.data.join("memory");
    let files: Vec<_> = fs::read_dir(&dir)
        .map_err(|err| LenkaError::io("list", &err))?
        .filter_map(std::result::Result::ok)
        .collect();
    assert_eq!(files.len(), 1);
    let text = fs::read_to_string(files[0].path()).map_err(|err| LenkaError::io("read", &err))?;
    assert!(text.starts_with("+++\n"), "{text}");
    assert!(text.contains("author = \"agent:a\""), "{text}");
    assert!(text.ends_with("+++\nThe text.\n"), "{text}");
    let feed = fs::read_to_string(fixture.data.join(".runtime/feed.jsonl"))
        .map_err(|err| LenkaError::io("read", &err))?;
    assert!(feed.contains("\"kind\":\"memory\""), "{feed}");
    assert!(
        !feed.contains("The text."),
        "the feed must hold no text: {feed}"
    );
    Ok(())
}

#[test]
fn a_secret_is_refused_and_the_error_hides_the_value() -> Result<()> {
    let fixture = fixture()?;
    let secret = format!("ghp_{}", "a1b2c3d4".repeat(5));
    let Err(error) = add(&fixture, &format!("The token is {secret}"), &[]) else {
        panic!("a secret must fail");
    };
    assert_eq!(error.code(), ErrorCode::SecretDetected);
    assert!(!error.to_string().contains(&secret));
    assert!(search(&fixture, json!({}))?.is_empty());
    Ok(())
}

#[test]
fn an_agent_cannot_delete_when_the_setting_denies_it() -> Result<()> {
    let fixture = fixture()?;
    let id = add(&fixture, "The text.", &[])?;
    let project = fixture.host.open_by_name("api")?;
    let mut config: ProjectConfig = (*project.config()).clone();
    config.limits.agent_delete = false;
    project.save_config(config)?;

    let Err(error) =
        fixture
            .host
            .call_tool(&caller(Actor::Agent), "memory.delete", json!({"id": id}))
    else {
        panic!("the delete must fail");
    };
    assert_eq!(error.code(), ErrorCode::DeleteRefused);

    fixture
        .host
        .call_tool(&caller(Actor::Human), "memory.delete", json!({"id": id}))?;
    assert!(search(&fixture, json!({}))?.is_empty());
    let Err(error) =
        fixture
            .host
            .call_tool(&caller(Actor::Human), "memory.delete", json!({"id": id}))
    else {
        panic!("a second delete must fail");
    };
    assert_eq!(error.code(), ErrorCode::NotFound);
    Ok(())
}

#[test]
fn the_service_validates_input_and_a_task_link() -> Result<()> {
    let fixture = fixture()?;
    let project = fixture.host.open_by_name("api")?;
    let service = memory_of(&project)?;
    let who = caller(Actor::Agent);

    let empty = service.add(
        &who,
        NewMemory {
            text: "  ".to_owned(),
            ..NewMemory::default()
        },
    );
    assert_eq!(
        empty.map(|_| ()).map_err(|error| error.code()),
        Err(ErrorCode::InvalidArgument)
    );

    let large = NewMemory {
        text: "x".repeat(lenka_memory::MAX_TEXT_BYTES + 1),
        ..NewMemory::default()
    };
    assert_eq!(
        service
            .add(&who, large)
            .map(|_| ())
            .map_err(|error| error.code()),
        Err(ErrorCode::InvalidArgument)
    );

    let marker = NewMemory {
        text: "Heading\n=======".to_owned(),
        ..NewMemory::default()
    };
    assert_eq!(
        service
            .add(&who, marker)
            .map(|_| ())
            .map_err(|error| error.code()),
        Err(ErrorCode::InvalidArgument)
    );

    let no_task = NewMemory {
        text: "Text.".to_owned(),
        task: Some("deadbeef".to_owned()),
        ..NewMemory::default()
    };
    assert_eq!(
        service
            .add(&who, no_task)
            .map(|_| ())
            .map_err(|error| error.code()),
        Err(ErrorCode::NotFound)
    );

    let task = lenka_plugin_api::TaskId::generate();
    fs::create_dir_all(fixture.data.join("tasks").join(task.to_string()))
        .map_err(|err| LenkaError::io("mkdir", &err))?;
    let record = service.add(
        &who,
        NewMemory {
            text: "Text.".to_owned(),
            task: Some(task.short()),
            ..NewMemory::default()
        },
    )?;
    assert_eq!(record.task, Some(task));
    assert_eq!(service.list()?.len(), 1);
    assert_eq!(service.resolve(&record.id.short())?, record.id);
    let found = service.search(&MemoryQuery {
        text: Some("text".to_owned()),
        ..MemoryQuery::default()
    })?;
    assert_eq!(found, vec![record]);
    Ok(())
}

#[test]
fn a_bad_file_does_not_hide_other_records() -> Result<()> {
    let fixture = fixture()?;
    add(&fixture, "Good text.", &[])?;
    let id = lenka_plugin_api::MemoryId::generate();
    let dir = fixture.data.join("memory");
    let write = |name: String, text: &str| {
        fs::write(dir.join(name), text).map_err(|err| LenkaError::io("write", &err))
    };
    write(format!("{id}.md"), "<<<<<<< HEAD\nbad\n")?;
    write("not-an-id.md".to_owned(), "bad")?;
    assert_eq!(search(&fixture, json!({}))?, vec!["Good text."]);
    Ok(())
}

#[test]
fn an_unknown_tool_is_not_found() -> Result<()> {
    let fixture = fixture()?;
    let Err(error) = fixture
        .host
        .call_tool(&caller(Actor::Agent), "memory.none", json!({}))
    else {
        panic!("an unknown tool must fail");
    };
    assert_eq!(error.code(), ErrorCode::NotFound);
    Ok(())
}
