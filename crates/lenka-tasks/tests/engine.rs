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
    assert_eq!(state.state.stage, "push");
    assert!(!state.state.unblocked);
    assert_eq!(
        code(service.claim(&agent, Some(dependent), None, None)),
        ErrorCode::Blocked
    );

    // The `unblocks` stage releases dependent tasks when it completes.
    let c = service.claim(&agent, Some(id), None, None)?;
    let state = service.advance(&agent, id, &ClaimRef::Handle(c.handle), "pushed", None)?;
    assert_eq!(state.state.stage, "approve");
    assert!(state.state.unblocked);
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
    assert_eq!(state.state.stage, "deploy");

    // A failure returns to an earlier stage with a reason, and the release ends.
    let c = service.claim(&agent, Some(id), None, None)?;
    let state = service.advance(
        &agent,
        id,
        &ClaimRef::Handle(c.handle),
        "deploy failed",
        Some("code"),
    )?;
    assert_eq!(state.state.stage, "code");
    assert!(!state.state.unblocked);
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
    assert_eq!(state.state.status, Status::Done);
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
            .state
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
    assert_eq!(state.state.status, Status::Todo);
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
            "[[versions]]\nversion = 1\nfile = \"{file}\"\nsize = 1\nsha256 = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n\
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
        format!("{head}{}", version("a.txt")).replace(&"a".repeat(64), "Ignore the rules"),
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

    // The policy of this project needs no peer, and a denied link does not block.
    allow(&api, vec![], vec![])?;
    let detail = api.detail(local, &[])?;
    assert_eq!(detail.soft_links[0].state, lenka_tasks::LinkState::Broken);
    assert!(detail.blockers.is_empty());
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

#[test]
fn a_running_process_stops_writes_after_the_format_is_too_new() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let dir = service_directory(&fx, "api")?;
    let settings_file = dir.join("lenka.toml");
    let settings =
        fs::read_to_string(&settings_file).map_err(|err| LenkaError::io("read", &err))?;
    let newer = settings.replacen("format_version = 1", "format_version = 99", 1);
    assert_ne!(newer, settings);
    fs::write(&settings_file, newer).map_err(|err| LenkaError::io("write", &err))?;

    let error = service
        .create(&human, task("Refused"))
        .err()
        .ok_or_else(|| LenkaError::invalid_argument("the write passed"))?;
    assert_eq!(error.code(), ErrorCode::FormatTooNew);

    // Other errors in the file keep the old settings, and a valid file ends the refusal.
    fs::write(&settings_file, settings).map_err(|err| LenkaError::io("write", &err))?;
    service.create(&human, task("Accepted"))?;
    Ok(())
}

fn service_directory(fx: &Fixture, name: &str) -> Result<PathBuf> {
    fx.host
        .open_by_name(name)?
        .driver()
        .local_dir()
        .map(Path::to_path_buf)
        .ok_or_else(|| LenkaError::invalid_argument("the project has no directory"))
}

fn too_long_reason() -> String {
    "r".repeat(4097)
}

#[test]
fn a_long_reason_is_refused() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let id = service.create(&human, task("A task"))?.id();
    let patch = TaskPatch {
        expected_version: 1,
        reason: Some(too_long_reason()),
        description: Some("New text.".to_owned()),
        ..TaskPatch::default()
    };
    let error = service.update(&human, id, patch).err();
    assert_eq!(
        error.map(|err| err.code()),
        Some(ErrorCode::InvalidArgument)
    );
    let finish = Finish::Done {
        evidence: "It works.".to_owned(),
        unchecked_reason: Some(too_long_reason()),
    };
    let error = service
        .finish(&human, id, &ClaimRef::Implicit, &finish)
        .err();
    assert_eq!(
        error.map(|err| err.code()),
        Some(ErrorCode::InvalidArgument)
    );
    Ok(())
}

#[test]
fn large_evidence_is_refused() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let id = service.create(&human, task("A task"))?.id();
    let evidence = "e".repeat(1024 * 1024 + 1);
    let finish = Finish::Done {
        evidence: evidence.clone(),
        unchecked_reason: None,
    };
    let error = service
        .finish(&human, id, &ClaimRef::Implicit, &finish)
        .err();
    assert_eq!(
        error.map(|err| err.code()),
        Some(ErrorCode::InvalidArgument)
    );
    let error = service
        .advance(&human, id, &ClaimRef::Implicit, &evidence, None)
        .err()
        .map(|err| err.to_string())
        .unwrap_or_default();
    assert!(error.contains("The evidence is larger"), "{error}");
    Ok(())
}

#[test]
fn a_bad_label_is_refused() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let many: Vec<String> = (0..1001).map(|index| format!("label{index}")).collect();
    for labels in [
        vec!["a\nb".to_owned()],
        vec!["a\u{1b}[2Jb".to_owned()],
        vec!["l".repeat(257)],
        many,
    ] {
        let new = NewTask {
            labels,
            ..task("A task")
        };
        let error = service.create(&human, new).err();
        assert_eq!(
            error.map(|err| err.code()),
            Some(ErrorCode::InvalidArgument)
        );
    }
    let id = service.create(&human, task("A task"))?.id();
    let patch = TaskPatch {
        expected_version: 1,
        labels_add: vec!["a\nb".to_owned()],
        ..TaskPatch::default()
    };
    let error = service.update(&human, id, patch).err();
    assert_eq!(
        error.map(|err| err.code()),
        Some(ErrorCode::InvalidArgument)
    );
    let patch = TaskPatch {
        expected_version: 1,
        relates_add: vec![Link::Local(id); 1001],
        ..TaskPatch::default()
    };
    let error = service.update(&human, id, patch).err();
    assert_eq!(
        error.map(|err| err.code()),
        Some(ErrorCode::InvalidArgument)
    );
    Ok(())
}

