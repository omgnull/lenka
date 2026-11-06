# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

lenka is an AI-first project management and collaboration tool, written in Rust (edition 2024, stable toolchain). It is at an early stage: a single binary crate with only a placeholder `src/main.rs`, so there is no architecture to follow yet.

## Commands

```sh
cargo build
cargo run
cargo test
cargo test <name>                  # tests whose name contains <name>
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
