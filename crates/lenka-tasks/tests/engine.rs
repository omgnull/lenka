// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! Behavior tests of the task engine: ready work, stages, claims, notices, links, and repair.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lenka_core::init::{Location, create_project};
use lenka_core::settings::ProjectsFile;
use lenka_core::{Builder, FixedClock, Host, Paths, ProjectConfig, Stage, TypeDef};
use lenka_plugin_api::{Actor, Caller, ErrorCode, LenkaError, ProjectScope, Result, TaskId};
use lenka_tasks::{
    ClaimRef, EntryKind, Finish, Link, ListFilter, NewEntry, NewTask, Priority, Status, TaskPatch,
    TaskService, service_of,
};
use proptest::prelude::*;

const HOUR_MS: i64 = 3_600_000;

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    paths: Paths,
    clock: Arc<FixedClock>,
    host: Host,
}

fn fixture() -> Result<Fixture> {
    let temp = tempfile::tempdir().map_err(|err| LenkaError::io("temp", &err))?;
    let root = temp.path().to_path_buf();
    let paths = Paths::new(root.join("config"), root.join("data"));
    let clock = Arc::new(FixedClock::new(1_793_000_000_000));
    let host = build_host(&paths, &clock)?;
    Ok(Fixture {
        _temp: temp,
        root,
        paths,
        clock,
        host,
    })
}

fn build_host(paths: &Paths, clock: &Arc<FixedClock>) -> Result<Host> {
    Builder::new()
        .plugin(lenka_driver_local::plugin())
        .plugin(lenka_artifact_fs::plugin())
        .plugin(lenka_tasks::plugin())
        .paths(paths.clone())
        .clock(Arc::clone(clock) as Arc<dyn lenka_core::Clock>)
        .build()
}

impl Fixture {
    fn project(&self, name: &str) -> Result<Arc<TaskService>> {
        let work = self.root.join("work").join(name);
        fs::create_dir_all(&work).map_err(|err| LenkaError::io("mkdir", &err))?;
        create_project(&self.paths, &work, Some(name), &Location::Shared)?;
        service_of(&self.host.open_by_name(name)?)
    }

    fn agent(&self, name: &str) -> Caller {
        caller(Actor::Agent, name)
    }

    fn human(&self) -> Caller {
        caller(Actor::Human, "me")
    }

    fn data_dir(&self, name: &str) -> PathBuf {
        self.paths.shared_project_dir(name)
    }
}

fn caller(actor: Actor, name: &str) -> Caller {
    let prefix = if actor == Actor::Agent {
        "agent"
    } else {
        "human"
    };
    Caller {
        actor,
        label: format!("{prefix}:{name}"),
        worktree: PathBuf::from(format!("/worktrees/{name}")),
        agent_key: lenka_core::notices::agent_key(Path::new(&format!("/worktrees/{name}")), None),
        scope: ProjectScope::All,
        default_project: None,
    }
}

fn task(title: &str) -> NewTask {
    NewTask {
        title: title.to_owned(),
        description: "Text.".to_owned(),
        ..NewTask::default()
    }
}

fn create(service: &TaskService, who: &Caller, title: &str) -> Result<TaskId> {
    Ok(service.create(who, task(title))?.id())
}

fn code(result: Result<impl std::fmt::Debug>) -> ErrorCode {
    match result {
        Ok(value) => panic!("the call must fail, but it gave {value:?}"),
        Err(error) => error.code(),
    }
}

fn ready_ids(service: &TaskService, who: &Caller) -> Result<Vec<String>> {
    let filter = ListFilter {
        ready: true,
        ..ListFilter::default()
    };
    Ok(service
        .list(who, &filter)?
        .into_iter()
        .map(|row| row.title)
        .collect())
}

fn feature_stages(service: &TaskService) -> Result<()> {
    let mut config: ProjectConfig = (*service.project().config()).clone();
    let stage = |name: &str, actor, unblocks| Stage {
        name: name.to_owned(),
        actor,
        unblocks,
    };
    config.types.insert(
        "feature".to_owned(),
        TypeDef {
            stages: vec![
                stage("code", Actor::Agent, false),
                stage("push", Actor::Agent, true),
                stage("approve", Actor::Human, false),
                stage("deploy", Actor::Agent, false),
            ],
        },
    );
    service.project().save_config(config)
}

#[test]
fn create_show_and_files_on_disk() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let meta = service.create(
        &agent,
        NewTask {
            title: "Add claims".to_owned(),
            description: "Only one agent.".to_owned(),
            requirements: "- Atomic".to_owned(),
            acceptance: "- [ ] One winner".to_owned(),
            plan: Some("Step one.".to_owned()),
            priority: Priority::High,
            labels: vec!["b".to_owned(), "a".to_owned(), "a".to_owned()],
            ..NewTask::default()
        },
    )?;
    let id = meta.id();
    let dir = fx.data_dir("api").join("tasks").join(id.to_string());
    for file in [
        "task.md",
        "plan.md",
        "state.toml",
        "versions/000001/task.md",
        "versions/000001/meta.toml",
    ] {
        assert!(dir.join(file).is_file(), "missing {file}");
    }
    let text = fs::read_to_string(dir.join("task.md")).map_err(|e| LenkaError::io("read", &e))?;
    assert!(text.starts_with("+++\n"));
    assert!(text.contains("## Acceptance criteria\n- [ ] One winner"));

    let detail = service.show(&agent, id, &["plan"])?;
    assert_eq!(detail.description, "Only one agent.");
    assert_eq!(detail.plan.as_deref(), Some("Step one."));
    assert_eq!(detail.criteria.len(), 1);
    assert_eq!(detail.meta.front.labels, vec!["a", "b"]);

    // By default the plan is not in the result.
    assert_eq!(service.show(&agent, id, &[])?.plan, None);
    Ok(())
}

#[test]
fn a_material_change_needs_a_reason_and_a_current_version() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let id = create(&service, &agent, "T")?;

    let patch = |version, reason: Option<&str>, text: &str| TaskPatch {
        expected_version: version,
        reason: reason.map(str::to_owned),
        description: Some(text.to_owned()),
        ..TaskPatch::default()
    };
    assert_eq!(
        code(service.update(&agent, id, patch(1, None, "New"))),
        ErrorCode::ReasonRequired
    );
    let meta = service.update(&agent, id, patch(1, Some("Why"), "New"))?;
    assert_eq!(meta.state.version, 2);
    assert_eq!(
        code(service.update(&agent, id, patch(1, Some("Why"), "Newer"))),
        ErrorCode::StaleVersion
    );

    let versions = service.detail(id, &["versions"])?.versions;
    assert_eq!(versions.len(), 2);
    assert!(versions[1].material);
    assert_eq!(versions[1].changed, vec!["description"]);
    assert_eq!(versions[1].reason, "Why");
    let (old, _) = service.read_version(id, 1)?;
    assert!(old.contains("Text."));
    Ok(())
}

