# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

lenka is an AI-first project management and collaboration tool, written in Rust (edition 2024, stable toolchain). Agents use it through MCP, and people use it through a CLI and a native desktop app.

## Architecture

All features are plugins in a Cargo workspace. The `lenka` crate builds the binary, and its Cargo features select the plugins.

| Crate | Purpose |
|---|---|
| `lenka-plugin-api` | IDs, errors, time, `Caller`, `ToolDef`, `StorageDriver`, `ArtifactStore`. No I/O |
| `lenka-core` | `Host`, `Plugin`, settings, project lookup, secret check, write journal, feed, inboxes |
| `lenka-driver-local` | Local directory storage driver |
| `lenka-artifact-fs` | Filesystem artifact store |
| `lenka-tasks` | Task model, Markdown, index, ready work, claims, notices, soft links, `TaskService`, task tools |
| `lenka-artifacts` | `ArtifactService`, artifact tools |
| `lenka-memory` | Shared agent memory |
| `lenka-git` | Commit and push data with system Git. Off by default |
| `lenka-mcp` | MCP over stdio with `rmcp` |
| `lenka-desktop` | Native app with `egui`: board, tree, editor, roadmap, Gantt chart, drawing board |
| `lenka` | CLI and default plugin list |

Rules that the design depends on:

- All writes go through `Project::write_locked` and `Locked::commit`, which run the secret check and the journal. Do not write to a data directory in another way.
- The version check runs in `Locked::check_version`. A write with an old version must fail with `stale_version`.
- Take the index mutex before the file lock. `TaskService::with_artifact_write` shows the order. The file lock also blocks in one process, so do not nest two write locks.
- The directory `.runtime/` holds only data that lenka can rebuild or lose with no harm to tasks.
- Task text is untrusted data. Tool results put it in fields named `untrusted_data`.

## Commands

```sh
cargo build
cargo run -p lenka -- --help
cargo test
cargo test <name>                  # tests whose name contains <name>
cargo run --release -p lenka-tasks --example perf -- gen <dir> 10000   # performance data
cargo run --release -p lenka-tasks --example perf -- run <dir>         # performance limits
```

CI runs these, and all of them must pass:

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo deny check                   # needs cargo-deny installed
```

## Additional rules

`docs/agents/` holds the shared rules for people and agents. Claude Code loads them through symlinks in `.claude/rules/`, so do not import them with `@`.

- `docs/agents/README.md`: the index, and the procedure to change or add a rule.
- `docs/agents/code.md`: general code and commit rules. Loads in each session.
- `docs/agents/rust.md`: Rust code and Cargo files. Loads when you use a matching file.
- `docs/agents/tooling.md`: CI, Dependabot, and cargo-deny. Loads when you use a matching file.

## Licensing

lenka is `GPL-3.0-only`. `CONTRIBUTING.md` has the required SPDX and copyright header for new source files. Do not edit the `LICENSE` file: it is the standard text, and its `<year>` and `<name of author>` placeholders must stay.

`deny.toml` allows only dependency licenses that are compatible with GPL-3.0-only. Before you add a license to its allow list, confirm that it is compatible.

## Task artifacts

Write all local plans and other task artifacts to a subfolder of `.agents/`, for example `.agents/plans/` or `.agents/specs/`. Do not write them directly in `.agents/` or in other folders of the repository. Git ignores `.agents/`, so do not commit these files.

## Writing style

Use a relaxed form of ASD-STE100 Simplified Technical English (Issue 9) in new and changed text. Apply it to documentation, code comments, rustdoc, commit messages, pull requests, issues, and user-facing messages. Do not apply it to code, commands, quoted text, or `LICENSE`.

Follow these STE rules:

- Write procedures in the imperative, with one instruction in each sentence and a maximum of 20 words.
- Write descriptions with a maximum of 25 words in each sentence.
- Write one topic in each paragraph, with a maximum of six sentences.
- Use the active voice.
- Use only simple verb forms: imperative, infinitive, simple present, simple past, and simple future.
- Do not use -ing forms, except in technical names.
- Do not remove articles or other small words to make text shorter.
- Do not use more than three words in a noun cluster.
- Use vertical lists for complex text.
- Use one word for one meaning, and use the same word for the same thing.

These relaxations apply:

- Prefer simple, common words, but do not check each word against the STE dictionary.
- Technical names and domain terms are permitted, for example `crate`, `workspace`, and `pull request`.
- Break a rule only when the strict form makes the text less clear or less accurate.
