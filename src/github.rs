// SPDX-License-Identifier: Apache-2.0
//! GitHub REST access via octocrab. One page of runs per repo is fetched and
//! everything (latest-per-workflow, recent history, fail-since, ETA) is derived
//! client-side — far fewer API calls than the original per-workflow approach.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use octocrab::Octocrab;
use serde::Deserialize;

use crate::error::{Error, Result};
use crate::model::{Badge, Dot, RateBucket, RepoResult, RepoStats, Snapshot, WorkflowRow};

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
/// (falls back to `GITHUB_TOKEN` / `GH_TOKEN`).
pub fn build_client() -> Result<Octocrab> {
    let token = gh_token().ok_or_else(|| {
        Error::GitHub(
            "no GitHub token found — run `gh auth login`, or set GITHUB_TOKEN/GH_TOKEN".into(),
        )
    })?;
    Octocrab::builder()
        .personal_token(token)
        .build()
        .map_err(|e| Error::GitHub(format!("failed to build GitHub client: {e}")))
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
            error: Some(e.to_string()),
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
        .map_err(|e| Error::GitHub(format!("fetching runs for {repo}: {e}")))?;

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

#[derive(Deserialize)]
struct RepoInfo {
    full_name: String,
    #[serde(default)]
    stargazers_count: i64,
    #[serde(default)]
    forks_count: i64,
    #[serde(default)]
    subscribers_count: i64,
    #[serde(default)]
    open_issues_count: i64,
}

#[derive(Deserialize)]
struct PullStub {}

/// Fetch a repo's headline stats (stars/forks/watchers/issues/PRs).
///
/// `open_issues_count` includes PRs, so we count open PRs separately and
/// subtract to isolate true issues.
pub async fn fetch_stats(octo: &Octocrab, repo: &str) -> RepoStats {
    match fetch_stats_inner(octo, repo).await {
        Ok((canonical, snapshot)) => RepoStats {
            repo: canonical,
            snapshot,
            error: None,
        },
        Err(e) => RepoStats {
            repo: repo.to_string(),
            snapshot: Snapshot::default(),
            error: Some(e.to_string()),
        },
    }
}

async fn fetch_stats_inner(octo: &Octocrab, repo: &str) -> Result<(String, Snapshot)> {
    let info: RepoInfo = octo
        .get(&format!("/repos/{repo}"), None::<&()>)
        .await
        .map_err(|e| Error::GitHub(format!("fetching repo {repo}: {e}")))?;

    let prs = open_pr_count(octo, &info.full_name).await.unwrap_or(0);
    let issues = (info.open_issues_count - prs).max(0);

    let snapshot = Snapshot {
        stars: info.stargazers_count,
        forks: info.forks_count,
        watchers: info.subscribers_count,
        issues,
        prs,
    };
    Ok((info.full_name, snapshot))
}

/// Open PR count via the pulls endpoint (caps at one page of 100 — plenty, and
/// avoids the search API's permission/rate-limit pitfalls on private repos).
async fn open_pr_count(octo: &Octocrab, repo: &str) -> Result<i64> {
    let route = format!("/repos/{repo}/pulls?state=open&per_page=100");
    let pulls: Vec<PullStub> = octo.get(&route, None::<&()>).await?;
    Ok(pulls.len() as i64)
}

#[derive(Deserialize)]
struct RateLimitResponse {
    resources: HashMap<String, ApiRate>,
}

#[derive(Deserialize)]
struct ApiRate {
    limit: i64,
    used: i64,
    remaining: i64,
    reset: i64,
}

/// Fetch all GitHub API rate-limit buckets. The `rate_limit` endpoint is free —
/// it does not count against any bucket — so it's safe to poll.
pub async fn fetch_rate(octo: &Octocrab) -> Result<Vec<RateBucket>> {
    let resp: RateLimitResponse = octo
        .get("/rate_limit", None::<&()>)
        .await
        .map_err(|e| Error::GitHub(format!("fetching rate limit: {e}")))?;
    let mut buckets: Vec<RateBucket> = resp
        .resources
        .into_iter()
        .map(|(name, r)| RateBucket {
            name,
            limit: r.limit,
            used: r.used,
            remaining: r.remaining,
            reset: DateTime::from_timestamp(r.reset, 0).unwrap_or_else(Utc::now),
        })
        .collect();
    buckets.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(buckets)
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
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}