#[test]
fn a_checkbox_is_not_material() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let id = service
        .create(
            &agent,
            NewTask {
                title: "T".to_owned(),
                acceptance: "- [ ] A\n- [ ] B".to_owned(),
                ..NewTask::default()
            },
        )?
        .id();
    let meta = service.update(
        &agent,
        id,
        TaskPatch {
            expected_version: 1,
            check: vec![lenka_tasks::CheckItem {
                index: 1,
                done: true,
            }],
            ..TaskPatch::default()
        },
    )?;
    assert_eq!(meta.state.version, 2);
    let detail = service.detail(id, &["versions"])?;
    assert!(detail.criteria[1].done && !detail.criteria[0].done);
    assert!(!detail.versions[1].material);
    Ok(())
}

#[test]
fn a_secret_is_rejected_and_nothing_is_written() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let value = format!("{}{}", "ghp_", "a1B2".repeat(10));
    let new = NewTask {
        title: "T".to_owned(),
        description: format!("key {value}"),
        ..NewTask::default()
    };
    let Err(error) = service.create(&agent, new.clone()) else {
        panic!("a secret must fail");
    };
    assert_eq!(error.code(), ErrorCode::SecretDetected);
    assert!(!error.message().contains(&value));
    assert!(service.list(&agent, &ListFilter::default())?.is_empty());

    // An agent cannot override, and a person can.
    let forced = NewTask {
        secret_policy: lenka_core::SecretPolicy::PersonOverride,
        ..new
    };
    assert_eq!(
        code(service.create(&agent, forced.clone())),
        ErrorCode::ActorDenied
    );
    assert!(service.create(&fx.human(), forced).is_ok());
    Ok(())
}

#[test]
fn ready_work_rules_and_order() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let first = create(&service, &agent, "first")?;
    let urgent = service
        .create(
            &agent,
            NewTask {
                title: "urgent".to_owned(),
                priority: Priority::Urgent,
                ..NewTask::default()
            },
        )?
        .id();
    let waiting = service
        .create(
            &agent,
            NewTask {
                title: "waiting".to_owned(),
                blocked_by: vec![Link::Local(first)],
                ..NewTask::default()
            },
        )?
        .id();
    let parent = create(&service, &agent, "parent")?;
    service.create(
        &agent,
        NewTask {
            title: "child".to_owned(),
            parent: Some(parent),
            ..NewTask::default()
        },
    )?;
    service.create(
        &agent,
        NewTask {
            title: "later".to_owned(),
            status: Some(Status::Backlog),
            ..NewTask::default()
        },
    )?;

    // A parent with an open child is not ready, and a backlog task is not ready.
    assert_eq!(
        ready_ids(&service, &agent)?,
        vec!["urgent", "first", "child"]
    );

    // A parent's blocker also blocks its child.
    let blocked_parent = service
        .create(
            &agent,
            NewTask {
                title: "bp".to_owned(),
                blocked_by: vec![Link::Local(urgent)],
                ..NewTask::default()
            },
        )?
        .id();
    service.create(
        &agent,
        NewTask {
            title: "bc".to_owned(),
            parent: Some(blocked_parent),
            ..NewTask::default()
        },
    )?;
    assert!(!ready_ids(&service, &agent)?.contains(&"bc".to_owned()));

    let detail = service.detail(waiting, &[])?;
    assert_eq!(detail.blockers.len(), 1);
    assert_eq!(
        code(service.claim(&agent, Some(waiting), None, None)),
        ErrorCode::Blocked
    );

    let claim = service.claim(&agent, Some(first), None, None)?;
    service.finish(
        &agent,
        first,
        &ClaimRef::Handle(claim.handle),
        &Finish::Done {
            evidence: "cargo test: ok".to_owned(),
            unchecked_reason: None,
        },
    )?;
    assert!(ready_ids(&service, &agent)?.contains(&"waiting".to_owned()));
    Ok(())
}

#[test]
fn next_ready_claim_takes_the_top_task_only_once() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let one = fx.agent("one");
    let two = fx.agent("two");
    let a = create(&service, &one, "a")?;
    let b = create(&service, &one, "b")?;
    assert_eq!(service.claim(&one, None, None, None)?.task, a);
    assert_eq!(service.claim(&two, None, None, None)?.task, b);
    assert_eq!(
        code(service.claim(&two, None, None, None)),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(service.claim(&two, Some(a), None, None)),
        ErrorCode::ClaimLost
    );
    Ok(())
}

#[test]
fn a_claim_expires_and_the_first_agent_loses_it() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let one = fx.agent("one");
    let two = fx.agent("two");
    let id = create(&service, &one, "a")?;
    let claim = service.claim(&one, Some(id), None, Some(30))?;

    // A repeat call with the handle prolongs the claim.
    fx.clock.advance_ms(20 * 60_000);
    let prolonged = service.claim(&one, None, Some(&claim.handle), Some(30))?;
    assert!(prolonged.expires_at > claim.expires_at);
    fx.clock.advance_ms(20 * 60_000);
    assert_eq!(
        code(service.claim(&two, Some(id), None, None)),
        ErrorCode::ClaimLost
    );

    fx.clock.advance_ms(HOUR_MS);
    assert_eq!(ready_ids(&service, &two)?, vec!["a"]);
    let second = service.claim(&two, Some(id), None, None)?;
    assert_ne!(second.handle, claim.handle);

    let handle = ClaimRef::Handle(claim.handle);
    let done = Finish::Done {
        evidence: "x".to_owned(),
        unchecked_reason: None,
    };
    assert_eq!(
        code(service.finish(&one, id, &handle, &done)),
        ErrorCode::ClaimLost
    );
    assert_eq!(
        code(service.update(
            &one,
            id,
            TaskPatch {
                expected_version: 1,
                claim: handle,
                title: Some("N".to_owned()),
                ..TaskPatch::default()
            }
        )),
        ErrorCode::ClaimLost
    );
    Ok(())
}

