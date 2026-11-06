# Code rules

These rules apply to all work in this repository.

## Changes

- Add or change a test when you change behavior.
- Do not add a dependency if the standard library or an existing dependency can do the work.
- Do not put secrets or personal data in logs, error messages, or test fixtures.
- Validate data from outside the program where the data enters the program.

## Source files

- Start each new source file with the SPDX and copyright header in [`CONTRIBUTING.md`](../../CONTRIBUTING.md#source-file-headers).
- Keep existing license notices when you change a file.

## Commits

- Sign off each commit with `git commit -s`. [`CONTRIBUTING.md`](../../CONTRIBUTING.md#sign-your-commits-off) explains the reason.
- Run the checks in [`CLAUDE.md`](../../CLAUDE.md#commands) before you open a pull request.