#[test]
fn control_characters_are_refused_in_text() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let bad = "text\u{1b}]0;title\u{7}";
    let error = service.create(&human, task("a\u{202e}b")).err();
    assert_eq!(
        error.map(|err| err.code()),
        Some(ErrorCode::InvalidArgument)
    );
    let error = service.create(&human, task(bad)).err();
    assert_eq!(
        error.map(|err| err.code()),
        Some(ErrorCode::InvalidArgument)
    );
    let new = NewTask {
        description: bad.to_owned(),
        ..task("A task")
    };
    let error = service.create(&human, new).err();
    assert_eq!(
        error.map(|err| err.code()),
        Some(ErrorCode::InvalidArgument)
    );
    let new = NewTask {
        plan: Some(bad.to_owned()),
        ..task("A task")
    };
    let error = service.create(&human, new).err();
    assert_eq!(
        error.map(|err| err.code()),
        Some(ErrorCode::InvalidArgument)
    );

    // Line breaks and tabs stay valid in a body.
    let new = NewTask {
        description: "one\r\ntwo\n\tthree".to_owned(),
        ..task("A task")
    };
    let id = service.create(&human, new)?.id();
    let entry = NewEntry {
        kind: EntryKind::Comment,
        text: bad.to_owned(),
        replaces: None,
        secret_policy: lenka_core::SecretPolicy::Enforce,
    };
    let error = service.add_entry(&human, id, entry).err();
    assert_eq!(
        error.map(|err| err.code()),
        Some(ErrorCode::InvalidArgument)
    );
    let error = service
        .advance(&human, id, &ClaimRef::Implicit, bad, None)
        .err()
        .map(|err| err.to_string())
        .unwrap_or_default();
    assert!(error.contains("The evidence has a control"), "{error}");
    Ok(())
}

#[test]
fn a_marker_line_with_a_carriage_return_is_refused() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let new = NewTask {
        description: "one\n=======\r".to_owned(),
        ..task("A task")
    };
    let error = service.create(&fx.human(), new).err();
    assert_eq!(
        error.map(|err| err.code()),
        Some(ErrorCode::InvalidArgument)
    );
    Ok(())
}

/// Writes an inbox file with notices of a task, directly on the disk.
fn write_inbox(
    fx: &Fixture,
    caller: &Caller,
    subject: TaskId,
    count: usize,
    reason: &str,
) -> Result<()> {
    let notice = lenka_core::Notice {
        id: lenka_plugin_api::NoticeId::generate(),
        subject: lenka_core::Subject::Task(subject.to_string()),
        version: 1,
        changed: vec!["description".to_owned()],
        author: "other".to_owned(),
        reason: reason.to_owned(),
        at: lenka_plugin_api::Timestamp::from_unix_ms(1_793_000_000_000),
        claim: "handle".to_owned(),
        claimed_task: subject.to_string(),
    };
    let mut text = String::new();
    for _ in 0..count {
        let fresh = lenka_core::Notice {
            id: lenka_plugin_api::NoticeId::generate(),
            ..notice.clone()
        };
        text.push_str(
            &serde_json::to_string(&fresh)
                .map_err(|err| LenkaError::invalid_file(err.to_string()))?,
        );
        text.push('\n');
    }
    let dir = service_directory(fx, "api")?;
    let path = lenka_core::notices::inbox_path(&caller.agent_key)?;
    let file = dir.join(path.as_str());
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).map_err(|err| LenkaError::io("mkdir", &err))?;
    }
    fs::write(file, text).map_err(|err| LenkaError::io("write", &err))
}

#[test]
fn a_reply_holds_a_limited_count_of_notes() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let agent = fx.agent("victim");
    let id = service.create(&human, task("A task"))?.id();

    write_inbox(&fx, &agent, id, 60, "A small reason.")?;
    let reply = lenka_tasks::with_notes(serde_json::json!({}), &service, &agent);
    assert_eq!(reply["unread_changes"].as_array().map(Vec::len), Some(50));
    assert_eq!(reply["unread_changes_omitted"], 10);

    // The bytes have a limit too, and one large old notice cannot fill a reply.
    write_inbox(&fx, &agent, id, 40, &"\"".repeat(1024 * 1024 / 8))?;
    let reply = lenka_tasks::with_notes(serde_json::json!({}), &service, &agent);
    let size = reply.to_string().len();
    assert!(size < 256 * 1024, "{size}");
    assert!(
        reply["unread_changes_omitted"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    Ok(())
}

#[test]
fn a_large_inbox_does_not_stop_reads_or_a_finish() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let agent = fx.agent("victim");
    let id = service.create(&human, task("A task"))?.id();
    write_inbox(&fx, &agent, id, 18, &"x".repeat(1024 * 1024))?;
    let notes = service.call_notes(&agent)?;
    assert!(!notes.unread.is_empty());
    let claim = service.claim(&agent, Some(id), None, None)?;
    service.show(&agent, id, &[])?;
    let finish = Finish::Done {
        evidence: "It works.".to_owned(),
        unchecked_reason: None,
    };
    service.finish(&agent, id, &ClaimRef::Handle(claim.handle), &finish)?;
    Ok(())
}