#[test]
fn stages_move_and_a_human_stage_needs_a_person() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    feature_stages(&service)?;
    let agent = fx.agent("a");
    let id = service
        .create(
            &agent,
            NewTask {
                title: "F".to_owned(),
                kind: "feature".to_owned(),
                ..NewTask::default()
            },
        )?
        .id();
    let dependent = service
        .create(
            &agent,
            NewTask {
                title: "D".to_owned(),
                blocked_by: vec![Link::Local(id)],
                ..NewTask::default()
            },
        )?
        .id();

    let c = service.claim(&agent, Some(id), None, None)?;
    assert_eq!(c.stage, "code");
    assert_eq!(
        code(service.advance(&agent, id, &ClaimRef::Handle(c.handle.clone()), "", None)),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        code(service.finish(
            &agent,
            id,
            &ClaimRef::Handle(c.handle.clone()),
            &Finish::Done {
                evidence: "x".to_owned(),
                unchecked_reason: None
            }
        )),
        ErrorCode::InvalidArgument,
        "later stages remain"
    );
    let state = service.advance(&agent, id, &ClaimRef::Handle(c.handle), "built", None)?;
    assert_eq!(state.stage, "push");
    assert!(!state.unblocked);
    assert_eq!(
        code(service.claim(&agent, Some(dependent), None, None)),
        ErrorCode::Blocked
    );

    // The `unblocks` stage releases dependent tasks when it completes.
    let c = service.claim(&agent, Some(id), None, None)?;
    let state = service.advance(&agent, id, &ClaimRef::Handle(c.handle), "pushed", None)?;
    assert_eq!(state.stage, "approve");
    assert!(state.unblocked);
    assert!(ready_ids(&service, &agent)?.contains(&"D".to_owned()));

    // An agent cannot do the human stage.
    assert_eq!(
        code(service.claim(&agent, Some(id), None, None)),
        ErrorCode::ActorDenied
    );
    assert!(!ready_ids(&service, &agent)?.contains(&"F".to_owned()));
    let human = fx.human();
    assert!(ready_ids(&service, &human)?.contains(&"F".to_owned()));
    let c = service.claim(&human, Some(id), None, None)?;
    let state = service.advance(&human, id, &ClaimRef::Handle(c.handle), "approved", None)?;
    assert_eq!(state.stage, "deploy");

    // A failure returns to an earlier stage with a reason, and the release ends.
    let c = service.claim(&agent, Some(id), None, None)?;
    let state = service.advance(
        &agent,
        id,
        &ClaimRef::Handle(c.handle),
        "deploy failed",
        Some("code"),
    )?;
    assert_eq!(state.stage, "code");
    assert!(!state.unblocked);
    assert!(!ready_ids(&service, &agent)?.contains(&"D".to_owned()));

    let entries = service.detail(id, &["entries"])?.entries;
    assert_eq!(entries.len(), 4);
    assert!(
        entries
            .iter()
            .all(|entry| entry.kind == EntryKind::Evidence)
    );
    Ok(())
}

#[test]
fn finish_rules() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let id = service
        .create(
            &agent,
            NewTask {
                title: "T".to_owned(),
                acceptance: "- [ ] A".to_owned(),
                ..NewTask::default()
            },
        )?
        .id();
    let c = service.claim(&agent, Some(id), None, None)?;
    let handle = ClaimRef::Handle(c.handle);
    let done = |evidence: &str, reason: Option<&str>| Finish::Done {
        evidence: evidence.to_owned(),
        unchecked_reason: reason.map(str::to_owned),
    };
    assert_eq!(
        code(service.finish(&agent, id, &handle, &done("", None))),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        code(service.finish(&agent, id, &handle, &done("ok", None))),
        ErrorCode::InvalidArgument
    );
    let state = service.finish(&agent, id, &handle, &done("ok", Some("out of scope")))?;
    assert_eq!(state.status, Status::Done);
    assert!(service.detail(id, &[])?.claim.is_none());

    // Release makes the task ready again.
    let other = create(&service, &agent, "R")?;
    let c = service.claim(&agent, Some(other), None, None)?;
    assert_eq!(
        service
            .finish(
                &agent,
                other,
                &ClaimRef::Handle(c.handle),
                &Finish::Released
            )?
            .status,
        Status::Todo
    );
    assert_eq!(ready_ids(&service, &agent)?, vec!["R"]);

    // A paused task is not ready until a claim names it.
    service.pause(&agent, other, &ClaimRef::Implicit)?;
    assert!(ready_ids(&service, &agent)?.is_empty());
    assert!(service.claim(&agent, Some(other), None, None).is_ok());
    Ok(())
}

#[test]
fn another_holder_blocks_an_update_and_a_person_can_override() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let one = fx.agent("one");
    let two = fx.agent("two");
    let id = create(&service, &one, "T")?;
    service.claim(&one, Some(id), None, None)?;
    let patch = |claim| TaskPatch {
        expected_version: 1,
        title: Some("N".to_owned()),
        claim,
        ..TaskPatch::default()
    };
    assert_eq!(
        code(service.update(&two, id, patch(ClaimRef::Implicit))),
        ErrorCode::ClaimLost
    );
    assert_eq!(
        code(service.update(&two, id, patch(ClaimRef::Override))),
        ErrorCode::ActorDenied
    );
    assert!(
        service
            .update(&fx.human(), id, patch(ClaimRef::Override))
            .is_ok()
    );
    Ok(())
}

#[test]
fn cycles_are_rejected() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let a = create(&service, &agent, "a")?;
    let b = service
        .create(
            &agent,
            NewTask {
                title: "b".to_owned(),
                blocked_by: vec![Link::Local(a)],
                ..NewTask::default()
            },
        )?
        .id();
    let add_blocker = |target| TaskPatch {
        expected_version: 1,
        blocked_by_add: vec![Link::Local(target)],
        ..TaskPatch::default()
    };
    assert_eq!(
        code(service.update(&agent, a, add_blocker(b))),
        ErrorCode::Cycle
    );
    assert_eq!(
        code(service.update(&agent, a, add_blocker(a))),
        ErrorCode::Cycle
    );

    // A parent cannot be its own descendant, and a child cannot wait for its ancestor.
    let child = service
        .create(
            &agent,
            NewTask {
                title: "c".to_owned(),
                parent: Some(a),
                ..NewTask::default()
            },
        )?
        .id();
    assert_eq!(
        code(service.update(
            &agent,
            a,
            TaskPatch {
                expected_version: 1,
                parent: Some(Some(child)),
                ..TaskPatch::default()
            }
        )),
        ErrorCode::Cycle
    );
    assert_eq!(
        code(service.update(&agent, child, add_blocker(a))),
        ErrorCode::Cycle
    );
    assert_eq!(
        code(service.update(&agent, a, add_blocker(child))),
        ErrorCode::Cycle
    );
    Ok(())
}

#[test]
fn delete_rules() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let parent = create(&service, &agent, "p")?;
    let child = service
        .create(
            &agent,
            NewTask {
                title: "c".to_owned(),
                parent: Some(parent),
                ..NewTask::default()
            },
        )?
        .id();
    let dep = service
        .create(
            &agent,
            NewTask {
                title: "d".to_owned(),
                blocked_by: vec![Link::Local(child)],
                ..NewTask::default()
            },
        )?
        .id();
    assert_eq!(
        code(service.delete(&agent, parent)),
        ErrorCode::DeleteRefused
    );
    assert_eq!(
        code(service.delete(&agent, child)),
        ErrorCode::DeleteRefused
    );
    service.delete(&agent, dep)?;
    service.delete(&agent, child)?;
    service.delete(&agent, parent)?;
    assert!(service.list(&agent, &ListFilter::default())?.is_empty());
    assert_eq!(code(service.detail(parent, &[])), ErrorCode::NotFound);

    let mut config = (*service.project().config()).clone();
    config.limits.agent_delete = false;
    service.project().save_config(config)?;
    let other = create(&service, &agent, "o")?;
    assert_eq!(
        code(service.delete(&agent, other)),
        ErrorCode::DeleteRefused
    );
    assert!(service.delete(&fx.human(), other).is_ok());
    Ok(())
}

