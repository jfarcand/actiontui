//! Watch-mode TUI: a live, alt-screen dashboard with background refresh,
//! spinner animation, keyboard control, and a hard auto-exit ceiling.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::Local;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use octocrab::Octocrab;
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;

use crate::model::RepoResult;
use crate::state::State;
use crate::ui::{self, Frame, WatchInfo};

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
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc => break,
                                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                                KeyCode::Char('r') | KeyCode::Char('R') => {
                                    if !self.loading {
                                        self.trigger_refresh(&tx);
                                    }
                                }
                                _ => {}
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
    }

    fn draw(&self, terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
        let remaining =
            self.interval.as_secs() as i64 - self.last_refresh.elapsed().as_secs() as i64;
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
        };
        let lines = ui::build_lines(&frame);
        terminal.draw(|f| {
            f.render_widget(Paragraph::new(lines), f.area());
        })?;
        Ok(())
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