#[test]
fn a_check_list_has_a_limit_and_runs_in_one_pass() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let count = 5000;
    let acceptance: String = (0..count)
        .map(|index| format!("- [ ] Item {index}\n"))
        .collect();
    let new = NewTask {
        acceptance,
        ..task("A task")
    };
    let id = service.create(&human, new)?.id();

    let too_many = TaskPatch {
        expected_version: 1,
        check: vec![
            lenka_tasks::CheckItem {
                index: 0,
                done: true
            };
            10_001
        ],
        ..TaskPatch::default()
    };
    let error = service.update(&human, id, too_many).err();
    assert_eq!(
        error.map(|err| err.code()),
        Some(ErrorCode::InvalidArgument)
    );

    let mut check: Vec<lenka_tasks::CheckItem> = (0..count)
        .map(|index| lenka_tasks::CheckItem { index, done: true })
        .collect();
    // The last mark of a position wins.
    check.push(lenka_tasks::CheckItem {
        index: 0,
        done: false,
    });
    let patch = TaskPatch {
        expected_version: 1,
        check,
        ..TaskPatch::default()
    };
    let started = std::time::Instant::now();
    service.update(&human, id, patch)?;
    let seconds = started.elapsed().as_secs();
    assert!(seconds < 5, "{seconds} s");
    let body = service.detail(id, &[])?.acceptance;
    assert!(body.contains("- [ ] Item 0\n"), "the last mark did not win");
    assert!(body.contains("- [x] Item 1\n"));
    assert!(body.contains(&format!("- [x] Item {}", count - 1)));
    Ok(())
}

#[test]
fn a_very_long_feed_line_does_not_stop_the_index() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let first = service.create(&human, task("First"))?.id();
    let dir = service_directory(&fx, "api")?;
    let feed = dir.join(".runtime/feed.jsonl");
    let mut text = fs::read(&feed).map_err(|err| LenkaError::io("read", &err))?;
    text.extend(std::iter::repeat_n(b'x', 65 * 1024 * 1024));
    fs::write(&feed, text).map_err(|err| LenkaError::io("write", &err))?;
    // This list moves past the long line. Then a new event must reach the index again.
    let _rows = service.list(&human, &lenka_tasks::ListFilter::default())?;
    let other = build_host(&fx.paths, &fx.clock)?;
    let other = service_of(&other.open_by_name("api")?)?;
    let second = other.create(&human, task("Second"))?.id();
    let rows = service.list(&human, &lenka_tasks::ListFilter::default())?;
    let titles = format!("{rows:?}");
    assert!(titles.contains("Second"), "{first} {second}: {titles}");
    Ok(())
}

fn task_file(fx: &Fixture, id: TaskId, name: &str) -> PathBuf {
    fx.data_dir("api")
        .join("tasks")
        .join(id.to_string())
        .join(name)
}

fn hand_edit(fx: &Fixture, id: TaskId, name: &str, from: &str, to: &str) -> Result<()> {
    let file = task_file(fx, id, name);
    let text = fs::read_to_string(&file).map_err(|err| LenkaError::io("read", &err))?;
    assert!(text.contains(from), "{text}");
    // A file system can have a coarse clock, and a person is never this fast.
    std::thread::sleep(std::time::Duration::from_millis(50));
    fs::write(&file, text.replace(from, to)).map_err(|err| LenkaError::io("write", &err))
}

fn described(service: &TaskService, who: &Caller, title: &str) -> Result<TaskId> {
    let meta = service.create(
        who,
        NewTask {
            description: "Text.".to_owned(),
            ..task(title)
        },
    )?;
    Ok(meta.id())
}

fn finish_done() -> Finish {
    Finish::Done {
        evidence: "It works.".to_owned(),
        unchecked_reason: None,
    }
}

#[test]
fn a_new_claim_does_not_drop_an_unread_notice() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let human = fx.human();
    let id = create(&service, &human, "T")?;
    service.claim(&agent, Some(id), None, Some(1))?;
    service.update(
        &human,
        id,
        TaskPatch {
            expected_version: 1,
            description: Some("Do not ship this.".to_owned()),
            reason: Some("The requirement changed.".to_owned()),
            claim: ClaimRef::Override,
            ..TaskPatch::default()
        },
    )?;
    // The claim ends by timeout, and the agent claims again.
    fx.clock.advance_ms(2 * 60 * 1000);
    let claim = service.claim(&agent, Some(id), None, None)?;
    assert_eq!(
        code(service.finish(
            &agent,
            id,
            &ClaimRef::Handle(claim.handle.clone()),
            &finish_done()
        )),
        ErrorCode::UnreadChanges
    );
    service.show(&agent, id, &[])?;
    service.finish(&agent, id, &ClaimRef::Handle(claim.handle), &finish_done())?;
    Ok(())
}