#[test]
fn notices_route_to_claim_holders_and_block_finish_until_read() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (one, two, three, four) = (
        fx.agent("one"),
        fx.agent("two"),
        fx.agent("three"),
        fx.agent("four"),
    );
    let contract = create(&service, &one, "contract")?;
    let parent_child = service
        .create(
            &one,
            NewTask {
                title: "child".to_owned(),
                parent: Some(contract),
                ..NewTask::default()
            },
        )?
        .id();
    let dependent = service
        .create(
            &one,
            NewTask {
                title: "dep".to_owned(),
                blocked_by: vec![Link::Local(contract)],
                ..NewTask::default()
            },
        )?
        .id();
    let unrelated = create(&service, &one, "other")?;
    let c2 = service.claim(&two, Some(parent_child), None, None)?;
    assert_eq!(service.detail(dependent, &[])?.blockers.len(), 1);
    service.claim(&four, Some(unrelated), None, None)?;

    let meta = service.update(
        &one,
        contract,
        TaskPatch {
            expected_version: 1,
            description: Some("Contract v2".to_owned()),
            reason: Some("Field renamed".to_owned()),
            ..TaskPatch::default()
        },
    )?;
    assert_eq!(meta.state.version, 2);

    let notes = service.call_notes(&two)?;
    assert_eq!(notes.unread.len(), 1, "a child holder gets a notice");
    assert_eq!(notes.unread[0].version, 2);
    assert_eq!(notes.unread[0].reason, "Field renamed");
    assert_eq!(notes.unread[0].changed, vec!["description"]);
    assert!(
        service.call_notes(&one)?.unread.is_empty(),
        "the author gets no notice"
    );
    assert!(service.call_notes(&three)?.unread.is_empty());
    assert!(
        service.call_notes(&four)?.unread.is_empty(),
        "an unrelated holder gets no notice"
    );

    let handle = ClaimRef::Handle(c2.handle);
    let done = Finish::Done {
        evidence: "ok".to_owned(),
        unchecked_reason: None,
    };
    assert_eq!(
        code(service.finish(&two, parent_child, &handle, &done)),
        ErrorCode::UnreadChanges
    );

    // A read of the new version closes the notice, with no extra tool.
    service.show(&two, contract, &[])?;
    assert!(service.call_notes(&two)?.unread.is_empty());
    assert!(service.finish(&two, parent_child, &handle, &done).is_ok());
    Ok(())
}

#[test]
fn a_second_process_sees_changes_through_the_feed() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let id = create(&service, &agent, "T")?;

    // A second host stands for a second process with its own index.
    let other_host = build_host(&fx.paths, &fx.clock)?;
    let other = service_of(&other_host.open_by_name("api")?)?;
    assert_eq!(other.detail(id, &[])?.meta.state.version, 1);

    service.update(
        &agent,
        id,
        TaskPatch {
            expected_version: 1,
            title: Some("New".to_owned()),
            ..TaskPatch::default()
        },
    )?;
    assert_eq!(other.detail(id, &[])?.meta.front.title, "New");
    let added = create(&service, &agent, "Added")?;
    assert!(other.resolve(&added.short()).is_ok());
    service.delete(&agent, added)?;
    assert_eq!(code(other.resolve(&added.short())), ErrorCode::NotFound);

    // The other process also sees claims.
    let two = fx.agent("two");
    service.claim(&agent, Some(id), None, None)?;
    assert_eq!(
        code(other.claim(&two, Some(id), None, None)),
        ErrorCode::ClaimLost
    );
    Ok(())
}

#[test]
fn an_external_edit_becomes_a_version_and_an_invalid_file_is_reported() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let id = create(&service, &agent, "T")?;
    let file = fx
        .data_dir("api")
        .join("tasks")
        .join(id.to_string())
        .join("task.md");
    let write = |text: String| fs::write(&file, text).map_err(|err| LenkaError::io("write", &err));
    let text = fs::read_to_string(&file).map_err(|err| LenkaError::io("read", &err))?;

    write(text.replace("Text.", "Edited by hand."))?;
    let detail = service.detail(id, &["versions"])?;
    assert_eq!(detail.description, "Edited by hand.");
    assert_eq!(detail.meta.state.version, 2);
    assert_eq!(detail.versions[1].author, "external");
    assert_eq!(detail.versions[1].changed, vec!["description"]);

    // A writer with the old version gets a stale error, and does not overwrite the edit.
    let patch = TaskPatch {
        expected_version: 1,
        title: Some("N".to_owned()),
        ..TaskPatch::default()
    };
    assert_eq!(
        code(service.update(&agent, id, patch)),
        ErrorCode::StaleVersion
    );

    // Conflict markers make the file invalid, and the last valid text stays readable.
    let current = fs::read_to_string(&file).map_err(|err| LenkaError::io("read", &err))?;
    write(format!(
        "{current}\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> other\n"
    ))?;
    let reopened = build_host(&fx.paths, &fx.clock)?;
    let fresh = service_of(&reopened.open_by_name("api")?)?;
    assert_eq!(code(fresh.show(&agent, id, &[])), ErrorCode::InvalidFile);
    assert!(
        fresh
            .doctor()?
            .invalid_files
            .iter()
            .any(|line| line.contains("conflict"))
    );

    write(current)?;
    fresh.reindex(&fx.human())?;
    assert!(fresh.show(&agent, id, &[]).is_ok());
    Ok(())
}

#[test]
fn a_stop_at_each_write_step_is_repaired() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    // A person has no loop limit, so the loop can make many changes.
    let agent = fx.human();
    let id = create(&service, &agent, "T")?;
    let project = Arc::clone(service.project());
    let mut completed = 0;
    for step in 1_u64..=12 {
        let version = step;
        project
            .crash_point()
            .set(usize::try_from(step).unwrap_or(0));
        let title = format!("Title {step}");
        let result = service.update(
            &agent,
            id,
            TaskPatch {
                expected_version: version,
                description: Some(format!("Text {step}")),
                reason: Some("r".to_owned()),
                title: Some(title.clone()),
                ..TaskPatch::default()
            },
        );
        project.crash_point().set(0);

        // A fresh process must see the new version in full, because the journal is complete
        // before the first operation applies.
        let other_host = build_host(&fx.paths, &fx.clock)?;
        let other = service_of(&other_host.open_by_name("api")?)?;
        let detail = other.detail(id, &["versions"])?;
        assert_eq!(
            detail.meta.front.title, title,
            "step {step}, result {result:?}"
        );
        assert_eq!(detail.description, format!("Text {step}"), "step {step}");
        assert_eq!(detail.meta.state.version, version + 1, "step {step}");
        assert_eq!(
            detail.versions.last().map(|v| v.version),
            Some(version + 1),
            "step {step}"
        );
        assert!(other.doctor()?.is_clean(), "step {step}");
        if result.is_ok() {
            completed += 1;
        }
    }
    assert!(completed > 0, "the loop must pass the last step of a write");
    Ok(())
}

fn secret_value() -> String {
    format!("{}{}", "ghp_", "a1B2".repeat(10))
}

fn write_file(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).map_err(|err| LenkaError::io("write", &err))
}

