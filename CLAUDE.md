# actiontui — Agent guide

A single-binary Rust CLI/TUI (Ratatui) that watches GitHub Actions across many
repos. Adapted from the dravr-platform standards, scoped to a small CLI crate.

## Git workflow

- **NEVER open Pull Requests** (`gh pr create` is forbidden). Small fixes go
  straight to `main`; larger work on a short-lived branch, then **local squash
  merge** into `main` followed by deleting the branch in the same session.
- Push to `main` directly for fixes: `git push origin main`.

## Commit messages (enforced by `.build/hooks/commit-msg`)

- **Max 2 lines.** Line 1 = brief summary with a conventional prefix
  (`feat:`/`fix:`/`docs:`/`ci:`/`chore:`/`test:`). Line 2 = optional detail.
- **No AI attribution.** No `Co-Authored-By: Claude/Anthropic`, no "Generated
  with", no 🤖. (This repo overrides the global default that adds them.)

## Validation before pushing

Install hooks once: `./scripts/setup-hooks.sh` (sets `core.hooksPath`).

The `pre-push` hook runs the local gate; you can run it by hand too:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Every `.rs` file starts with `// SPDX-License-Identifier: Apache-2.0` (pre-commit checks it).

## After Pushing — MANDATORY CI MONITORING

**A push is the START of validation, not the end.** The local gate only catches
fmt/clippy/test regressions on your machine; CI runs them clean on Linux.

- After every push, watch CI for the pushed commit until it reaches a terminal
  state. Work is **not done** until CI is green on the head commit.
- If CI fails, fix it and re-push in the same session — don't move on.
- **Monitor cheaply.** Prefer a single `gh run list --branch main` or a WebFetch
  of the Actions page. **NEVER** use `gh run watch` or tight `while`/`sleep`
  polling loops (they burn API quota). For longer waits, use `ScheduleWakeup`
  to re-check after a delay.

## Test output verification

`cargo test` exits 0 even when **0 tests run**. After any test command, confirm
`running N tests` with N > 0 and `N passed`. `running 0 tests` / `0 passed` /
`filtered out` means the run FAILED — never claim tests pass when 0 ran.

## Error handling

- Propagate with `?`; return `Result`/`Option`. Prefer `.context()` (anyhow) to
  add a human message at fallible boundaries.
- **No `unwrap()`/`expect()`/`panic!()` in non-test code**, except: invariants on
  compile-time-constant data, or `main()`/setup where failing should abort.
- Note: unlike dravr-platform, `anyhow` + `.context()` IS the intended error
  style here (small CLI binary) — we do not mandate structured error enums.

## Code style

- Match the surrounding code's idiom, naming, and comment density.
- Keep `rustfmt` clean and `clippy --all-targets -- -D warnings` green.