#[test]
fn each_state_write_records_a_hand_edit_first() -> Result<()> {
    type Write<'a> = Box<dyn Fn(&TaskService, &Caller, TaskId) -> Result<()> + 'a>;
    let writes: Vec<(&str, Write<'_>)> = vec![
        (
            "claim",
            Box::new(|service, who, id| service.claim(who, Some(id), None, None).map(|_| ())),
        ),
        (
            "update",
            Box::new(|service, who, id| {
                let detail = TaskPatch {
                    expected_version: 1,
                    title: Some("New".to_owned()),
                    ..TaskPatch::default()
                };
                service.update(who, id, detail).map(|_| ())
            }),
        ),
        (
            "advance",
            Box::new(|service, who, id| {
                service
                    .advance(who, id, &ClaimRef::Implicit, "It works.", None)
                    .map(|_| ())
            }),
        ),
        (
            "finish",
            Box::new(|service, who, id| {
                service
                    .finish(who, id, &ClaimRef::Implicit, &finish_done())
                    .map(|_| ())
            }),
        ),
        (
            "pause",
            Box::new(|service, who, id| service.pause(who, id, &ClaimRef::Implicit).map(|_| ())),
        ),
        (
            "status",
            Box::new(|service, who, id| {
                service
                    .set_status(who, id, Status::Cancelled, &ClaimRef::Implicit)
                    .map(|_| ())
            }),
        ),
    ];
    for (name, write) in writes {
        let fx = fixture()?;
        let service = fx.project("api")?;
        let human = fx.human();
        let id = described(&service, &human, "T")?;
        if name == "advance" {
            feature_stages(&service)?;
            service.update(
                &human,
                id,
                TaskPatch {
                    expected_version: 1,
                    kind: Some("feature".to_owned()),
                    reason: Some("A new type.".to_owned()),
                    ..TaskPatch::default()
                },
            )?;
        }
        let version = service.detail(id, &[])?.meta.state.version;
        hand_edit(&fx, id, "task.md", "Text.", "Edited by hand.")?;
        let _ = write(&service, &human, id);
        let root = fx.data_dir("api").join("tasks").join(id.to_string());
        let meta = root.join(format!("versions/{:06}/meta.toml", version + 1));
        let recorded = fs::read_to_string(&meta).unwrap_or_default();
        assert!(recorded.contains("external"), "{name}: {recorded}");
    }
    Ok(())
}

#[test]
fn a_hand_edit_reaches_the_holder_before_a_finish() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let id = described(&service, &agent, "T")?;
    let claim = service.claim(&agent, Some(id), None, None)?;
    hand_edit(&fx, id, "task.md", "Text.", "Do not ship this.")?;
    assert_eq!(
        code(service.finish(&agent, id, &ClaimRef::Handle(claim.handle), &finish_done())),
        ErrorCode::UnreadChanges
    );
    Ok(())
}

#[test]
fn a_state_write_reads_a_hand_edit_of_the_state_file() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let id = create(&service, &agent, "T")?;
    service.list(&agent, &lenka_tasks::ListFilter::default())?;
    hand_edit(
        &fx,
        id,
        "state.toml",
        "status = \"todo\"",
        "status = \"cancelled\"",
    )?;
    assert!(service.claim(&agent, Some(id), None, None).is_err());
    let state = fs::read_to_string(task_file(&fx, id, "state.toml"))
        .map_err(|err| LenkaError::io("read", &err))?;
    assert!(state.contains("cancelled"), "{state}");
    Ok(())
}

#[test]
fn a_lower_version_in_the_state_file_keeps_the_history() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let id = described(&service, &agent, "T")?;
    for version in 1..=3 {
        service.update(
            &agent,
            id,
            TaskPatch {
                expected_version: version,
                description: Some(format!("Revision {version}.")),
                reason: Some("A change.".to_owned()),
                ..TaskPatch::default()
            },
        )?;
    }
    let old = task_file(&fx, id, "versions/000002/task.md");
    let before = fs::read_to_string(&old).map_err(|err| LenkaError::io("read", &err))?;
    // A person restores an old state file from a backup.
    hand_edit(&fx, id, "state.toml", "version = 4", "version = 1")?;

    let patch = TaskPatch {
        expected_version: 1,
        description: Some("Late.".to_owned()),
        reason: Some("A change.".to_owned()),
        ..TaskPatch::default()
    };
    assert_eq!(
        code(service.update(&agent, id, patch)),
        ErrorCode::StaleVersion
    );
    assert_eq!(service.detail(id, &[])?.meta.state.version, 4);
    assert_eq!(
        fs::read_to_string(&old).map_err(|err| LenkaError::io("read", &err))?,
        before
    );
    Ok(())
}

#[test]
fn a_lost_feed_does_not_hide_a_change_from_a_running_process() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let lost = create(&service, &agent, "Lost")?;
    let other_task = create(&service, &agent, "Other")?;
    let feed = fx.data_dir("api").join(".runtime/feed.jsonl");
    fs::remove_file(&feed).map_err(|err| LenkaError::io("remove", &err))?;
    let other_host = build_host(&fx.paths, &fx.clock)?;
    let other = service_of(&other_host.open_by_name("api")?)?;
    let change = |id: TaskId, version: u64| {
        service.update(
            &agent,
            id,
            TaskPatch {
                expected_version: version,
                title: Some(format!("Title {version}")),
                reason: Some("A change.".to_owned()),
                ..TaskPatch::default()
            },
        )
    };
    change(other_task, 1)?;
    change(other_task, 2)?;
    other.refresh()?;

    fs::remove_file(&feed).map_err(|err| LenkaError::io("remove", &err))?;
    // The new feed gets past the offset of the other process, at a line end.
    change(lost, 1)?;
    change(other_task, 3)?;
    change(other_task, 4)?;

    other.refresh()?;
    let rows = other.list(&agent, &lenka_tasks::ListFilter::default())?;
    let row = rows.iter().find(|row| row.id == lost.short());
    assert_eq!(row.map(|row| row.title.as_str()), Some("Title 1"));
    Ok(())
}

