//! Watch-mode TUI: a live, alt-screen dashboard with background refresh,
//! spinner animation, keyboard control, and a hard auto-exit ceiling.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::Local;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use octocrab::Octocrab;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;

use crate::model::RepoResult;
use crate::state::State;
use crate::ui::{self, Frame, WatchInfo};

/// A selectable row, flattened across repos in render order.
struct Sel {
    repo: String,
    run_id: u64,
    workflow: String,
    sha: Option<String>,
}

/// A pending re-run awaiting y/n confirmation.
struct Confirm {
    repo: String,
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

    /// Index into the flattened selectable rows.
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
    ) -> App {
        let now = Instant::now();
        App {
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
        let (tx, mut rx) = mpsc::channel::<Vec<RepoResult>>(4);

        self.trigger_refresh(&tx);
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
                        self.trigger_refresh(&tx);
                    }
                    if self.started.elapsed() >= MAX_WATCH {
                        break;
                    }
                }
                Some(results) = rx.recv() => {
                    self.apply(results);
                }
            }
            self.draw(terminal)?;
        }
        Ok(())
    }

    /// Spawn a background fetch for all repos; results arrive over the channel.
    fn trigger_refresh(&mut self, tx: &mpsc::Sender<Vec<RepoResult>>) {
        self.loading = true;
        let octo = Arc::clone(&self.octo);
        let repos = self.repos.clone();
        let branch = self.branch.clone();
        let exclude = self.exclude.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let results = fetch_all(&octo, &repos, &branch, &exclude).await;
            let _ = tx.send(results).await;
        });
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
                    workflow: row.workflow_name.clone(),
                    sha: row.head_sha.clone(),
                });
            }
        }
        out
    }

    /// Handle a keypress. Returns `false` to quit.
    fn on_key(&mut self, key: KeyEvent, tx: &mpsc::Sender<Vec<RepoResult>>) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let rows = self.selectable();

        // A pending re-run confirmation swallows input until answered.
        if self.confirm.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    let c = self.confirm.take().unwrap();
                    match rerun(&c.repo, c.run_id) {
                        Ok(()) => {
                            self.status =
                                Some(format!("⟳ re-run triggered — {} ({})", c.workflow, c.repo));
                            self.trigger_refresh(tx);
                        }
                        Err(e) => self.status = Some(format!("✗ re-run failed: {e}")),
                    }
                }
                _ => {
                    self.confirm = None;
                    self.status = Some("re-run cancelled".into());
                }
            }
            return true;
        }

        match key.code {
            KeyCode::Char('q') => return false,
            KeyCode::Esc => return false,
            KeyCode::Char('c') if ctrl => return false,

            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < rows.len() {
                    self.selected += 1;
                }
            }

            KeyCode::Char('r') | KeyCode::Char('R') => {
                if !self.loading {
                    self.trigger_refresh(tx);
                }
            }
            KeyCode::Char('t') => {
                crate::notify::test(self.sound);
                self.status = Some("test notification sent".into());
            }
            KeyCode::Char('o') => {
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
            KeyCode::Char('x') | KeyCode::Enter => {
                if let Some(s) = rows.get(self.selected) {
                    self.confirm = Some(Confirm {
                        repo: s.repo.clone(),
                        run_id: s.run_id,
                        workflow: s.workflow.clone(),
                    });
                }
            }
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
        let n = self.selectable().len();
        let selected = (n > 0).then(|| self.selected.min(n - 1));
        let frame = Frame {
            results: &self.results,
            aggregate: self.aggregate,
            branch: &self.branch,
            now: Local::now(),
            watch: Some(WatchInfo {
                interval: self.interval.as_secs(),
                remaining,
            }),
            spinner: self.spinner,
            loading: self.loading,
            hyperlinks: false,
            selected,
            prompt: self.prompt_line(),
        };
        let lines = ui::build_lines(&frame);
        terminal.draw(|f| {
            f.render_widget(Paragraph::new(lines), f.area());
        })?;
        Ok(())
    }
}

/// Trigger a re-run of a workflow run via `gh api` (reuses gh's auth + scopes).
fn rerun(repo: &str, run_id: u64) -> Result<(), String> {
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