#[cfg(unix)]
#[test]
fn a_symlink_in_a_write_path_stops_only_that_write() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let bad = create(&service, &human, "bad")?;
    let good = create(&service, &human, "good")?;
    let versions = fx
        .data_dir("api")
        .join("tasks")
        .join(bad.to_string())
        .join("versions");
    let aside = fx.root.join("aside");
    fs::rename(&versions, &aside).map_err(|err| LenkaError::io("rename", &err))?;
    std::os::unix::fs::symlink(&aside, &versions).map_err(|err| LenkaError::io("link", &err))?;
    let patch = |version| TaskPatch {
        expected_version: version,
        title: Some("New".to_owned()),
        ..TaskPatch::default()
    };
    assert_eq!(
        code(service.update(&human, bad, patch(1))),
        ErrorCode::InvalidFile
    );
    // The failure left no journal, so other work continues.
    let journal = fx.data_dir("api").join(".runtime/pending.json");
    assert_eq!(fs::metadata(&journal).map_or(0, |meta| meta.len()), 0);
    assert_eq!(service.update(&human, good, patch(1))?.state.version, 2);
    assert_eq!(service.list(&human, &ListFilter::default())?.len(), 2);
    Ok(())
}

#[test]
fn labels_cannot_hold_a_secret() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let mut new = task("T");
    new.labels = vec![secret_value()];
    assert_eq!(code(service.create(&agent, new)), ErrorCode::SecretDetected);
    let id = create(&service, &agent, "T")?;
    let patch = TaskPatch {
        expected_version: 1,
        labels_add: vec![secret_value()],
        ..TaskPatch::default()
    };
    assert_eq!(
        code(service.update(&agent, id, patch)),
        ErrorCode::SecretDetected
    );
    Ok(())
}

#[test]
fn field_text_cannot_change_the_task_structure() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    for bad in [
        "```\ncode",
        "## Requirements\nFAKE",
        "a\n=======\nb",
        "<<<<<<< HEAD",
    ] {
        let mut new = task("T");
        new.description = bad.to_owned();
        assert_eq!(
            code(service.create(&agent, new)),
            ErrorCode::InvalidArgument,
            "{bad}"
        );
    }
    let mut new = task("T");
    new.description = "### Part\n```\n## In a fence\n```\n".to_owned();
    new.acceptance = "- [ ] must pass".to_owned();
    let id = service.create(&agent, new)?.id();
    let detail = service.detail(id, &[])?;
    assert_eq!(detail.criteria.len(), 1);
    let patch = TaskPatch {
        expected_version: 1,
        description: Some("```\nhide".to_owned()),
        reason: Some("r".to_owned()),
        ..TaskPatch::default()
    };
    assert_eq!(
        code(service.update(&agent, id, patch)),
        ErrorCode::InvalidArgument
    );
    let mut plan = task("T");
    plan.plan = Some("a\n>>>>>>> branch\nb".to_owned());
    assert_eq!(
        code(service.create(&agent, plan)),
        ErrorCode::InvalidArgument
    );
    Ok(())
}

#[test]
fn a_task_over_the_size_limit_stays_out() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let mut new = task("T");
    new.description = "x".repeat(900 * 1024);
    new.requirements = "y".repeat(900 * 1024);
    assert_eq!(
        code(service.create(&agent, new)),
        ErrorCode::InvalidArgument
    );
    assert!(service.list(&agent, &ListFilter::default())?.is_empty());
    assert!(service.doctor()?.is_clean());
    Ok(())
}

#[test]
fn a_new_process_finds_an_invalid_task_again() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let id = create(&service, &human, "T")?;
    let file = fx
        .data_dir("api")
        .join("tasks")
        .join(id.to_string())
        .join("task.md");
    let text = fs::read_to_string(&file).map_err(|err| LenkaError::io("read", &err))?;
    write_file(&file, &format!("{text}\n<<<<<<< HEAD\n"))?;
    for _ in 0..3 {
        let host = build_host(&fx.paths, &fx.clock)?;
        let other = service_of(&host.open_by_name("api")?)?;
        assert!(!other.detail(id, &[])?.warnings.is_empty());
        assert!(ready_ids(&other, &human)?.is_empty());
        assert_eq!(
            code(other.claim(&human, Some(id), None, None)),
            ErrorCode::InvalidFile
        );
    }
    Ok(())
}

#[test]
fn a_group_of_tasks_cannot_wait_for_itself() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let parent = create(&service, &human, "P")?;

    // A new child that waits for its parent.
    let mut child = task("C");
    child.parent = Some(parent);
    child.blocked_by = vec![Link::Local(parent)];
    assert_eq!(code(service.create(&human, child)), ErrorCode::Cycle);

    // Q is a child of P, Q waits for R, and R would wait for P.
    let mut q = task("Q");
    q.parent = Some(parent);
    let q = service.create(&human, q)?.id();
    let r = create(&service, &human, "R")?;
    let link = |target: TaskId| TaskPatch {
        expected_version: 1,
        blocked_by_add: vec![Link::Local(target)],
        ..TaskPatch::default()
    };
    service.update(&human, q, link(r))?;
    assert_eq!(
        code(service.update(&human, r, link(parent))),
        ErrorCode::Cycle
    );

    // A task cannot take its blocker as its parent.
    let blocker = create(&service, &human, "B")?;
    let waiter = create(&service, &human, "W")?;
    service.update(&human, waiter, link(blocker))?;
    let to_parent = TaskPatch {
        expected_version: 2,
        parent: Some(Some(blocker)),
        ..TaskPatch::default()
    };
    assert_eq!(
        code(service.update(&human, waiter, to_parent)),
        ErrorCode::Cycle
    );

    // The same patch cannot set a parent and a blocker on that parent.
    let free = create(&service, &human, "F")?;
    let both = TaskPatch {
        expected_version: 1,
        parent: Some(Some(blocker)),
        blocked_by_add: vec![Link::Local(blocker)],
        ..TaskPatch::default()
    };
    assert_eq!(code(service.update(&human, free, both)), ErrorCode::Cycle);
    assert!(service.doctor()?.is_clean());
    Ok(())
}

#[test]
fn a_notice_for_a_deleted_task_stops_no_work() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let human = fx.human();
    let blocker = create(&service, &human, "Dependency")?;
    let mut new = task("Work");
    new.relates = vec![Link::Local(blocker)];
    let work = service.create(&agent, new)?.id();
    let claim = service.claim(&agent, Some(work), None, None)?;
    let patch = TaskPatch {
        expected_version: 1,
        description: Some("Changed.".to_owned()),
        reason: Some("A change.".to_owned()),
        ..TaskPatch::default()
    };
    service.update(&human, blocker, patch)?;
    assert_eq!(service.call_notes(&agent)?.unread.len(), 1);
    let unlink = TaskPatch {
        expected_version: 1,
        relates_remove: vec![Link::Local(blocker)],
        ..TaskPatch::default()
    };
    service.update(&agent, work, unlink)?;
    service.delete(&human, blocker)?;
    assert!(service.call_notes(&agent)?.unread.is_empty());
    let finish = Finish::Done {
        evidence: "ok".to_owned(),
        unchecked_reason: None,
    };
    service.finish(&agent, work, &ClaimRef::Handle(claim.handle), &finish)?;
    Ok(())
}