#[test]
fn a_refused_person_learns_about_the_override() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let human = fx.human();
    let id = create(&service, &human, "T")?;
    service.claim(&agent, Some(id), None, None)?;
    let patch = |reason: &str| TaskPatch {
        expected_version: 1,
        description: Some("New.".to_owned()),
        reason: Some(reason.to_owned()),
        ..TaskPatch::default()
    };
    let error = service.update(&human, id, patch("A change.")).err();
    let message = error.map(|err| err.to_string()).unwrap_or_default();
    assert!(message.contains("--override"), "{message}");
    let other = fx.agent("b");
    let error = service.update(&other, id, patch("A change.")).err();
    let message = error.map(|err| err.to_string()).unwrap_or_default();
    assert!(message.contains("task.entry.add"), "{message}");
    Ok(())
}

fn text_change(version: u64) -> TaskPatch {
    TaskPatch {
        expected_version: version,
        description: Some(format!("Text {version}")),
        reason: Some("A change.".to_owned()),
        ..TaskPatch::default()
    }
}

/// Makes agent changes until the loop limit stops the task.
fn reach_loop_limit(service: &TaskService, agent: &Caller, id: TaskId) -> Result<()> {
    for version in 1..=5 {
        service.update(agent, id, text_change(version))?;
    }
    assert_eq!(
        code(service.update(agent, id, text_change(6))),
        ErrorCode::LoopLimit
    );
    Ok(())
}

#[test]
fn an_agent_cannot_delete_or_write_entries_on_a_stopped_task() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (agent, human) = (fx.agent("a"), fx.human());
    let id = create(&service, &agent, "T")?;
    reach_loop_limit(&service, &agent, id)?;
    let entry = || NewEntry {
        kind: EntryKind::Comment,
        text: "A note".to_owned(),
        replaces: None,
        secret_policy: lenka_core::SecretPolicy::Enforce,
    };
    assert_eq!(code(service.delete(&agent, id)), ErrorCode::LoopLimit);
    assert_eq!(
        code(service.add_entry(&agent, id, entry())),
        ErrorCode::LoopLimit
    );
    assert!(service.add_entry(&human, id, entry()).is_ok());
    assert!(service.delete(&human, id).is_ok());
    Ok(())
}

#[test]
fn a_closed_task_refuses_an_entry() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let id = create(&service, &human, "T")?;
    service.set_status(&human, id, Status::Cancelled, &ClaimRef::Implicit)?;
    let entry = NewEntry {
        kind: EntryKind::Comment,
        text: "A note".to_owned(),
        replaces: None,
        secret_policy: lenka_core::SecretPolicy::Enforce,
    };
    assert!(service.add_entry(&human, id, entry).is_err());
    Ok(())
}

/// A status change with an override ends the claim of the agent, so the agent gets a notice.
#[test]
fn an_override_of_the_status_gives_the_holder_a_notice() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (agent, human) = (fx.agent("a"), fx.human());
    let id = create(&service, &human, "T")?;
    service.claim(&agent, Some(id), None, None)?;
    service.set_status(&human, id, Status::Todo, &ClaimRef::Override)?;
    let key = agent.agent_key.clone();
    let notices = lenka_core::notices::read_inbox(service.project().driver(), &key)?;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].changed, vec!["status".to_owned()]);
    Ok(())
}

/// A lost feed also lost the claim events, so a running process reads the claim files again.
#[test]
fn a_reset_of_the_feed_reloads_the_claims() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (one, two) = (fx.agent("one"), fx.agent("two"));
    let id = create(&service, &fx.human(), "T")?;
    service.claim(&one, Some(id), None, None)?;
    let runtime = fx.data_dir("api").join(".runtime");
    fs::remove_file(runtime.join("claims").join(format!("{id}.toml")))
        .map_err(|err| LenkaError::io("remove", &err))?;
    fs::remove_file(runtime.join("feed.jsonl")).map_err(|err| LenkaError::io("remove", &err))?;
    assert!(service.claim(&two, Some(id), None, None).is_ok());
    Ok(())
}

