// SPDX-License-Identifier: Apache-2.0
//! GitHub REST access via a thin `reqwest` client with conditional-request
//! caching. Each GET stores the response `ETag` and replays `If-None-Match` on
//! the next call; an unchanged resource returns `304 Not Modified`, which GitHub
//! does NOT count against the rate limit — so steady-state polling is ~free.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use reqwest::header::{ACCEPT, ETAG, IF_NONE_MATCH};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};
use crate::model::{
    Badge, Dot, RateBucket, RepoResult, RepoStats, RunPoint, Snapshot, WorkflowDetail, WorkflowRow,
};

const RECENT_COUNT: usize = 6;
const RUN_PAGE_SIZE: usize = 100;
const API_BASE: &str = "https://api.github.com";

#[derive(Clone)]
struct CacheEntry {
    etag: String,
    body: Vec<u8>,
}

/// Authenticated GitHub client with per-URL `ETag` caching.
pub struct GhClient {
    http: reqwest::Client,
    token: String,
    etags: Mutex<HashMap<String, CacheEntry>>,
}

impl GhClient {
    /// GET a JSON resource, using a conditional request when we have an `ETag`.
    pub async fn get<T: DeserializeOwned>(&self, route: &str) -> Result<T> {
        let bytes = self.get_bytes(route).await?;
        serde_json::from_slice(&bytes).map_err(|e| Error::GitHub(format!("decoding {route}: {e}")))
    }

    async fn get_bytes(&self, route: &str) -> Result<Vec<u8>> {
        let url = format!("{API_BASE}{route}");
        let cached = self.cache_get(&url);

        let mut req = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(c) = &cached {
            req = req.header(IF_NONE_MATCH, c.etag.clone());
        }

        let resp = req
            .send()
            .await
            .map_err(|e| Error::GitHub(format!("request to {route} failed: {e}")))?;

        // 304 → nothing changed; serve the cached body (free, no rate-limit cost).
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED
            && let Some(c) = cached
        {
            return Ok(c.body);
        }

        let status = resp.status();
        let etag = resp
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| Error::GitHub(format!("reading {route}: {e}")))?
            .to_vec();

