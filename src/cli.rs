//! Command-line interface (mirrors the original `ghactions` flags).

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "actiontui",
    about = "A Ratatui dashboard for GitHub Actions workflow runs",
    long_about = "Watch GitHub Actions workflow runs across one or more repos.\n\n\
        Repos are resolved from -R/positional args, then ~/.config/actiontui/repos.conf,\n\
        then the current git remote. Notifies (with sound) when a workflow turns red or recovers."
)]
pub struct Cli {
    /// Watch mode: live-refreshing TUI. Optional refresh interval in seconds (default 60).
    #[arg(short = 'w', long = "watch", value_name = "SECONDS", num_args = 0..=1, default_missing_value = "60")]
    pub watch: Option<u64>,

    /// Aggregate every repo into a single table.
    #[arg(short = 'a', long = "aggregate")]
    pub aggregate: bool,

    /// Branch to inspect (default: main, or `branch` from config.toml).
    #[arg(short = 'b', long = "branch")]
    pub branch: Option<String>,

    /// Repo to watch (owner/repo). Repeatable.
    #[arg(short = 'R', long = "repo", value_name = "OWNER/REPO")]
    pub repo: Vec<String>,

    /// Disable notification sounds (visual notifications still fire).
    #[arg(long = "no-sound")]
    pub no_sound: bool,

    /// Hide workflows whose name contains this (case-insensitive). Repeatable.
    #[arg(short = 'x', long = "exclude", value_name = "PATTERN")]
    pub exclude: Vec<String>,

    /// Additional repos as positional args (owner/repo).
    #[arg(value_name = "OWNER/REPO")]
    pub repos: Vec<String>,
}

impl Cli {
    /// All explicitly-named repos (flags + positional), in order.
    pub fn explicit_repos(&self) -> Vec<String> {
        self.repo.iter().chain(self.repos.iter()).cloned().collect()
    }
}
