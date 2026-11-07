---
paths:
  - ".github/**/*"
  - "**/Cargo.toml"
  - "deny.toml"
---

# Tooling rules

## GitHub Actions

Pin every action to a full 40-character commit SHA, for compatibility and security. Never use a tag or branch such as `@v7` or `@main`, in new workflows or when editing existing ones. Add the version as a trailing comment:

```yaml
- uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
```

Get the SHA for a release from GitHub, not from memory:

```sh
gh api repos/<owner>/<repo>/commits/<tag> --jq .sha
```

Dependabot updates the pinned SHAs. CI installs Rust with `rustup` from the runner image, not with a third-party action.

## Workspace builds in CI

`--all-features` does not find a feature combination that fails to compile. CI therefore also builds the minimal feature set and each plugin alone. Add a new plugin feature to the `features` job in `.github/workflows/ci.yml`.

## Mutation check in CI

The `mutants` job runs `cargo mutants --in-diff` on a pull request. It covers only the changed lines of `lenka-tasks`, `lenka-core`, `lenka-mcp`, and `crates/lenka/src/hook.rs`. It uses the tests of all crates except `lenka-desktop`.

- The job is a report, and a missed mutant does not stop the pull request.
- The job skips a change with more than 40 mutants, because each mutant takes about one minute.
- Add a path to the `git diff` in the job when a new crate holds engine rules.

## Lints in CI

CI runs clippy with `-D warnings`, so a lint at the `warn` level stops CI when it fires. Run the clippy command in [`CLAUDE.md`](../../CLAUDE.md#commands) after you add a lint to `Cargo.toml`.

## Dependency licenses

`deny.toml` allows only licenses that are compatible with `GPL-3.0-only`. Confirm that a license is compatible before you add it to the allow list.