/// A task with invalid files and no record is only in the invalid list. It must go with its files.
#[test]
fn a_removed_invalid_task_stops_blocking_in_a_running_process() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let blocker = create(&service, &human, "Blocker")?;
    let new = NewTask {
        blocked_by: vec![Link::Local(blocker)],
        ..task("Blocked")
    };
    let blocked = service.create(&human, new)?.id();
    fs::write(task_file(&fx, blocker, "task.md"), "not a task\n")
        .map_err(|err| LenkaError::io("write", &err))?;
    let versions = task_file(&fx, blocker, "versions");
    if versions.exists() {
        fs::remove_dir_all(versions).map_err(|err| LenkaError::io("remove", &err))?;
    }
    // A new process finds the invalid files in its first scan.
    let host = build_host(&fx.paths, &fx.clock)?;
    let running = service_of(&host.open_by_name("api")?)?;
    assert_eq!(running.detail(blocked, &[])?.blockers.len(), 1);
    assert_eq!(code(running.detail(blocker, &[])), ErrorCode::InvalidFile);

    let directory = task_file(&fx, blocker, "task.md");
    let directory = directory
        .parent()
        .ok_or_else(|| LenkaError::not_found("no directory"))?;
    fs::remove_dir_all(directory).map_err(|err| LenkaError::io("remove", &err))?;
    // A lost feed starts a full scan in the running process.
    let feed = fx.data_dir("api").join(".runtime/feed.jsonl");
    fs::remove_file(&feed).map_err(|err| LenkaError::io("remove", &err))?;
    assert_eq!(code(running.detail(blocker, &[])), ErrorCode::NotFound);
    Ok(())
}

#[test]
fn a_settings_save_cannot_change_the_id_or_write_a_future_format() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let project = service.project();
    let mut other_id = (*project.config()).clone();
    other_id.id = lenka_plugin_api::ProjectId::generate();
    assert_eq!(
        code(project.save_config(other_id.clone())),
        ErrorCode::InvalidArgument
    );
    let base = project.config();
    assert_eq!(
        code(project.save_config_over(&base, other_id)),
        ErrorCode::InvalidArgument
    );
    let mut future = (*project.config()).clone();
    future.format_version += 1;
    assert_eq!(code(project.save_config(future)), ErrorCode::FormatTooNew);
    Ok(())
}

/// The state of a task with invalid files is not known, so the task still blocks others.
#[test]
fn an_invalid_blocker_with_no_record_still_blocks() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (agent, human) = (fx.agent("a"), fx.human());
    let blocker = create(&service, &human, "Blocker")?;
    let new = NewTask {
        blocked_by: vec![Link::Local(blocker)],
        ..task("Blocked")
    };
    let blocked = service.create(&human, new)?.id();
    fs::write(task_file(&fx, blocker, "task.md"), "not a task\n")
        .map_err(|err| LenkaError::io("write", &err))?;
    let versions = task_file(&fx, blocker, "versions");
    if versions.exists() {
        fs::remove_dir_all(versions).map_err(|err| LenkaError::io("remove", &err))?;
    }
    // A new process with no snapshot has no earlier record of the task.
    fs::remove_dir_all(fx.data_dir("api").join(".runtime"))
        .map_err(|err| LenkaError::io("remove", &err))?;
    let other_host = build_host(&fx.paths, &fx.clock)?;
    let fresh = service_of(&other_host.open_by_name("api")?)?;
    let shown = fresh.detail(blocked, &[])?;
    assert_eq!(shown.blockers.len(), 1);
    assert_eq!(shown.blockers[0].id, blocker.short());
    assert_eq!(shown.blockers[0].title, None);
    assert_eq!(
        code(fresh.claim(&agent, Some(blocked), None, None)),
        ErrorCode::Blocked
    );
    assert!(ready_ids(&fresh, &agent)?.is_empty());
    Ok(())
}

#[test]
fn a_stage_completion_by_a_person_ends_the_loop_stop() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (agent, human) = (fx.agent("a"), fx.human());
    let mut config = (*service.project().config()).clone();
    if let Some(def) = config.types.get_mut("task") {
        let mut next = def.stages[0].clone();
        next.name = "review".to_owned();
        def.stages.push(next);
    }
    service.project().save_config(config)?;
    let id = create(&service, &agent, "T")?;
    reach_loop_limit(&service, &agent, id)?;
    service.advance(
        &human,
        id,
        &ClaimRef::Implicit,
        "A person checked it.",
        None,
    )?;
    assert!(!service.detail(id, &[])?.meta.state.loop_stopped);
    assert!(service.claim(&agent, Some(id), None, None).is_ok());
    Ok(())
}

#[test]
fn an_agent_cannot_claim_a_task_at_the_loop_limit_until_a_person_acts() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let agent = fx.agent("a");
    let id = create(&service, &agent, "T")?;
    reach_loop_limit(&service, &agent, id)?;

    assert_eq!(
        code(service.claim(&agent, Some(id), None, None)),
        ErrorCode::LoopLimit
    );
    assert_eq!(
        code(service.update(&agent, id, text_change(6))),
        ErrorCode::LoopLimit
    );
    // Time does not end the stop, and neither does a loss of the runtime data.
    fx.clock.advance_ms(3 * HOUR_MS);
    let loops = fx.data_dir("api").join(".runtime/loops");
    fs::remove_dir_all(&loops).map_err(|err| LenkaError::io("remove", &err))?;
    assert_eq!(
        code(service.claim(&agent, Some(id), None, None)),
        ErrorCode::LoopLimit
    );
    assert_eq!(
        code(service.update(&agent, id, text_change(6))),
        ErrorCode::LoopLimit
    );
    let state = fs::read_to_string(task_file(&fx, id, "state.toml"))
        .map_err(|err| LenkaError::io("read", &err))?;
    assert!(state.contains("loop_stopped = true"), "{state}");
    Ok(())
}

