// SPDX-License-Identifier: Apache-2.0
//! Watch-mode TUI: a live, alt-screen dashboard with background refresh,
//! spinner animation, keyboard control, and a hard auto-exit ceiling.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{Local, Utc};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use octocrab::Octocrab;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;

use crate::error::Result;
use crate::model::{RateBucket, RateRow, RepoResult, RepoStats, StatsRow, WorkflowDetail};
use crate::state::State;
use crate::statsdb::StatsDb;
use crate::ui::{self, DetailFrame, Frame, RateFrame, StatsFrame, WatchInfo};

/// Which view is on screen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Ci,
    Stats,
    Rate,
    Detail,
}

/// A background-fetch result delivered over the channel.
enum Msg {
    Ci(Vec<RepoResult>),
    Stats(Vec<RepoStats>),
    Rate(Vec<RateBucket>),
    Detail(Box<WorkflowDetail>),
}

/// API quota below which the rate view fires a one-shot alert.
const RATE_ALERT_THRESHOLD: i64 = 1000;

/// Time window for the workflow detail chart.
const DETAIL_DAYS: u32 = 7;

/// Seconds added/removed per `+`/`-` keypress (clamped to 5s..1h).
const INTERVAL_STEP: i64 = 15;

/// Which view the TUI opens in.
#[derive(Clone, Copy)]
pub enum StartView {
    Ci,
    Stats,
    Rate,
}

/// A selectable row, flattened across repos in render order.
struct Sel {
    repo: String,
    run_id: u64,
    workflow_id: u64,
    workflow: String,
    sha: Option<String>,
}

/// A pending re-run awaiting y/n confirmation.
struct Confirm {
    repo: String,
    run_id: u64,
    workflow: String,
}

/// The workflow drilled into for the detail view.
struct DetailTarget {
    repo: String,
    workflow_id: u64,
    run_id: u64,
    workflow: String,
}

/// Auto-stop a forgotten watch after 6h (matches the original's ceiling, and
/// keeps long-lived sessions from accumulating).
const MAX_WATCH: Duration = Duration::from_secs(6 * 3600);

pub struct App {
    octo: Arc<Octocrab>,
    repos: Vec<String>,
    branch: String,
    aggregate: bool,
    sound: bool,
    exclude: Vec<String>,
    interval: Duration,

    results: Vec<RepoResult>,
    loading: bool,
    spinner: usize,
    last_refresh: Instant,
    started: Instant,
    state: State,

    /// Active view (CI workflows, repo stats, or API rate limits).
    view: View,
    /// Stats rows + persistence (lazily loaded the first time Stats is shown).
    stats: Vec<StatsRow>,
    stats_loaded: bool,
    statsdb: StatsDb,
    /// Rate-limit rows + per-bucket used baseline for deltas.
    rate: Vec<RateRow>,
    rate_loaded: bool,
    rate_prev_used: HashMap<String, i64>,
    rate_alert_fired: bool,
    /// The drilled-in workflow + its fetched detail.
    detail_target: Option<DetailTarget>,
    detail: Option<WorkflowDetail>,

