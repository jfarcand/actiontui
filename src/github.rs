//! GitHub REST access via octocrab. One page of runs per repo is fetched and
//! everything (latest-per-workflow, recent history, fail-since, ETA) is derived
//! client-side — far fewer API calls than the original per-workflow approach.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use octocrab::Octocrab;
use serde::Deserialize;

use crate::model::{Badge, Dot, RepoResult, WorkflowRow};

const RECENT_COUNT: usize = 6;
const RUN_PAGE_SIZE: usize = 100;

#[derive(Deserialize)]
struct RunsResponse {
    workflow_runs: Vec<ApiRun>,
}

#[derive(Deserialize)]
struct ApiRun {
    id: u64,
    #[serde(default)]
    name: Option<String>,
    workflow_id: u64,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    head_sha: String,
    #[serde(default)]
    run_started_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ApiRun {
    fn started(&self) -> DateTime<Utc> {
        self.run_started_at.unwrap_or(self.created_at)
    }
}

#[derive(Deserialize)]
struct WorkflowsResponse {
    workflows: Vec<ApiWorkflow>,
}

#[derive(Deserialize)]
struct ApiWorkflow {
    id: u64,
    state: String,
}

/// Build an authenticated client, pulling the token from `gh auth token`
/// (falls back to GITHUB_TOKEN / GH_TOKEN).
pub fn build_client() -> Result<Octocrab> {
    let token = gh_token()
        .context("no GitHub token found — run `gh auth login`, or set GITHUB_TOKEN/GH_TOKEN")?;
    Octocrab::builder()
        .personal_token(token)
        .build()
        .context("failed to build GitHub client")
}

fn gh_token() -> Option<String> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(v) = std::env::var(var)
            && !v.trim().is_empty()
        {
            return Some(v.trim().to_string());
        }
    }
    let out = std::process::Command::new("gh")
        .args(["auth", "token"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let tok = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tok.is_empty() { None } else { Some(tok) }
}

/// Fetch and derive the workflow rows for a single repo + branch.
///
/// `exclude` holds case-insensitive substrings; any workflow whose name matches
/// one is dropped (so it also can't trigger a notification).
pub async fn fetch_repo(
    octo: &Octocrab,
    repo: &str,
    branch: &str,
    exclude: &[String],
) -> RepoResult {
    match fetch_repo_inner(octo, repo, branch, exclude).await {
        Ok(rows) => RepoResult {
            repo: repo.to_string(),
            rows,
            error: None,
        },
        Err(e) => RepoResult {
            repo: repo.to_string(),
            rows: Vec::new(),
            error: Some(format!("{e:#}")),
        },
    }
}

async fn fetch_repo_inner(
    octo: &Octocrab,
    repo: &str,
    branch: &str,
    exclude: &[String],
) -> Result<Vec<WorkflowRow>> {
    let active = active_workflow_ids(octo, repo).await.unwrap_or_default();

    let runs_route = format!(
        "/repos/{repo}/actions/runs?branch={branch}&per_page={RUN_PAGE_SIZE}",
        branch = urlencode(branch),
    );
    let resp: RunsResponse = octo
        .get(&runs_route, None::<&()>)
        .await
        .with_context(|| format!("fetching runs for {repo}"))?;

    // Group runs by workflow, filtering to active workflows when we know them.
    let mut groups: HashMap<u64, Vec<ApiRun>> = HashMap::new();
    for run in resp.workflow_runs {
        if !active.is_empty() && !active.contains(&run.workflow_id) {
            continue;
        }
        groups.entry(run.workflow_id).or_default().push(run);
    }

    let mut rows = Vec::with_capacity(groups.len());
    for (_id, mut group) in groups {
        // Newest first.
        group.sort_by_key(|r| std::cmp::Reverse(r.started()));
        let latest = &group[0];

        let status = latest.status.as_deref().unwrap_or("unknown");
        let conclusion = latest.conclusion.as_deref();
        let badge = Badge::from_run(status, conclusion);

        rows.push(WorkflowRow {
            workflow_name: latest.name.clone().unwrap_or_else(|| "unknown".into()),
            badge: badge.clone(),
            started_at: Some(latest.started()),
            finished_at: (status == "completed").then_some(latest.updated_at),
            eta_total_secs: estimate_duration(&group),
            head_sha: (!latest.head_sha.is_empty()).then(|| latest.head_sha.clone()),
            run_id: latest.id,
            recent: recent_dots(&group),
        });
    }

    if !exclude.is_empty() {
        let patterns: Vec<String> = exclude.iter().map(|p| p.to_lowercase()).collect();
        rows.retain(|r| {
            let name = r.workflow_name.to_lowercase();
            !patterns.iter().any(|p| name.contains(p))
        });
    }

    rows.sort_by_key(|r| r.workflow_name.to_lowercase());
    Ok(rows)
}

async fn active_workflow_ids(octo: &Octocrab, repo: &str) -> Result<HashSet<u64>> {
    let route = format!("/repos/{repo}/actions/workflows?per_page=100");
    let resp: WorkflowsResponse = octo.get(&route, None::<&()>).await?;
    Ok(resp
        .workflows
        .into_iter()
        .filter(|w| w.state == "active")
        .map(|w| w.id)
        .collect())
}

/// Estimated total duration from the most recent successful run in the group.
fn estimate_duration(group: &[ApiRun]) -> Option<i64> {
    group
        .iter()
        .find(|r| r.conclusion.as_deref() == Some("success"))
        .map(|r| (r.updated_at - r.started()).num_seconds().max(0))
}

fn recent_dots(group: &[ApiRun]) -> Vec<Dot> {
    group
        .iter()
        .take(RECENT_COUNT)
        .map(|r| match (r.status.as_deref(), r.conclusion.as_deref()) {
            (Some("completed"), Some("success")) => Dot::Pass,
            (Some("completed"), Some("failure" | "timed_out")) => Dot::Fail,
            (Some("in_progress" | "queued" | "pending"), _) => Dot::Active,
            _ => Dot::Other,
        })
        .collect()
}

/// Minimal URL-encoding for branch names (handles `/`, spaces, `#`, etc.).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
