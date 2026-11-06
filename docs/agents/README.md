# Rules for contributors and agents

This folder holds the project rules for people and for AI agents of any vendor. Everybody can read these files and propose changes with a pull request.

| File | Topic | Applies to |
|---|---|---|
| [`code.md`](code.md) | General code and commit rules | All work |
| [`rust.md`](rust.md) | Rust code and Cargo files | `**/*.rs`, `**/Cargo.toml` |
| [`tooling.md`](tooling.md) | CI, Dependabot, and cargo-deny | `.github/**/*`, `Cargo.toml`, `deny.toml` |

Read the files for your task before you start. Claude Code loads them automatically through symlinks in `.claude/rules/`.

## Change a rule

- Keep each rule short, specific, and possible to check.
- Add a source for a rule that comes from a standard or a style guide.
- Remove a rule when it stops being true. Two rules that contradict each other are a defect.
- Use the writing style in [`CLAUDE.md`](../../CLAUDE.md#writing-style).

## Add a rule file

1. Add the file to this folder, with one topic in each file.
2. Add `paths` frontmatter if the rule applies only to some files. Claude Code reads this field, and other tools can ignore it.
3. Add the file to the table in this README.
4. Add a symlink for Claude Code:

   ```sh
   ln -s ../../docs/agents/<file>.md .claude/rules/<file>.md
   ```

Put long procedures and reference text in a skill, not in a rule. Claude Code loads each rule without `paths` in each session, so long rules cost context.

## Windows

Git checks out a symlink as a plain text file unless `core.symlinks` is on. On such a clone, `.claude/rules/` holds only paths. To correct this, enable Developer Mode, set `core.symlinks` to `true`, and clone again.
