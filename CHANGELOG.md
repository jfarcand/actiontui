# Changelog

## [0.1.3] — 2026-06-10

- perf: ETag conditional requests (octocrab→reqwest) — 304s are free quota
- perf: cache active-workflow-id lookups (~10m TTL) to halve CI API calls
- feat: width-filling taller charts; r=rerun, +/- live refresh interval
- feat: workflow detail view (Enter) — 7-day duration chart + summary
- feat: add API rate-limit view (g key / --rate), mirroring ghrate


## [0.1.2] — 2026-06-06

- chore: relicense under Apache-2.0 only (drop MIT)
- ci: decouple crates.io publish from binary builds so a flaky runner can't block it


## [0.1.1] — 2026-06-06

Maintenance release — no user-facing behavior change. Dual-licensed
MIT OR Apache-2.0; adopted a strict clippy lint policy and a structured error
type (replacing `anyhow`); pinned the Rust toolchain (1.94.0) for reproducible
CI; added git hooks and contributor standards (`CLAUDE.md`).

## [0.1.0] — 2026-06-05

First release.

### Added

- Cross-repo **GitHub Actions** dashboard: latest run per workflow with status,
  started/finished, duration, ETA, a recent-history dot column, and the head
  commit (clickable in one-shot output).
- **Watch mode** — alt-screen TUI with background refresh, animated spinner,
  row selection, re-run a workflow (`x`), open a commit (`o`), and a 6h auto-exit.
- **Stats view** (`t`) — per-repo Stars/Forks/Watchers/Issues/PRs with
  day-over-day deltas and a star-history chart, persisted to SQLite.
- **Aggregate** view, repo resolution from `config.toml`/`repos.conf`/git remote,
  workflow `exclude` filters, and desktop notifications (with sound) on
  red↔green transitions.