#[test]
fn an_expired_claim_stops_a_write_with_no_handle() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let other = fx.agent("b");
    let id = create(&service, &agent, "T")?;
    service.claim(&agent, Some(id), None, Some(30))?;
    fx.clock.advance_ms(HOUR_MS);
    let patch = |title: &str| TaskPatch {
        expected_version: 1,
        title: Some(title.to_owned()),
        ..TaskPatch::default()
    };
    assert_eq!(
        code(service.update(&agent, id, patch("Late"))),
        ErrorCode::ClaimLost
    );
    // An agent that never held the claim and a new claim of the same agent can both write.
    assert_eq!(service.update(&other, id, patch("Other"))?.state.version, 2);
    service.claim(&agent, Some(id), None, Some(30))?;
    let again = TaskPatch {
        expected_version: 2,
        title: Some("Again".to_owned()),
        ..TaskPatch::default()
    };
    assert_eq!(service.update(&agent, id, again)?.state.version, 3);
    Ok(())
}

#[test]
fn a_person_can_release_with_an_override() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let human = fx.human();
    let id = create(&service, &agent, "T")?;
    service.claim(&agent, Some(id), None, None)?;
    let state = service.finish(&human, id, &ClaimRef::Override, &Finish::Released)?;
    assert_eq!(state.status, Status::Todo);
    assert_eq!(
        code(service.finish(&agent, id, &ClaimRef::Override, &Finish::Released)),
        ErrorCode::ActorDenied
    );
    Ok(())
}

#[test]
fn a_child_with_an_invalid_state_holds_its_parent_open() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let parent = create(&service, &human, "P")?;
    let mut child = task("C");
    child.parent = Some(parent);
    let child = service.create(&human, child)?.id();
    let state = fx
        .data_dir("api")
        .join("tasks")
        .join(child.to_string())
        .join("state.toml");
    write_file(&state, "not valid = [")?;
    let snapshot = fx.data_dir("api").join(".runtime/index/snapshot.json");
    let _ = fs::remove_file(snapshot);
    let host = build_host(&fx.paths, &fx.clock)?;
    let other = service_of(&host.open_by_name("api")?)?;
    let finish = Finish::Done {
        evidence: "ok".to_owned(),
        unchecked_reason: None,
    };
    assert_eq!(
        code(other.finish(&human, parent, &ClaimRef::Implicit, &finish)),
        ErrorCode::Blocked
    );
    assert!(!other.doctor()?.is_clean());
    Ok(())
}

#[test]
fn a_hostile_artifact_record_gives_invalid_file() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let id = lenka_plugin_api::ArtifactId::generate();
    let dir = fx.data_dir("api").join("artifacts").join(id.to_string());
    fs::create_dir_all(&dir).map_err(|err| LenkaError::io("mkdir", &err))?;
    let head = format!(
        "id = \"{id}\"\nname = \"a.txt\"\nmedia_type = \"text/plain\"\nversion = 1\n\
         created_by = \"agent:a\"\ncreated_at = \"2026-11-06T12:00:00Z\"\n"
    );
    let version = |file: &str| {
        format!(
            "[[versions]]\nversion = 1\nfile = \"{file}\"\nsize = 1\nsha256 = \"00\"\n\
             author = \"agent:a\"\nat = \"2026-11-06T12:00:00Z\"\nreason = \"\"\n"
        )
    };
    let hostile = [
        format!("{head}versions = []\n"),
        format!("{head}{}", version("../../../../outside.txt")),
        format!("{head}{}", version("..")),
        format!("{head}{}", version("a\\\\b")),
        head.replace("name = \"a.txt\"", "name = \"../a\"") + &version("a.txt"),
        head.replace("version = 1", "version = 9") + &version("a.txt"),
        head.replace(
            &id.to_string(),
            &lenka_plugin_api::ArtifactId::generate().to_string(),
        ) + &version("a.txt"),
    ];
    for text in hostile {
        fs::write(dir.join("artifact.toml"), &text).map_err(|err| LenkaError::io("write", &err))?;
        let error = lenka_tasks::read_artifact(service.project(), id)
            .err()
            .ok_or_else(|| LenkaError::invalid_argument(format!("accepted: {text}")))?;
        assert_eq!(
            error.code(),
            lenka_plugin_api::ErrorCode::InvalidFile,
            "{text}"
        );
    }
    let good = format!("{head}{}", version("a.txt"));
    fs::write(dir.join("artifact.toml"), good).map_err(|err| LenkaError::io("write", &err))?;
    assert!(lenka_tasks::read_artifact(service.project(), id)?.is_some());
    Ok(())
}

#[test]
fn a_torn_journal_changes_nothing_and_strict_flush_works() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let id = create(&service, &human, "T")?;
    let journal = fx.data_dir("api").join(".runtime/pending.json");
    fs::write(&journal, "[{\"op\":\"put\",\"path\":\"tasks/")
        .map_err(|err| LenkaError::io("write", &err))?;
    let host = build_host(&fx.paths, &fx.clock)?;
    let other = service_of(&host.open_by_name("api")?)?;
    assert_eq!(other.detail(id, &[])?.meta.state.version, 1);
    assert_eq!(
        fs::metadata(&journal)
            .map_err(|err| LenkaError::io("stat", &err))?
            .len(),
        0
    );

    let mut config = (*service.project().config()).clone();
    config.limits.strict_flush = true;
    service.project().save_config(config)?;
    let meta = service.update(
        &human,
        id,
        TaskPatch {
            expected_version: 1,
            title: Some("N".to_owned()),
            ..TaskPatch::default()
        },
    )?;
    assert_eq!(meta.state.version, 2);
    Ok(())
}

#[test]
fn the_loop_limit_pauses_a_task_until_a_person_acts() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (agent, human) = (fx.agent("a"), fx.human());
    let id = create(&service, &agent, "T")?;
    let change = |version: u64| TaskPatch {
        expected_version: version,
        description: Some(format!("Text {version}")),
        reason: Some("r".to_owned()),
        ..TaskPatch::default()
    };
    for version in 1..=5 {
        service.update(&agent, id, change(version))?;
    }
    assert_eq!(
        code(service.update(&agent, id, change(6))),
        ErrorCode::LoopLimit
    );
    assert_eq!(service.detail(id, &[])?.meta.state.status, Status::Paused);
    assert!(ready_ids(&service, &agent)?.is_empty());

    service.set_status(&human, id, Status::Todo, &ClaimRef::Implicit)?;
    assert!(service.update(&agent, id, change(6)).is_ok());
    assert_eq!(
        code(service.set_status(&agent, id, Status::Todo, &ClaimRef::Implicit)),
        ErrorCode::ActorDenied
    );
    Ok(())
}

