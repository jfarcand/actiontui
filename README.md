# actiontui

[![CI](https://github.com/jfarcand/actiontui/actions/workflows/ci.yml/badge.svg)](https://github.com/jfarcand/actiontui/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/actiontui.svg)](https://crates.io/crates/actiontui)
[![license](https://img.shields.io/crates/l/actiontui.svg)](LICENSE)

A [Ratatui](https://ratatui.rs) terminal dashboard for watching **GitHub Actions** workflow runs across one or more repositories — with a live-refreshing TUI, recent-history sparkdots, ETA estimates, and desktop notifications (with sound) when CI turns red or recovers.

It's a Rust rewrite of a shell tool, built for a richer terminal experience: animated spinners, colored status badges, a "Recent" run-history column, and an alt-screen watch mode with bounded memory.

```
  GitHub Actions  2026-06-05 17:07:16

  jfarcand/pierre_mcp_server (main)
  ┌────────────────────────┬──────────┬────────────────┬────────────────┬──────────┬──────────┬───────────────┬────────────┐
  │ Workflow               │ Status   │ Started        │ Finished       │ Duration │ ETA      │ Recent        │ FailSince  │
  ├────────────────────────┼──────────┼────────────────┼────────────────┼──────────┼──────────┼───────────────┼────────────┤
  │ API Contracts          │ pass     │ 02-16 18:26:05 │ 02-16 18:34:55 │ 8m 50s   │ --       │ ● ●           │ --         │
  │ Backend CI             │ FAIL     │ 03-07 23:17:54 │ 03-08 00:00:48 │ 42m 54s  │ --       │ ● ● ● ● ● ●   │ 04948a9    │
  │ Code Coverage          │ pass     │ 03-07 23:40:23 │ 03-08 00:22:20 │ 41m 57s  │ --       │ ● ● ● ● ● ●   │ --         │
  └────────────────────────┴──────────┴────────────────┴────────────────┴──────────┴──────────┴───────────────┴────────────┘
```

## Features

- **Per-workflow table** — latest run per workflow on a branch: status, started/finished (local time), duration, ETA, recent history, and the commit that kicked it off.
- **Recent column** — the last few runs as colored dots: `●` green pass, `●` red fail, `◐` running, `○` other. Spot a flaky workflow at a glance.
- **Clickable commit** — the Commit column shows the latest run's head SHA (red when failing). In one-shot output it's an OSC-8 hyperlink to the commit on GitHub (⌘-click in iTerm2); in watch mode, press `o` to open the selected row's commit.
- **ETA** — for in-progress runs, estimated time remaining based on the most recent successful run's duration (`~3m 10s`), turning red with `+overrun` once it runs long.
- **Watch mode** — a live, alt-screen TUI that refreshes in the background with an animated spinner, a refresh countdown, row selection, and a 6h auto-exit.
- **Re-run from the TUI** — select a workflow with `↑`/`↓` and press `x` to re-run it (with a `y`/`n` confirm), via `gh api`. No browser round-trip.
- **Stats view** (`t`) — per-repo Stars / Forks / Watchers / Issues / PRs with day-over-day deltas, plus a full-width unicode chart of the selected repo's star history. Snapshots are persisted to SQLite (`~/.config/actiontui/stats.db`) so the trend grows over time — far past GitHub's own 14-day traffic window. Launch straight into it with `--stats`.
- **Aggregate view** — collapse every repo into one table grouped by repo.
- **Notifications** — on a green→red or red→green transition, fires a macOS notification + distinct sound (`Basso` for failure, `Glass` for recovery); degrades to a terminal bell elsewhere. Test the channel any time with `--test-notify` or the `t` key.
- **Efficient** — one page of runs per repo, with latest/recent/commit/ETA all derived client-side. Repos fetched concurrently.

## Install

Requires the [`gh`](https://cli.github.com) CLI authenticated (`gh auth login`) — actiontui pulls its token from `gh auth token`, or from `GH_TOKEN`/`GITHUB_TOKEN`.

```sh
cargo install --path .
# or, after publishing:
# cargo install actiontui
```

## Usage

```sh
actiontui                                  # current repo's git remote, main branch
actiontui -b feature-x                     # a specific branch
actiontui -R owner/repo                     # a specific repo
actiontui -R owner/repo1 -R owner/repo2     # multiple repos
actiontui owner/repo1 owner/repo2           # multiple repos (positional)
actiontui -w                                # watch mode (60s refresh)
actiontui -w 30                             # watch mode, 30s refresh
actiontui -a -R r1 -R r2                    # aggregate into a single table
actiontui --no-sound -w                     # visual notifications only
actiontui --test-notify                     # fire a sample notification + sound, then exit
actiontui --stats                           # launch into the repo Stats view
```

```sh
actiontui -x "Update #" -x "in /."         # hide workflows matching either pattern
```

### Repo resolution

Repos are resolved in this order:

1. `-R`/`--repo` flags and positional args
2. `repos` in `~/.config/actiontui/config.toml`
3. `~/.config/actiontui/repos.conf` — one `owner/repo` per line (`#` comments allowed)
4. the `origin` git remote of the current directory

### Keys (watch mode)

| Key             | Action                                   |
| --------------- | ---------------------------------------- |
| `t`             | toggle the **Stats** view (CI ↔ stats)   |
| `↑` / `↓` (`k`/`j`) | move the selection                   |
| `x` / `Enter`   | re-run the selected workflow (`y`/`n` confirm) *(CI view)* |
| `o`             | open the selected row's commit in the browser *(CI view)* |
| `T`             | fire a test notification + sound         |
| `r` / `R`       | refresh now                              |
| `q` / `Ctrl-C`  | quit (`Esc` leaves the Stats view, else quits) |

## Configuration

`~/.config/actiontui/config.toml` holds defaults (CLI flags override it):

```toml
repos = ["owner/repo1", "owner/repo2"]
branch = "main"
aggregate = true
sound = true
# Hide workflows whose name contains any of these (case-insensitive):
exclude = ["Update #", "in /."]   # drops Dependabot version-update runs
# Launch in watch mode without typing -w:
# watch = true
# interval = 60
```

| Path                                  | Purpose                                       |
| ------------------------------------- | --------------------------------------------- |
| `~/.config/actiontui/config.toml`     | defaults (repos, branch, aggregate, exclude…) |
| `~/.config/actiontui/repos.conf`      | alternate repo list (one per line)            |
| `~/.config/actiontui/state.json`      | last-known conclusions (transition detection) |
| `~/.config/actiontui/stats.db`        | SQLite history of repo stats (for the chart)  |

## How it works

For each repo, actiontui fetches one page (100) of workflow runs for the branch plus the list of active workflows, then derives — entirely client-side — the latest run per workflow, the recent-history dots, the failing-since commit (oldest run of the current consecutive-failure streak), and the ETA (most recent successful run's wall-clock duration). State transitions are detected by diffing against the persisted `state.json`.

## Releasing

CI (`.github/workflows/ci.yml`) runs fmt + clippy + build + test on every push and PR.

Publishing to [crates.io](https://crates.io) is automated by `.github/workflows/release.yml` — push a version tag and it verifies the tag matches `Cargo.toml`, builds, and publishes:

```sh
# bump version in Cargo.toml first, then:
git tag v0.1.0
git push origin v0.1.0
```

Requires a repository secret `CARGO_REGISTRY_TOKEN` (a crates.io API token from <https://crates.io/settings/tokens>).

## Roadmap

Recently shipped: clickable commit SHA, on-demand notification/sound test, and re-run from the TUI. Ideas under consideration:

- **Re-run only failed jobs** — `rerun-failed-jobs` as an alternate to a full re-run.
- **Branch switcher** — change the inspected branch from within the TUI.
- **Per-workflow drill-in** — expand a row to its recent runs / job list.

## License

MIT

