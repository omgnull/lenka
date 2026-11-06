---
paths:
  - "**/*.rs"
  - "**/Cargo.toml"
---

# Rust rules

The IDs in parentheses refer to the sources at the end of this file. `C-` IDs come from the Rust API Guidelines, and `M-` IDs come from the Microsoft Rust Guidelines.

## Lints

- Set lint levels in the `[lints]` table of `Cargo.toml`.
- Override a lint with `#[expect(lint, reason = "...")]`, not `#[allow]` (M-LINT-OVERRIDE-EXPECT). Generated code and macros can use `#[allow]`.
- Do not add `unsafe` code. `Cargo.toml` forbids it, so discuss a change to this rule in an issue first.

## Names

- Follow the standard casing for each item (C-CASE).
- Use `as_`, `to_`, and `into_` for conversions (C-CONV).
- Do not use a `get_` prefix on getters (C-GETTER).

## Types

- Use newtypes for domain values, such as a task ID (C-NEWTYPE).
- Use a custom type instead of a `bool` argument (C-CUSTOM-TYPE).
- Keep struct fields private (C-STRUCT-PRIVATE).
- Implement `Debug` on public types (C-DEBUG, M-PUBLIC-DEBUG).
- Write a custom `Debug` for a type that holds secrets, and test that the secret does not leak (M-PUBLIC-DEBUG).
- Make Cargo features additive, so that any combination works (M-FEATURES-ADDITIVE).

## Errors and panics

- Panic on a detected programming bug (M-PANIC-ON-BUG).
- Do not use a panic to report an error that a caller can handle (M-PANIC-IS-STOP).
- Give an intentional panic a message with the relevant values (M-PANIC-MESSAGE).
- Use `anyhow` or `eyre` only in application code (M-APP-ERROR).
- Use situation-specific error structs in library code (M-ERRORS-CANONICAL-STRUCTS).

## Documentation and logs

- Document public items with the sections Examples, Errors, Panics, and Safety where they apply (M-CANONICAL-DOCS, C-FAILURE).
- Use `?` in doc examples, not `unwrap` (C-QUESTION-MARK).
- Write structured logs with named properties, and redact sensitive data (M-LOG-STRUCTURED).

## Exceptions

- Ignore C-PERMISSIVE. lenka is `GPL-3.0-only`, and `deny.toml` controls dependency licenses.

## Sources

- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html)
- [Microsoft Rust Guidelines](https://microsoft.github.io/rust-guidelines/guidelines/)
- [Cargo `[lints]` table](https://doc.rust-lang.org/cargo/reference/manifest.html#the-lints-section)
