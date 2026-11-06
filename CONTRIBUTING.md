# Contributing to lenka

Issues and pull requests are welcome. For a large change, open an issue first
so we can agree on the approach.

## License

lenka is licensed under [GPL-3.0-only](LICENSE). By contributing, you agree
that your contribution is licensed under the same terms. You keep the copyright
on your work.

## Sign your commits off

Sign off each commit to certify the
[Developer Certificate of Origin](https://developercertificate.org/), which
states that you have the right to submit the code under this license:

```sh
git commit -s
```

## Source file headers

Start each new source file with these lines:

```rust
// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) <year> <name>
```

Keep existing notices when you change a file. You may add your own copyright
line to a file that you change substantially.

## Before you open a pull request

CI runs these checks on every pull request:

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --all-features --locked
cargo deny check
```

Install `cargo-deny` with `cargo install --locked cargo-deny`.

## Dependencies

New dependencies must use a license that is allowed in [`deny.toml`](deny.toml)
and compatible with GPL-3.0-only.

## Project rules

[`docs/agents/`](docs/agents/README.md) holds the rules for code, Rust, and
tooling. These rules apply to people and to AI agents. You can propose a change
to them with a pull request.
