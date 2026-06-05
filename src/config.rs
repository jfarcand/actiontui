//! Config + repo resolution: flags/args → repos.conf → current git remote.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

pub struct Paths {
    pub config_dir: PathBuf,
    pub state_file: PathBuf,
    pub repos_conf: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Paths> {
        let base = dirs::config_dir()
            .context("could not determine config directory")?
            .join("actiontui");
        Ok(Paths {
            state_file: base.join("state.json"),
            repos_conf: base.join("repos.conf"),
            config_dir: base,
        })
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("creating {}", self.config_dir.display()))
    }
}

/// Resolve the list of `owner/repo` strings to watch.
///
/// Precedence: explicit (flags + positional) → repos.conf → current git remote.
pub fn resolve_repos(explicit: &[String], paths: &Paths) -> Result<Vec<String>> {
    if !explicit.is_empty() {
        return Ok(dedup(explicit.to_vec()));
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
        "no repos specified — pass `-R owner/repo`, add them to {}, or run inside a GitHub repo",
        paths.repos_conf.display()
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
    repos.into_iter().filter(|r| seen.insert(r.clone())).collect()
}
