# Changelog

## [0.1.1] — 2026-06-06



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