        if !status.is_success() {
            let msg: String = String::from_utf8_lossy(&bytes).chars().take(200).collect();
            return Err(Error::GitHub(format!("{route}: {status} {msg}")));
        }
        if let Some(etag) = etag {
            self.cache_put(
                url,
                CacheEntry {
                    etag,
                    body: bytes.clone(),
                },
            );
        }
        Ok(bytes)
    }

    fn cache_get(&self, url: &str) -> Option<CacheEntry> {
        self.etags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(url)
            .cloned()
    }

    fn cache_put(&self, url: String, entry: CacheEntry) {
        self.etags
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(url, entry);
    }
}

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
pub fn build_client() -> Result<GhClient> {
    let token = gh_token()
        .ok_or_else(|| Error::GitHub("no GitHub token found — run `gh auth login`".into()))?;
    let http = reqwest::Client::builder()
        .user_agent("actiontui")
        .build()
        .map_err(|e| Error::GitHub(format!("building HTTP client: {e}")))?;
    Ok(GhClient {
        http,
        token,
        etags: Mutex::new(HashMap::new()),
    })
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
/// one is dropped (so it also can't trigger a notification). `active` is the
/// caller-cached set of non-deleted workflow ids.
pub async fn fetch_repo(
    client: &GhClient,
    repo: &str,
    branch: &str,
    exclude: &[String],
    active: &HashSet<u64>,
) -> RepoResult {
    match fetch_repo_inner(client, repo, branch, exclude, active).await {
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
    client: &GhClient,
    repo: &str,
    branch: &str,
    exclude: &[String],
    active: &HashSet<u64>,
) -> Result<Vec<WorkflowRow>> {
    let route = format!(
        "/repos/{repo}/actions/runs?branch={branch}&per_page={RUN_PAGE_SIZE}",
        branch = urlencode(branch),
    );
    let resp: RunsResponse = client.get(&route).await?;

    // Group runs by workflow, filtering to active workflows when we know them.
    let mut groups: HashMap<u64, Vec<ApiRun>> = HashMap::new();
    for run in resp.workflow_runs {
        if !active.is_empty() && !active.contains(&run.workflow_id) {
            continue;
        }
        groups.entry(run.workflow_id).or_default().push(run);
    }

    let mut rows = Vec::with_capacity(groups.len());
    for (workflow_id, mut group) in groups {
        group.sort_by_key(|r| std::cmp::Reverse(r.started()));
        let latest = &group[0];

        let status = latest.status.as_deref().unwrap_or("unknown");
        let conclusion = latest.conclusion.as_deref();
        let badge = Badge::from_run(status, conclusion);

        rows.push(WorkflowRow {
            workflow_name: latest.name.clone().unwrap_or_else(|| "unknown".into()),
            workflow_id,
            badge,
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
pub async fn fetch_stats(client: &GhClient, repo: &str) -> RepoStats {
    match fetch_stats_inner(client, repo).await {
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

async fn fetch_stats_inner(client: &GhClient, repo: &str) -> Result<(String, Snapshot)> {
    let info: RepoInfo = client.get(&format!("/repos/{repo}")).await?;
    let prs = open_pr_count(client, &info.full_name).await.unwrap_or(0);
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
async fn open_pr_count(client: &GhClient, repo: &str) -> Result<i64> {
    let pulls: Vec<PullStub> = client
        .get(&format!("/repos/{repo}/pulls?state=open&per_page=100"))
        .await?;
    Ok(pulls.len() as i64)
}

/// Fetch a single workflow's run history over the last `days`, for the detail
/// chart. Returns runs oldest → newest.
pub async fn fetch_workflow_detail(
    client: &GhClient,
    repo: &str,
    workflow_id: u64,
    workflow: &str,
    branch: &str,
    days: u32,
) -> Result<WorkflowDetail> {
    let route = format!(
        "/repos/{repo}/actions/workflows/{workflow_id}/runs?branch={branch}&per_page=100",
        branch = urlencode(branch),
    );
    let resp: RunsResponse = client
        .get(&route)
        .await
        .map_err(|e| Error::GitHub(format!("fetching runs for {workflow}: {e}")))?;

    let cutoff = Utc::now() - chrono::Duration::days(i64::from(days));
    let mut runs: Vec<RunPoint> = resp
        .workflow_runs
        .into_iter()
        .filter_map(|r| {
            let started = r.started();
            if started < cutoff {
                return None;
            }
            let status = r.status.as_deref().unwrap_or("");
            let duration_secs = if status == "completed" {
                (r.updated_at - started).num_seconds().max(0)
            } else {
                0
            };
            let dot = match (status, r.conclusion.as_deref()) {
                ("completed", Some("success")) => Dot::Pass,
                ("completed", Some("failure" | "timed_out")) => Dot::Fail,
                ("in_progress" | "queued" | "pending", _) => Dot::Active,
                _ => Dot::Other,
            };
            Some(RunPoint {
                started,
                duration_secs,
                dot,
            })
        })
        .collect();
    runs.sort_by_key(|r| r.started);

    Ok(WorkflowDetail { days, runs })
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
pub async fn fetch_rate(client: &GhClient) -> Result<Vec<RateBucket>> {
    let resp: RateLimitResponse = client.get("/rate_limit").await?;
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

/// Active (non-deleted, non-disabled) workflow ids for a repo. Cached by the
/// caller and refreshed infrequently — this list rarely changes.
pub async fn fetch_active_workflow_ids(client: &GhClient, repo: &str) -> Result<HashSet<u64>> {
    let resp: WorkflowsResponse = client
        .get(&format!("/repos/{repo}/actions/workflows?per_page=100"))
        .await?;
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