#[test]
fn a_status_change_by_a_person_ends_the_loop_stop() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (agent, human) = (fx.agent("a"), fx.human());
    let id = create(&service, &agent, "T")?;
    reach_loop_limit(&service, &agent, id)?;
    let state = service.set_status(&human, id, Status::Todo, &ClaimRef::Implicit)?;
    assert!(!state.loop_stopped);
    service.claim(&agent, Some(id), None, None)?;
    Ok(())
}

#[test]
fn a_claim_by_a_person_ends_the_loop_stop() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (agent, human) = (fx.agent("a"), fx.human());
    let id = create(&service, &agent, "T")?;
    reach_loop_limit(&service, &agent, id)?;
    service.claim(&human, Some(id), None, None)?;
    service.finish(&human, id, &ClaimRef::Implicit, &Finish::Released)?;
    service.claim(&agent, Some(id), None, None)?;
    // The person also started a new count.
    service.update(&agent, id, text_change(6))?;
    Ok(())
}

#[test]
fn a_person_can_let_an_agent_continue_the_loop_until_the_task_is_done() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (agent, human) = (fx.agent("a"), fx.human());
    let id = create(&service, &agent, "T")?;
    reach_loop_limit(&service, &agent, id)?;
    let mark = |value: bool, version: u64| TaskPatch {
        expected_version: version,
        loop_until_done: Some(value),
        ..TaskPatch::default()
    };
    // An agent cannot set this mark.
    assert_eq!(
        code(service.update(&agent, id, mark(true, 6))),
        ErrorCode::ActorDenied
    );

    let meta = service.update(&human, id, mark(true, 6))?;
    assert!(meta.state.loop_until_done);
    assert!(!meta.state.loop_stopped);
    assert_ne!(meta.state.status, Status::Paused, "the task is ready again");
    assert_eq!(ready_ids(&service, &agent)?.len(), 1);

    let claim = service.claim(&agent, Some(id), None, None)?;
    let handle = ClaimRef::Handle(claim.handle);
    let mut version = meta.state.version;
    for _ in 0..20 {
        let patch = TaskPatch {
            claim: handle.clone(),
            ..text_change(version)
        };
        version = service.update(&agent, id, patch)?.state.version;
    }
    let done = service.finish(&agent, id, &handle, &finish_done())?;
    assert!(!done.state.loop_until_done, "the mark ends with the task");

    // A reopened task has the limit again.
    service.set_status(&human, id, Status::Todo, &ClaimRef::Implicit)?;
    for _ in 0..5 {
        version = service
            .update(&agent, id, text_change(version))?
            .state
            .version;
    }
    assert_eq!(
        code(service.update(&agent, id, text_change(version))),
        ErrorCode::LoopLimit
    );
    Ok(())
}

#[test]
fn a_person_can_remove_the_loop_mark() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (agent, human) = (fx.agent("a"), fx.human());
    let id = create(&service, &agent, "T")?;
    let on = TaskPatch {
        expected_version: 1,
        loop_until_done: Some(true),
        ..TaskPatch::default()
    };
    let version = service.update(&human, id, on)?.state.version;
    let off = TaskPatch {
        expected_version: version,
        loop_until_done: Some(false),
        ..TaskPatch::default()
    };
    let meta = service.update(&human, id, off)?;
    assert!(!meta.state.loop_until_done);
    let mut version = meta.state.version;
    for _ in 0..5 {
        version = service
            .update(&agent, id, text_change(version))?
            .state
            .version;
    }
    assert_eq!(
        code(service.update(&agent, id, text_change(version))),
        ErrorCode::LoopLimit
    );
    Ok(())
}

#[test]
fn a_closed_task_changes_only_by_an_explicit_reopen() -> Result<()> {
    for closed in [Status::Done, Status::Cancelled] {
        let fx = fixture()?;
        let service = fx.project("api")?;
        let (agent, human) = (fx.agent("a"), fx.human());
        feature_stages(&service)?;
        let id = service
            .create(
                &human,
                NewTask {
                    kind: "feature".to_owned(),
                    ..task("T")
                },
            )?
            .id();
        if closed == Status::Done {
            service.finish(&human, id, &ClaimRef::Override, &finish_done())?;
        } else {
            service.set_status(&human, id, closed, &ClaimRef::Implicit)?;
        }
        let before = fs::read_dir(task_file(&fx, id, "entries")).map_or(0, Iterator::count);

        for who in [&agent, &human] {
            let patch = TaskPatch {
                expected_version: 1,
                title: Some("New".to_owned()),
                ..TaskPatch::default()
            };
            assert_eq!(
                code(service.update(who, id, patch)),
                ErrorCode::InvalidArgument,
                "update by {}",
                who.label
            );
        }
        assert_eq!(
            code(service.advance(&human, id, &ClaimRef::Override, "It works.", None)),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            code(service.finish(&human, id, &ClaimRef::Override, &finish_done())),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            code(service.finish(&human, id, &ClaimRef::Override, &Finish::Released)),
            ErrorCode::InvalidArgument
        );
        let detail = service.detail(id, &[])?;
        assert_eq!(detail.meta.state.status, closed);
        assert_eq!(detail.meta.state.version, 1);
        assert_eq!(
            fs::read_dir(task_file(&fx, id, "entries")).map_or(0, Iterator::count),
            before,
            "a refused finish adds no evidence"
        );

        let state = service.set_status(&human, id, Status::Todo, &ClaimRef::Implicit)?;
        assert_eq!(state.status, Status::Todo);
        assert!(!state.unblocked, "a reopened task blocks its dependents");
    }
    Ok(())
}