#[test]
fn a_coordinator_holds_the_plan_and_workers_propose() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (lead, worker, human) = (fx.agent("lead"), fx.agent("worker"), fx.human());
    let parent = create(&service, &human, "tree")?;
    let meta = service.update(
        &human,
        parent,
        TaskPatch {
            expected_version: 1,
            coordinated: Some(true),
            ..TaskPatch::default()
        },
    )?;
    assert!(meta.front.coordinated);
    assert_eq!(
        code(service.update(
            &lead,
            parent,
            TaskPatch {
                expected_version: 2,
                coordinated: Some(false),
                ..TaskPatch::default()
            }
        )),
        ErrorCode::ActorDenied
    );
    let child = service
        .create(
            &human,
            NewTask {
                title: "c".to_owned(),
                parent: Some(parent),
                ..NewTask::default()
            },
        )?
        .id();

    // A coordinator can name a coordinated task that has open children.
    service.claim(&lead, Some(parent), None, None)?;

    let plan = |version| TaskPatch {
        expected_version: version,
        plan: Some("My plan".to_owned()),
        reason: Some("r".to_owned()),
        ..TaskPatch::default()
    };
    assert_eq!(
        code(service.update(&worker, child, plan(1))),
        ErrorCode::ActorDenied
    );
    assert!(service.update(&lead, child, plan(1)).is_ok());

    service.add_entry(
        &worker,
        child,
        NewEntry {
            kind: EntryKind::Proposal,
            text: "Change it".to_owned(),
            replaces: None,
            secret_policy: lenka_core::SecretPolicy::Enforce,
        },
    )?;
    let notes = service.call_notes(&lead)?;
    assert!(
        notes
            .unread
            .iter()
            .any(|notice| notice.changed == ["proposal"])
    );
    Ok(())
}

#[test]
fn soft_links_between_projects() -> Result<()> {
    let fx = fixture()?;
    let api = fx.project("api")?;
    let billing = fx.project("billing")?;
    let agent = fx.agent("a");
    let target = create(&billing, &agent, "billing task")?;
    let link = Link::Remote(billing.project().id(), target);
    let new = || NewTask {
        title: "local".to_owned(),
        blocked_by: vec![link],
        ..NewTask::default()
    };

    // The default is no links.
    assert_eq!(code(api.create(&agent, new())), ErrorCode::LinkDenied);
    let allow = |service: &TaskService, to: Vec<String>, from: Vec<String>| -> Result<()> {
        let mut config = (*service.project().config()).clone();
        config.links.link_to = to;
        config.links.accept_from = from;
        service.project().save_config(config)
    };
    allow(&api, vec![billing.project().id().to_string()], vec![])?;
    assert_eq!(
        code(api.create(&agent, new())),
        ErrorCode::LinkDenied,
        "both sides must allow"
    );
    allow(&billing, vec![], vec!["*".to_owned()])?;

    let local = api.create(&agent, new())?.id();
    let detail = api.detail(local, &[])?;
    assert_eq!(detail.soft_links[0].state, lenka_tasks::LinkState::Ok);
    assert_eq!(detail.soft_links[0].title.as_deref(), Some("billing task"));
    assert_eq!(detail.blockers.len(), 1);
    assert!(ready_ids(&api, &agent)?.is_empty());

    // A claim gets a cross-project notice at the next call, after the target changes.
    let helper = create(&api, &agent, "holder")?;
    let relating = api.update(
        &agent,
        helper,
        TaskPatch {
            expected_version: 1,
            relates_add: vec![link],
            ..TaskPatch::default()
        },
    )?;
    assert_eq!(relating.state.version, 2);
    api.claim(&agent, Some(helper), None, None)?;
    assert!(api.call_notes(&agent)?.cross_project.is_empty());
    billing.update(
        &fx.agent("b"),
        target,
        TaskPatch {
            expected_version: 1,
            title: Some("renamed".to_owned()),
            ..TaskPatch::default()
        },
    )?;
    let notes = api.call_notes(&agent)?;
    assert_eq!(notes.cross_project.len(), 1);
    assert_eq!(notes.cross_project[0].title.as_deref(), Some("renamed"));
    assert!(api.call_notes(&agent)?.cross_project.is_empty());

    // A finished target releases the link.
    let human = fx.human();
    billing.finish(
        &human,
        target,
        &ClaimRef::Implicit,
        &Finish::Done {
            evidence: "ok".to_owned(),
            unchecked_reason: None,
        },
    )?;
    assert!(api.detail(local, &[])?.blockers.is_empty());
    billing.set_status(&human, target, Status::Todo, &ClaimRef::Implicit)?;
    assert_eq!(api.detail(local, &[])?.blockers.len(), 1);

    // A policy that no longer allows the link makes it broken, which no longer blocks.
    allow(&billing, vec![], vec![])?;
    let view = &api.detail(local, &[])?.soft_links[0];
    assert_eq!(view.state, lenka_tasks::LinkState::Broken);
    assert_eq!(view.title, None, "a broken link shows no data");
    assert!(api.detail(local, &[])?.blockers.is_empty());
    assert_eq!(
        api.doctor()?.broken_links.len(),
        2,
        "the blocker link and the relates link"
    );

    // A deleted target looks the same as a denied target.
    allow(&billing, vec![], vec!["*".to_owned()])?;
    billing.delete(&human, target)?;
    assert_eq!(
        api.detail(local, &[])?.soft_links[0].state,
        lenka_tasks::LinkState::Broken
    );

    // A project that this machine does not have still blocks.
    let mut list = ProjectsFile::load(&fx.paths)?;
    list.projects.remove("billing");
    list.save(&fx.paths)?;
    let detail = api.detail(local, &[])?;
    assert_eq!(
        detail.soft_links[0].state,
        lenka_tasks::LinkState::Unavailable
    );
    assert_eq!(detail.blockers.len(), 1);
    assert!(
        detail
            .warnings
            .iter()
            .any(|text| text.contains("unavailable"))
    );
    Ok(())
}

#[test]
fn project_isolation_for_an_agent() -> Result<()> {
    let fx = fixture()?;
    fx.project("api")?;
    fx.project("secret")?;
    let agent = Caller {
        scope: ProjectScope::Listed(vec!["api".to_owned()]),
        default_project: Some("api".to_owned()),
        ..fx.agent("a")
    };
    assert_eq!(fx.host.project_for(&agent, None)?.name(), "api");
    let Err(error) = fx.host.project_for(&agent, Some("secret")) else {
        panic!("the project must not be available");
    };
    assert_eq!(error.code(), ErrorCode::ProjectNotAvailable);
    assert!(error.message().contains("api") && !error.message().contains("secret,"));
    let names: Vec<String> = fx.host.available_projects(&agent)?.into_keys().collect();
    assert_eq!(names, vec!["api"]);
    Ok(())
}

#[test]
fn short_ids_and_ambiguity() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let id = create(&service, &agent, "T")?;
    assert_eq!(service.resolve(&id.short())?, id);
    assert_eq!(service.resolve(&id.to_string())?, id);
    assert_eq!(
        code(service.resolve("../../etc")),
        ErrorCode::InvalidArgument
    );
    assert_eq!(code(service.resolve("ffffffff")), ErrorCode::NotFound);
    Ok(())
}