    /// Index into the active view's rows.
    selected: usize,
    /// Pending re-run confirmation.
    confirm: Option<Confirm>,
    /// Transient status line (cleared on the next status-producing action).
    status: Option<String>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        octo: Octocrab,
        repos: Vec<String>,
        branch: String,
        aggregate: bool,
        sound: bool,
        exclude: Vec<String>,
        interval_secs: u64,
        state: State,
        statsdb: StatsDb,
        start_view: StartView,
    ) -> Self {
        let now = Instant::now();
        Self {
            octo: Arc::new(octo),
            repos,
            branch,
            aggregate,
            sound,
            exclude,
            interval: Duration::from_secs(interval_secs.max(5)),
            results: Vec::new(),
            loading: false,
            spinner: 0,
            last_refresh: now,
            started: now,
            state,
            view: match start_view {
                StartView::Ci => View::Ci,
                StartView::Stats => View::Stats,
                StartView::Rate => View::Rate,
            },
            stats: Vec::new(),
            stats_loaded: false,
            statsdb,
            rate: Vec::new(),
            rate_loaded: false,
            rate_prev_used: HashMap::new(),
            rate_alert_fired: false,
            detail_target: None,
            detail: None,
            selected: 0,
            confirm: None,
            status: None,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        let mut terminal = ratatui::init();
        let res = self.event_loop(&mut terminal).await;
        ratatui::restore();
        res
    }

    async fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        let mut events = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(120));
        let (tx, mut rx) = mpsc::channel::<Msg>(4);

        self.refresh_active(&tx);
        self.draw(terminal)?;

        loop {
            tokio::select! {
                maybe_event = events.next() => {
                    match maybe_event {
                        Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                            if !self.on_key(key, &tx) {
                                break;
                            }
                        }
                        Some(Ok(_)) => {}        // resize etc. → redraw below
                        Some(Err(_)) | None => break,
                    }
                }
                _ = tick.tick() => {
                    if self.loading {
                        self.spinner = self.spinner.wrapping_add(1);
                    }
                    if !self.loading && self.last_refresh.elapsed() >= self.interval {
                        self.refresh_active(&tx);
                    }
                    if self.started.elapsed() >= MAX_WATCH {
                        break;
                    }
                }
                Some(msg) = rx.recv() => {
                    match msg {
                        Msg::Ci(results) => self.apply(results),
                        Msg::Stats(stats) => self.apply_stats(stats),
                        Msg::Rate(buckets) => self.apply_rate(buckets),
                        Msg::Detail(detail) => self.apply_detail(*detail),
                    }
                }
            }
            self.draw(terminal)?;
        }
        Ok(())
    }

    /// Refresh whichever view is active.
    fn refresh_active(&mut self, tx: &mpsc::Sender<Msg>) {
        match self.view {
            View::Ci => self.trigger_refresh(tx),
            View::Stats => self.trigger_stats(tx),
            View::Rate => self.trigger_rate(tx),
            View::Detail => self.trigger_detail(tx),
        }
    }

    /// Spawn a background CI fetch for all repos; results arrive over the channel.
    fn trigger_refresh(&mut self, tx: &mpsc::Sender<Msg>) {
        self.loading = true;
        let octo = Arc::clone(&self.octo);
        let repos = self.repos.clone();
        let branch = self.branch.clone();
        let exclude = self.exclude.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let results = fetch_all(&octo, &repos, &branch, &exclude).await;
            let _ = tx.send(Msg::Ci(results)).await;
        });
    }

    /// Spawn a background stats fetch for all repos.
    fn trigger_stats(&mut self, tx: &mpsc::Sender<Msg>) {
        self.loading = true;
        let octo = Arc::clone(&self.octo);
        let repos = self.repos.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let stats = fetch_stats_all(&octo, &repos).await;
            let _ = tx.send(Msg::Stats(stats)).await;
        });
    }

    /// Spawn a background rate-limit fetch.
    fn trigger_rate(&mut self, tx: &mpsc::Sender<Msg>) {
        self.loading = true;
        let octo = Arc::clone(&self.octo);
        let tx = tx.clone();
        tokio::spawn(async move {
            let _ = match crate::github::fetch_rate(&octo).await {
                Ok(buckets) => tx.send(Msg::Rate(buckets)).await,
                Err(_) => tx.send(Msg::Rate(Vec::new())).await,
            };
        });
    }

    /// Drill into the selected CI row's workflow detail.
    fn open_detail(&mut self, tx: &mpsc::Sender<Msg>) {
        let rows = self.selectable();
        let Some(s) = rows.get(self.selected) else {
            return;
        };
        self.detail_target = Some(DetailTarget {
            repo: s.repo.clone(),
            workflow_id: s.workflow_id,
            run_id: s.run_id,
            workflow: s.workflow.clone(),
        });
        self.detail = None;
        self.view = View::Detail;
        if !self.loading {
            self.trigger_detail(tx);
        }
    }

    /// Spawn a background fetch of the drilled-in workflow's run history.
    fn trigger_detail(&mut self, tx: &mpsc::Sender<Msg>) {
        let Some(t) = &self.detail_target else {
            return;
        };
        let (repo, workflow_id, workflow) = (t.repo.clone(), t.workflow_id, t.workflow.clone());
        self.loading = true;
        let octo = Arc::clone(&self.octo);
        let branch = self.branch.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let detail = crate::github::fetch_workflow_detail(
                &octo,
                &repo,
                workflow_id,
                &workflow,
                &branch,
                DETAIL_DAYS,
            )
            .await
            .unwrap_or(WorkflowDetail {
                days: DETAIL_DAYS,
                runs: Vec::new(),
            });
            let _ = tx.send(Msg::Detail(Box::new(detail))).await;
        });
    }

    fn apply_detail(&mut self, detail: WorkflowDetail) {
        self.detail = Some(detail);
        self.loading = false;
        self.last_refresh = Instant::now();
    }

    /// Go to `target`, or back to CI if already there. Loads data on first entry.
    fn switch_view(&mut self, target: View, tx: &mpsc::Sender<Msg>) {
        self.view = if self.view == target {
            View::Ci
        } else {
            target
        };
        self.confirm = None;
        self.selected = 0;
        if self.loading {
            return;
        }
        match self.view {
            View::Ci if self.results.is_empty() => self.trigger_refresh(tx),
            View::Stats if !self.stats_loaded => self.trigger_stats(tx),
            View::Rate if !self.rate_loaded => self.trigger_rate(tx),
            _ => {}
        }
    }

    fn apply_rate(&mut self, buckets: Vec<RateBucket>) {
        let rows: Vec<RateRow> = buckets
            .into_iter()
            .map(|b| {
                let delta = self
                    .rate_prev_used
                    .insert(b.name.clone(), b.used)
                    .map(|prev| b.used - prev);
                RateRow {
                    bucket: b,
                    delta_used: delta,
                }
            })
            .collect();

        // One-shot alert when the core bucket dips below the threshold.
        if let Some(core) = rows.iter().find(|r| r.bucket.name == "core") {
            if core.bucket.remaining < RATE_ALERT_THRESHOLD {
                if !self.rate_alert_fired {
                    crate::notify::rate_alert(
                        "core",
                        core.bucket.remaining,
                        core.bucket.limit,
                        self.sound,
                    );
                    self.rate_alert_fired = true;
                }
            } else {
                self.rate_alert_fired = false;
            }
        }

        self.rate = rows;
        self.rate_loaded = true;
        self.loading = false;
        self.last_refresh = Instant::now();
    }

    fn apply_stats(&mut self, stats: Vec<RepoStats>) {
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let mut rows = Vec::with_capacity(stats.len());
        for st in stats {
            let (prev, trend) = if st.error.is_none() {
                let _ = self.statsdb.record(&st.repo, &today, &st.snapshot);
                (
                    self.statsdb.previous(&st.repo, &today).ok().flatten(),
                    self.statsdb.star_history(&st.repo, 60).unwrap_or_default(),
                )
            } else {
                (None, Vec::new())
            };
            rows.push(StatsRow {
                stats: st,
                prev,
                trend,
            });
        }
        self.stats = rows;
        self.stats_loaded = true;
        self.loading = false;
        self.last_refresh = Instant::now();

        let n = self.stats.len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

    /// Adjust the live refresh interval (clamped 5s..1h).
    fn adjust_interval(&mut self, delta: i64) {
        let secs = (self.interval.as_secs() as i64 + delta).clamp(5, 3600) as u64;
        self.interval = Duration::from_secs(secs);
        self.status = Some(format!("refresh every {secs}s"));
    }

    /// Number of selectable rows in the active view.
    fn active_len(&self) -> usize {
        match self.view {
            View::Ci => self.selectable().len(),
            View::Stats => self.stats.len(),
            View::Rate | View::Detail => 0, // not selectable
        }
    }

    fn apply(&mut self, results: Vec<RepoResult>) {
        let transitions = self.state.diff(&results);
        crate::notify::announce(&transitions, &self.branch, self.sound);
        self.state.commit(&results);
        self.results = results;
        self.loading = false;
        self.last_refresh = Instant::now();

        // Keep the selection within bounds as rows come and go.
        let n = self.selectable().len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

    /// Flatten all data rows across repos, in the same order the UI renders them.
    fn selectable(&self) -> Vec<Sel> {
        let mut out = Vec::new();
        for repo in &self.results {
            for row in &repo.rows {
                out.push(Sel {
                    repo: repo.repo.clone(),
                    run_id: row.run_id,
                    workflow_id: row.workflow_id,
                    workflow: row.workflow_name.clone(),
                    sha: row.head_sha.clone(),
                });
            }
        }
        out
    }

    /// Handle a keypress. Returns `false` to quit.
    fn on_key(&mut self, key: KeyEvent, tx: &mpsc::Sender<Msg>) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // A pending re-run confirmation swallows input until answered.
        if let Some(c) = self.confirm.take() {
            if let KeyCode::Char('y' | 'Y') = key.code {
                match rerun(&c.repo, c.run_id) {
                    Ok(()) => {
                        self.status =
                            Some(format!("⟳ re-run triggered — {} ({})", c.workflow, c.repo));
                        self.refresh_active(tx);
                    }
                    Err(e) => self.status = Some(format!("✗ re-run failed: {e}")),
                }
            } else {
                self.status = Some("re-run cancelled".into());
            }
            return true;
        }

        let in_ci = self.view == View::Ci;
        match key.code {
            KeyCode::Char('q') => return false,
            KeyCode::Char('c') if ctrl => return false,
            // Esc leaves an overlay view back to CI; from CI it quits.
            KeyCode::Esc => {
                if self.view == View::Ci {
                    return false;
                }
                self.switch_view(View::Ci, tx);
            }

            KeyCode::Char('t') => self.switch_view(View::Stats, tx),
            KeyCode::Char('g') => self.switch_view(View::Rate, tx),
            KeyCode::Char('T') => {
                crate::notify::test(self.sound);
                self.status = Some("test notification sent".into());
            }

            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.active_len() {
                    self.selected += 1;
                }
            }

            KeyCode::Char('R') => {
                if !self.loading {
                    self.refresh_active(tx);
                }
            }
            // Change the refresh interval on the fly.
            KeyCode::Char('+' | '=') => self.adjust_interval(INTERVAL_STEP),
            KeyCode::Char('-' | '_') => self.adjust_interval(-INTERVAL_STEP),

            // Re-run: the selected CI row, or the workflow being viewed in detail.
            KeyCode::Char('r') => {
                let target = match self.view {
                    View::Ci => self
                        .selectable()
                        .get(self.selected)
                        .map(|s| (s.repo.clone(), s.run_id, s.workflow.clone())),
                    View::Detail => self
                        .detail_target
                        .as_ref()
                        .map(|t| (t.repo.clone(), t.run_id, t.workflow.clone())),
                    _ => None,
                };
                if let Some((repo, run_id, workflow)) = target {
                    self.confirm = Some(Confirm {
                        repo,
                        run_id,
                        workflow,
                    });
                }
            }

            // CI-only actions.
            KeyCode::Char('o') if in_ci => {
                let rows = self.selectable();
                if let Some(s) = rows.get(self.selected) {
                    if let Some(sha) = &s.sha {
                        open_url(&ui::commit_url(&s.repo, sha));
                        let short: String = sha.chars().take(7).collect();
                        self.status = Some(format!("opened commit {short}"));
                    } else {
                        self.status = Some("no commit for that row".into());
                    }
                }
            }
            KeyCode::Enter if in_ci => self.open_detail(tx),
            _ => {}
        }
        true
    }

    /// The confirm prompt or transient status, rendered under the header.
    fn prompt_line(&self) -> Option<Line<'static>> {
        if let Some(c) = &self.confirm {
            return Some(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("Re-run “{}” in {} ?", c.workflow, c.repo),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "   y = yes · n = no".to_string(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        self.status.as_ref().map(|s| {
            Line::from(vec![
                Span::raw("  "),
                Span::styled(s.clone(), Style::default().fg(Color::Cyan)),
            ])
        })
    }

    fn draw(&self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        let remaining =
            self.interval.as_secs() as i64 - self.last_refresh.elapsed().as_secs() as i64;
        let watch = WatchInfo {
            interval: self.interval.as_secs(),
            remaining,
        };
        let n = self.active_len();
        let selected = (n > 0).then(|| self.selected.min(n - 1));
        let width = terminal.size().map_or(80, |s| s.width);

        let lines = match self.view {
            View::Ci => ui::build_lines(&Frame {
                results: &self.results,
                aggregate: self.aggregate,
                branch: &self.branch,
                now: Local::now(),
                watch: Some(watch),
                spinner: self.spinner,
                loading: self.loading,
                hyperlinks: false,
                selected,
                prompt: self.prompt_line(),
            }),
            View::Stats => ui::build_stats_lines(&StatsFrame {
                rows: &self.stats,
                now: Local::now(),
                watch: Some(watch),
                spinner: self.spinner,
                loading: self.loading,
                selected,
                width,
            }),
            View::Rate => ui::build_rate_lines(&RateFrame {
                rows: &self.rate,
                now: Local::now(),
                watch: Some(watch),
                spinner: self.spinner,
                loading: self.loading,
            }),
            View::Detail => {
                let (repo, workflow) = self
                    .detail_target
                    .as_ref()
                    .map_or(("", ""), |t| (t.repo.as_str(), t.workflow.as_str()));
                ui::build_detail_lines(&DetailFrame {
                    repo,
                    workflow,
                    detail: self.detail.as_ref(),
                    now: Local::now(),
                    watch: Some(watch),
                    spinner: self.spinner,
                    loading: self.loading,
                    width,
                })
            }
        };
        terminal.draw(|f| {
            f.render_widget(Paragraph::new(lines), f.area());
        })?;
        Ok(())
    }
}

/// Fetch stats for every repo concurrently.
pub async fn fetch_stats_all(octo: &Octocrab, repos: &[String]) -> Vec<RepoStats> {
    let futs = repos.iter().map(|r| crate::github::fetch_stats(octo, r));
    futures::future::join_all(futs).await
}

/// Trigger a re-run of a workflow run via `gh api` (reuses gh's auth + scopes).
/// Returns a human message string on failure (shown in the status line).
fn rerun(repo: &str, run_id: u64) -> std::result::Result<(), String> {
    let out = std::process::Command::new("gh")
        .args([
            "api",
            "--method",
            "POST",
            &format!("repos/{repo}/actions/runs/{run_id}/rerun"),
        ])
        .output()
        .map_err(|e| format!("could not run gh: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let msg = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("gh api failed");
    Err(msg.to_string())
}

/// Open a URL in the default browser (macOS `open`).
fn open_url(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
    }
}

/// Fetch every repo concurrently.
pub async fn fetch_all(
    octo: &Octocrab,
    repos: &[String],
    branch: &str,
    exclude: &[String],
) -> Vec<RepoResult> {
    let futs = repos
        .iter()
        .map(|r| crate::github::fetch_repo(octo, r, branch, exclude));
    futures::future::join_all(futs).await
}