#[test]
fn a_closed_parent_has_no_open_children() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let parent = create(&service, &human, "Parent")?;
    let child = |parent: TaskId| NewTask {
        parent: Some(parent),
        ..task("Child")
    };
    let first = service.create(&human, child(parent))?.id();
    assert_eq!(
        code(service.set_status(&human, parent, Status::Cancelled, &ClaimRef::Implicit)),
        ErrorCode::Blocked
    );
    service.set_status(&human, first, Status::Cancelled, &ClaimRef::Implicit)?;
    service.finish(&human, parent, &ClaimRef::Override, &finish_done())?;

    assert_eq!(
        code(service.create(&human, child(parent))),
        ErrorCode::InvalidArgument
    );
    let free = create(&service, &human, "Free")?;
    let patch = TaskPatch {
        expected_version: 1,
        parent: Some(Some(parent)),
        ..TaskPatch::default()
    };
    assert_eq!(
        code(service.update(&human, free, patch)),
        ErrorCode::InvalidArgument
    );
    // A child cannot open again below a closed parent.
    assert_eq!(
        code(service.set_status(&human, first, Status::Todo, &ClaimRef::Implicit)),
        ErrorCode::InvalidArgument
    );
    Ok(())
}

#[test]
fn a_type_change_updates_the_state_and_the_claim_and_gives_a_notice() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (agent, human) = (fx.agent("a"), fx.human());
    feature_stages(&service)?;
    let id = create(&service, &human, "T")?;
    let claim = service.claim(&agent, Some(id), None, None)?;
    let patch = TaskPatch {
        expected_version: 1,
        kind: Some("feature".to_owned()),
        reason: Some("This is a larger task.".to_owned()),
        claim: ClaimRef::Override,
        ..TaskPatch::default()
    };
    let meta = service.update(&human, id, patch)?;
    let stage = meta.state.stage.clone();
    assert!(!meta.state.unblocked);

    let notes = service.call_notes(&agent)?;
    assert_eq!(notes.unread.len(), 1);
    assert_eq!(notes.unread[0].changed, vec!["type"]);
    let holder = fs::read_to_string(
        fx.data_dir("api")
            .join(".runtime/claims")
            .join(format!("{id}.toml")),
    )
    .map_err(|err| LenkaError::io("read", &err))?;
    assert!(holder.contains(&format!("stage = \"{stage}\"")), "{holder}");
    assert!(holder.contains(&claim.handle));
    Ok(())
}

#[test]
fn a_type_change_makes_a_task_block_its_dependents_again() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    feature_stages(&service)?;
    let new = NewTask {
        kind: "feature".to_owned(),
        ..task("T")
    };
    let id = service.create(&human, new)?.id();
    for _ in 0..2 {
        service.advance(&human, id, &ClaimRef::Implicit, "It works.", None)?;
    }
    assert!(service.detail(id, &[])?.meta.state.unblocked);
    let patch = TaskPatch {
        expected_version: 1,
        kind: Some("task".to_owned()),
        reason: Some("This is a small task.".to_owned()),
        ..TaskPatch::default()
    };
    let meta = service.update(&human, id, patch)?;
    assert!(!meta.state.unblocked, "{:?}", meta.state);
    Ok(())
}

#[test]
fn a_type_update_can_repair_a_stage_that_the_type_does_not_have() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let human = fx.human();
    let id = create(&service, &human, "T")?;
    let stage = service.detail(id, &[])?.meta.state.stage;
    hand_edit(
        &fx,
        id,
        "state.toml",
        &format!("stage = \"{stage}\""),
        "stage = \"gone\"",
    )?;
    let kind = service.detail(id, &[])?.meta.front.kind;
    let patch = TaskPatch {
        expected_version: 1,
        kind: Some(kind),
        ..TaskPatch::default()
    };
    let meta = service.update(&human, id, patch)?;
    assert_eq!(meta.state.stage, stage);
    Ok(())
}

#[test]
fn an_invalid_blocker_with_no_record_stops_a_claim() -> Result<()> {
    let fx = fixture()?;
    let service = fx.project("api")?;
    let (agent, human) = (fx.agent("a"), fx.human());
    let blocker = create(&service, &human, "Blocker")?;
    let id = service
        .create(
            &human,
            NewTask {
                blocked_by: vec![Link::Local(blocker)],
                ..task("T")
            },
        )?
        .id();
    // All files of the blocker are invalid, so no record remains.
    let root = fx.data_dir("api").join("tasks").join(blocker.to_string());
    fs::remove_dir_all(root.join("versions")).map_err(|err| LenkaError::io("remove", &err))?;
    fs::write(root.join("task.md"), "not a task").map_err(|err| LenkaError::io("write", &err))?;
    fs::write(root.join("state.toml"), "not a state")
        .map_err(|err| LenkaError::io("write", &err))?;
    service.reindex(&human)?;
    assert_eq!(
        code(service.claim(&agent, Some(id), None, None)),
        ErrorCode::Blocked
    );
    Ok(())
}
