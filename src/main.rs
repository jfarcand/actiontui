// SPDX-License-Identifier: Apache-2.0
//! actiontui — a Ratatui dashboard for GitHub Actions workflow runs.

mod app;
mod cli;
mod config;
mod error;
mod github;
mod model;
mod notify;
mod state;
mod statsdb;
mod ui;

use std::io::IsTerminal;

use chrono::Local;
use clap::Parser;

use crate::app::App;
use crate::cli::Cli;
use crate::config::{FileConfig, Paths, Settings};
use crate::error::{Error, Result};
use crate::state::State;
use crate::ui::Frame;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let paths = Paths::resolve()?;
    paths.ensure()?;

    let file = FileConfig::load(&paths.config_toml)?;

    // --test-notify is standalone: fire a sample alert and exit (no repos/token needed).
    if cli.test_notify {
        let sound = !cli.no_sound && file.sound.unwrap_or(true);
        notify::test(sound);
        return Ok(());
    }

    let settings = Settings::resolve(&cli, &file, &paths)?;
    let octo = github::build_client()?;

    match settings.watch {
        Some(interval) => {
            if !std::io::stdout().is_terminal() {
                return Err(Error::Config(
                    "watch mode needs an interactive terminal — run without -w to print a one-shot table".into(),
                ));
            }
            let state = State::load(&paths.state_file);
            let statsdb = statsdb::StatsDb::open(&paths.stats_db)?;
            let app = App::new(
                octo,
                settings.repos,
                settings.branch,
                settings.aggregate,
                settings.sound,
                settings.exclude,
                interval,
                state,
                statsdb,
                settings.start_stats,
            );
            app.run().await
        }
        None => run_once(octo, &settings, &paths).await,
    }
}

/// One-shot snapshot: fetch, notify on transitions, print an ANSI table.
async fn run_once(octo: octocrab::Octocrab, settings: &Settings, paths: &Paths) -> Result<()> {
    let results = app::fetch_all(&octo, &settings.repos, &settings.branch, &settings.exclude).await;

    let mut state = State::load(&paths.state_file);
    let transitions = state.diff(&results);
    notify::announce(&transitions, &settings.branch, settings.sound);
    state.commit(&results);

    let frame = Frame {
        results: &results,
        aggregate: settings.aggregate,
        branch: &settings.branch,
        now: Local::now(),
        watch: None,
        spinner: 0,
        loading: false,
        hyperlinks: true,
        selected: None,
        prompt: None,
    };
    let lines = ui::build_lines(&frame);
    print!("{}", ui::lines_to_ansi(&lines));

    // Non-zero exit if any repo failed to fetch, so scripts can detect it.
    if results.iter().any(|r| r.error.is_some()) {
        std::process::exit(1);
    }
    Ok(())
}
