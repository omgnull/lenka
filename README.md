# lenka

A project management tool for AI agents first, and for people second. Agents use lenka to keep tasks and plans, manage relations between tasks, and do the work in the correct order. People use the same data through a native desktop app and a CLI.

lenka is one Rust binary. Tasks are Markdown files that people can read in Git. Any MCP client can connect.

**Status:** version 1 for individuals and small teams on one machine. See [Known limits](#known-limits).

## Features

- **Tasks with plans.** A task holds a minimal description, requirements, and acceptance criteria. A plan is an optional part of a task. Entries record decisions, comments, proposals, and evidence.
- **Order.** `parent`, `blocked_by`, and `relates` relations, with cycle checks. `task.list` with `ready` gives the next work in a fixed order.
- **Stages.** A project admin sets the task types and the stages of each type. A stage can need an agent or a person.
- **Claims.** An atomic claim with a timeout. An agent can prolong its claim.
- **Shared versions and change notices.** All agents on a project see the same versioned tasks, in any worktree. A material change requires a reason, and it creates a notice for the agents that it affects. `task.finish` fails until the agent reads the change.
- **Artifacts.** Attachments have versions, and one artifact can link to many tasks.
- **Multiple projects.** A clone lists its projects. Soft links between projects need a policy on both sides.
- **Search and memory.** Text search over tasks, plans, and decisions, a search for related tasks and blockers, and a shared memory for agents.
- **Desktop app.** Board with filters, task tree, task editor, roadmap, Gantt chart, and a drawing board. Agent changes appear live.
- **Secret check.** lenka rejects common secret patterns in task text and text artifacts. Only a person can override this.
- **Git.** An optional plugin commits and pushes the data on the branch `lenka-data`.

## Install

Requires a [Rust toolchain](https://rustup.rs/).

```sh
cargo install --path crates/lenka
```

For a build with no GUI, use an explicit list of plugins:

```sh
cargo install --path crates/lenka --no-default-features --features tasks,artifacts,memory,git,mcp
```

## Quick start

Create a project in a Git repository:

```sh
lenka init
```

The data goes to `<git-common-dir>/lenka-data`, so all worktrees of the clone share it, and your code branches stay clean. Use `lenka init --shared` and `lenka project attach <name>` to share one project between repositories.

Connect an MCP client. For Claude Code:

```sh
claude mcp add lenka -- lenka mcp
```

Optional: show change notices to Claude Code between tool calls.

```sh
lenka hook install
```

Open the desktop app:

```sh
lenka desktop
```

Use the CLI:

```sh
lenka task create "Add task claims" --description "Let only one agent claim a task."
lenka task list --ready
lenka doctor
```

## Agent workflow

```text
task.create      Store the task
task.update      Add a plan, link a task or an artifact
task.create      Optional: add child tasks from the plan
task.claim       Take a specific task, or the next ready task
task.entry.add   Record a decision, a comment, or a proposal
task.advance     Finish a stage with evidence
task.finish      Finish with evidence, or release the task
```

Other tools: `project.list`, `task.show`, `task.list`, `task.related`, `task.pause`, `task.delete`, `artifact.create`, `artifact.update`, `artifact.delete`, `memory.add`, `memory.search`, and `memory.delete`.

An MCP call acts as the actor `agent`, and a CLI or desktop call acts as the actor `human`.

## Project data

```text
<data-dir>/
├── lenka.toml               # Project ID, format version, types, stages, links, no secrets
├── tasks/<task-id>/
│   ├── task.md              # TOML front matter and Markdown
│   ├── plan.md              # Optional
│   ├── state.toml           # Status, stage, version
│   ├── entries/<id>.md
│   └── versions/000012/     # meta.toml, task.md, plan.md
├── artifacts/<artifact-id>/
├── memory/<memory-id>.md
└── .runtime/                # Claims, inboxes, change feed, index. Git ignores this directory
```

You can edit a Markdown file directly. lenka records the edit as a new version with the author `external`.

### Types and stages

Edit `lenka.toml`, or use the project settings in the desktop app:

```toml
[types.feature]
stages = [
  { name = "code",    actor = "agent" },
  { name = "push",    actor = "agent", unblocks = true },
  { name = "approve", actor = "human" },
  { name = "deploy",  actor = "agent" },
]
```

### Links between projects

Both projects must allow a link. The default is no links.

```toml
[links]
link_to = ["<project-id>"]      # Projects that this project can link to
accept_from = ["<project-id>"]  # Projects that can link to this project. "*" means any
```

A link shows only the title, status, and stage of its target.

| State | Cause | `blocked_by` result |
|---|---|---|
| `ok` | lenka can read the target | Normal rules |
| `unavailable` | This machine does not have the project | Still blocks, with a warning |
| `broken` | The target is deleted, or a policy no longer allows the link | No longer blocks, with a warning |

### Drawings

A drawing is an artifact with the media type `application/vnd.lenka.drawing+json`. Its file is sorted, formatted JSON, so Git can compare it. An agent can read and write this file through `artifact.update`, and an open desktop app shows the new version.

## Plugins

All features are plugins. Cargo features select lenka's own plugins:

```sh
cargo build --release --no-default-features --features tasks,artifacts,mcp
```

A third-party build uses a small wrapper crate:

```rust
let host = lenka::default_builder().plugin(acme_sso::plugin()).build()?;
```

A compiled plugin runs in the lenka process with no sandbox, so `lenka plugins` labels it "fully trusted". It must have a GPL-compatible license, because it forms one program with lenka. A plugin that changes network or security behavior is off by default. `plugins.toml` in the user config directory turns plugins on and off.

## Security

lenka protects against mistakes and untrusted files. It does not protect against a malicious local process, because an agent with a shell can edit the files directly.

- Task text can be a prompt injection. Tool results put task text in fields that are labeled as untrusted data, with the author beside the text. This reduces the risk, but it does not remove it.
- The secret check finds common patterns only. Do not put secrets in tasks.
- A delete changes only the current files. Committed bytes stay in the Git history, so rotate an exposed secret.
- The only network use is the Git plugin through system Git, and only when you enable it.
- Data from a remote is untrusted. `lenka sync` and `lenka init` refuse a data branch that holds anything other than plain data files, such as a `.runtime` directory or a symlink. A manual `git pull` in a data directory does not have this check, so use `lenka sync`.

## Known limits

- No server, no live collaboration between machines, no verified identity, and no enforced roles. These come in a later release.
- No vault plugin. A task cannot attach a secret yet.
- Claims do not sync between machines.
- Network folders are not supported, because file locks are unreliable on them.
- The desktop app uses the fonts of your system, and it needs one such as Noto Sans. Characters that the font does not have, such as CJK or emoji, appear as boxes.
- After a power failure, a file system that does not keep the order of writes can lose the newest writes. Set `strict_flush = true` under `[limits]` in `lenka.toml` for a flush of each file, which is much slower.

## Develop

```sh
cargo build
cargo test
cargo run -p lenka -- --help
```

[`CONTRIBUTING.md`](CONTRIBUTING.md) has the contribution rules. `crates/lenka-tasks/examples/perf.rs` measures the engine against the performance limits.

## License

Copyright (C) 2026 Igor Pie and lenka contributors

Licensed under the [GNU General Public License v3.0 only](LICENSE).