#[test]
fn text_search_covers_tasks_plans_and_decisions() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let by_plan = service
        .create(
            &agent,
            NewTask {
                title: "a".to_owned(),
                plan: Some("Use the walrus index".to_owned()),
                ..NewTask::default()
            },
        )?
        .id();
    let by_entry = create(&service, &agent, "b")?;
    service.add_entry(
        &agent,
        by_entry,
        NewEntry {
            kind: EntryKind::Decision,
            text: "We pick WALRUS.".to_owned(),
            replaces: None,
            secret_policy: lenka_core::SecretPolicy::Enforce,
        },
    )?;
    let comment = create(&service, &agent, "c")?;
    service.add_entry(
        &agent,
        comment,
        NewEntry {
            kind: EntryKind::Comment,
            text: "walrus".to_owned(),
            replaces: None,
            secret_policy: lenka_core::SecretPolicy::Enforce,
        },
    )?;
    create(&service, &agent, "walrus title")?;
    create(&service, &agent, "none")?;
    let filter = ListFilter {
        text: Some("walrus".to_owned()),
        ..ListFilter::default()
    };
    let titles: Vec<String> = service
        .list(&agent, &filter)?
        .into_iter()
        .map(|row| row.title)
        .collect();
    assert_eq!(titles, vec!["a", "b", "walrus title"]);
    assert_eq!(by_plan.short().len(), 8);
    Ok(())
}

#[test]
fn related_finds_blockers_through_other_tasks_and_similar_titles() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let root = create(&service, &agent, "database schema")?;
    let middle = service
        .create(
            &agent,
            NewTask {
                title: "middle".to_owned(),
                blocked_by: vec![Link::Local(root)],
                ..NewTask::default()
            },
        )?
        .id();
    let top = service
        .create(
            &agent,
            NewTask {
                title: "top".to_owned(),
                blocked_by: vec![Link::Local(middle)],
                ..NewTask::default()
            },
        )?
        .id();
    create(&service, &agent, "database migration tool")?;
    let view = service.related(top)?;
    let names: Vec<&str> = view.blockers.iter().map(|row| row.title.as_str()).collect();
    assert!(
        names.contains(&"middle") && names.contains(&"database schema"),
        "{names:?}"
    );
    let view = service.related(root)?;
    assert_eq!(view.blocks.len(), 1);
    assert_eq!(view.similar[0].title, "database migration tool");
    Ok(())
}

#[test]
fn a_task_in_a_missing_stage_is_invalid() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    feature_stages(&service)?;
    let agent = fx.agent("a");
    let id = service
        .create(
            &agent,
            NewTask {
                title: "F".to_owned(),
                kind: "feature".to_owned(),
                ..NewTask::default()
            },
        )?
        .id();
    let mut config = (*service.project().config()).clone();
    config.types.remove("feature");
    service.project().save_config(config)?;
    assert!(ready_ids(&service, &agent)?.is_empty());
    assert_eq!(
        code(service.claim(&agent, Some(id), None, None)),
        ErrorCode::InvalidFile
    );
    assert_eq!(service.doctor()?.missing_stages.len(), 1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn hostile_files_give_clear_errors() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let id = create(&service, &agent, "T")?;
    let tasks = fx.data_dir("api").join("tasks");
    let io = |err: std::io::Error| LenkaError::io("fixture", &err);

    // A directory name that is not an ID.
    fs::create_dir_all(tasks.join("not-an-id")).map_err(io)?;
    // A symlink in place of a current file of another task.
    let other = create(&service, &agent, "O")?;
    let other_file = tasks.join(other.to_string()).join("task.md");
    fs::remove_file(&other_file).map_err(io)?;
    std::os::unix::fs::symlink(fx.root.join("config/projects.toml"), &other_file).map_err(io)?;
    // Another task ID inside a task file.
    let third = create(&service, &agent, "X")?;
    let third_file = tasks.join(third.to_string()).join("task.md");
    let text = fs::read_to_string(&third_file).map_err(io)?;
    fs::write(
        &third_file,
        text.replace(&third.to_string(), &id.to_string()),
    )
    .map_err(io)?;
    // A very large file.
    let large = create(&service, &agent, "L")?;
    let large_file = tasks.join(large.to_string()).join("plan.md");
    fs::write(&large_file, "x".repeat(2 * 1024 * 1024)).map_err(io)?;

    let host = build_host(&fx.paths, &fx.clock)?;
    let fresh = service_of(&host.open_by_name("api")?)?;
    let report = fresh.doctor()?;
    assert_eq!(report.bad_names, vec!["not-an-id"]);
    assert_eq!(report.invalid_files.len(), 3, "{:?}", report.invalid_files);
    assert!(
        report
            .invalid_files
            .iter()
            .any(|line| line.contains("symlink"))
    );
    assert!(
        report
            .invalid_files
            .iter()
            .any(|line| line.contains("another task"))
    );
    assert!(
        report
            .invalid_files
            .iter()
            .any(|line| line.contains("limit"))
    );
    assert!(
        fresh.show(&agent, id, &[]).is_ok(),
        "a good task stays usable"
    );
    Ok(())
}

#[test]
fn a_cycle_from_a_merge_is_found_at_load() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let a = create(&service, &agent, "a")?;
    let b = service
        .create(
            &agent,
            NewTask {
                title: "b".to_owned(),
                blocked_by: vec![Link::Local(a)],
                ..NewTask::default()
            },
        )?
        .id();
    let file = fx
        .data_dir("api")
        .join("tasks")
        .join(a.to_string())
        .join("task.md");
    let text = fs::read_to_string(&file).map_err(|err| LenkaError::io("read", &err))?;
    let edited = text.replace("blocked_by = []", &format!("blocked_by = [\"{b}\"]"));
    assert_ne!(text, edited);
    fs::write(&file, edited).map_err(|err| LenkaError::io("write", &err))?;
    let host = build_host(&fx.paths, &fx.clock)?;
    let fresh = service_of(&host.open_by_name("api")?)?;
    assert_eq!(fresh.doctor()?.cycles.len(), 1);
    assert!(
        ready_ids(&fresh, &agent)?.is_empty(),
        "tasks in a cycle are not ready"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, ..ProptestConfig::default() })]

    /// No sequence of link requests gets a cycle in, and the order stays the same for the same data.
    #[test]
    fn no_cycle_gets_in(edges in proptest::collection::vec((0_usize..8, 0_usize..8, any::<bool>()), 1..30)) {
        let fx = fixture().map_err(|err| TestCaseError::fail(err.to_string()))?;
        let service = fx.project("api").map_err(|err| TestCaseError::fail(err.to_string()))?;
        let agent = fx.agent("a");
        let ids: Vec<TaskId> = (0..8)
            .map(|index| create(&service, &agent, &format!("t{index}")))
            .collect::<Result<_>>()
            .map_err(|err| TestCaseError::fail(err.to_string()))?;
        for (from, to, is_parent) in edges {
            let version = service.detail(ids[from], &[]).map_err(|err| TestCaseError::fail(err.to_string()))?.meta.state.version;
            let patch = if is_parent {
                TaskPatch { expected_version: version, parent: Some(Some(ids[to])), ..TaskPatch::default() }
            } else {
                TaskPatch { expected_version: version, blocked_by_add: vec![Link::Local(ids[to])], ..TaskPatch::default() }
            };
            let _ = service.update(&agent, ids[from], patch);
        }
        let report = service.doctor().map_err(|err| TestCaseError::fail(err.to_string()))?;
        prop_assert!(report.cycles.is_empty(), "{:?}", report.cycles);
        let first = ready_ids(&service, &agent).map_err(|err| TestCaseError::fail(err.to_string()))?;
        let second = ready_ids(&service, &agent).map_err(|err| TestCaseError::fail(err.to_string()))?;
        prop_assert_eq!(first, second);
    }
}
