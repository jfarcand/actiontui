// SPDX-License-Identifier: MIT
//! Config + settings resolution.
//!
//! Effective settings come from three layers, highest priority first:
//!   1. CLI flags
//!   2. `~/.config/actiontui/config.toml`
//!   3. built-in defaults (and, for repos, `repos.conf` / the git remote)

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::cli::Cli;

pub struct Paths {
    pub config_dir: PathBuf,
    pub state_file: PathBuf,
    pub repos_conf: PathBuf,
    pub config_toml: PathBuf,
    pub stats_db: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Paths> {
        // Use XDG `~/.config` (matching the original tool and CLI convention),
        // not macOS's `~/Library/Application Support`.
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .context("could not determine config directory")?;
        let base = config_home.join("actiontui");
        Ok(Paths {
            state_file: base.join("state.json"),
            repos_conf: base.join("repos.conf"),
            config_toml: base.join("config.toml"),
            stats_db: base.join("stats.db"),
            config_dir: base,
        })
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("creating {}", self.config_dir.display()))
    }
}

/// `~/.config/actiontui/config.toml`.
#[derive(Deserialize, Default, Debug)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    #[serde(default)]
    pub repos: Vec<String>,
    pub branch: Option<String>,
    pub aggregate: Option<bool>,
    /// Enable watch mode by default.
    pub watch: Option<bool>,
    /// Watch refresh interval in seconds.
    pub interval: Option<u64>,
    pub sound: Option<bool>,
    /// Hide workflows whose name contains any of these (case-insensitive).
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl FileConfig {
    pub fn load(path: &Path) -> Result<FileConfig> {
        if !path.exists() {
            return Ok(FileConfig::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

/// Fully-resolved runtime settings.
pub struct Settings {
    pub repos: Vec<String>,
    pub branch: String,
    pub aggregate: bool,
    /// `Some(interval_secs)` enables watch mode.
    pub watch: Option<u64>,
    pub sound: bool,
    /// Case-insensitive substrings; workflows matching any are hidden.
    pub exclude: Vec<String>,
    /// Start the TUI in the Stats view.
    pub start_stats: bool,
}

impl Settings {
    pub fn resolve(cli: &Cli, file: &FileConfig, paths: &Paths) -> Result<Settings> {
        let repos = resolve_repos(&cli.explicit_repos(), file, paths)?;

        let branch = cli
            .branch
            .clone()
            .or_else(|| file.branch.clone())
            .unwrap_or_else(|| "main".to_string());

        let aggregate = cli.aggregate || file.aggregate.unwrap_or(false);

        // -w on the CLI wins; otherwise honor `watch = true` in config.
        // --stats implies the TUI, so default an interval if none is set.
        let watch = cli
            .watch
            .or_else(|| {
                file.watch
                    .unwrap_or(false)
                    .then(|| file.interval.unwrap_or(60))
            })
            .or_else(|| cli.stats.then(|| file.interval.unwrap_or(60)));

        // --no-sound forces off; otherwise config, defaulting to on.
        let sound = !cli.no_sound && file.sound.unwrap_or(true);

        let exclude = cli
            .exclude
            .iter()
            .chain(file.exclude.iter())
            .cloned()
            .collect();

        Ok(Settings {
            repos,
            branch,
            aggregate,
            watch,
            sound,
            exclude,
            start_stats: cli.stats,
        })
    }
}

/// Resolve the list of `owner/repo` strings to watch.
///
/// Precedence: explicit (CLI) → config.toml `repos` → repos.conf → git remote.
fn resolve_repos(explicit: &[String], file: &FileConfig, paths: &Paths) -> Result<Vec<String>> {
    if !explicit.is_empty() {
        return Ok(dedup(explicit.to_vec()));
    }
    if !file.repos.is_empty() {
        return Ok(dedup(file.repos.clone()));
    }
    if paths.repos_conf.exists() {
        let text = std::fs::read_to_string(&paths.repos_conf)
            .with_context(|| format!("reading {}", paths.repos_conf.display()))?;
        let repos: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect();
        if !repos.is_empty() {
            return Ok(dedup(repos));
        }
    }
    if let Some(repo) = git_remote_repo() {
        return Ok(vec![repo]);
    }
    bail!(
        "no repos specified — pass `-R owner/repo`, list them under `repos` in {}, or run inside a GitHub repo",
        paths.config_toml.display()
    );
}

/// Parse `owner/repo` out of the origin remote URL of the current directory.
fn git_remote_repo() -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    parse_github_slug(&url)
}

/// Extract `owner/repo` from an ssh or https GitHub remote URL.
fn parse_github_slug(url: &str) -> Option<String> {
    let after = url.split("github.com").nth(1)?;
    let slug = after.trim_start_matches([':', '/']).trim_end_matches('/');
    let slug = slug.strip_suffix(".git").unwrap_or(slug);
    if slug.contains('/') && !slug.is_empty() {
        Some(slug.to_string())
    } else {
        None
    }
}

fn dedup(repos: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    repos
        .into_iter()
        .filter(|r| seen.insert(r.clone()))
        .collect()
}
