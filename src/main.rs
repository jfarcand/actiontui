//! actiontui — a Ratatui dashboard for GitHub Actions workflow runs.

mod app;
mod cli;
mod config;
mod github;
mod model;
mod notify;
mod state;
mod ui;

use std::io::IsTerminal;

use anyhow::{Result, bail};
use chrono::Local;
use clap::Parser;

use crate::app::App;
use crate::cli::Cli;
use crate::config::Paths;
use crate::state::State;
use crate::ui::Frame;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let paths = Paths::resolve()?;
    paths.ensure()?;

    let repos = config::resolve_repos(&cli.explicit_repos(), &paths)?;
    let octo = github::build_client()?;
    let sound = !cli.no_sound;

    match cli.watch {
        Some(interval) => {
            if !std::io::stdout().is_terminal() {
                bail!("watch mode needs an interactive terminal — run without -w to print a one-shot table");
            }
            let state = State::load(&paths.state_file);
            let app = App::new(octo, repos, cli.branch, cli.aggregate, sound, interval, state);
            app.run().await
        }
        None => run_once(octo, repos, &cli, &paths, sound).await,
    }
}

/// One-shot snapshot: fetch, notify on transitions, print an ANSI table.
async fn run_once(
    octo: octocrab::Octocrab,
    repos: Vec<String>,
    cli: &Cli,
    paths: &Paths,
    sound: bool,
) -> Result<()> {
    let results = app::fetch_all(&octo, &repos, &cli.branch).await;

    let mut state = State::load(&paths.state_file);
    let transitions = state.diff(&results);
    notify::announce(&transitions, &cli.branch, sound);
    state.commit(&results);

    let frame = Frame {
        results: &results,
        aggregate: cli.aggregate,
        branch: &cli.branch,
        now: Local::now(),
        watch: None,
        spinner: 0,
        loading: false,
    };
    let lines = ui::build_lines(&frame);
    print!("{}", ui::lines_to_ansi(&lines));

    // Non-zero exit if any repo failed to fetch, so scripts can detect it.
    if results.iter().any(|r| r.error.is_some()) {
        std::process::exit(1);
    }
    Ok(())
}
