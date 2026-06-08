// SPDX-License-Identifier: Apache-2.0
//! Domain types shared across fetching, state, and rendering.

use chrono::{DateTime, Utc};

/// Coarse status used for coloring and transition detection. Derived from the
/// GitHub run's `(status, conclusion)` pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Badge {
    Pass,
    Fail,
    Running,
    Queued,
    Pending,
    Cancelled,
    Skipped,
    Other(String),
}

impl Badge {
    /// Map a GitHub `(status, conclusion)` pair to a coarse badge.
    // The two `Other(..)` arms look identical but are distinct: one carries an
    // unknown *conclusion*, the other an unknown *status*.
    #[allow(clippy::match_same_arms)]
    pub fn from_run(status: &str, conclusion: Option<&str>) -> Self {
        match (status, conclusion) {
            ("completed", Some("success")) => Self::Pass,
            ("completed", Some("failure" | "timed_out")) => Self::Fail,
            ("completed", Some("cancelled")) => Self::Cancelled,
            ("completed", Some("skipped")) => Self::Skipped,
            ("completed", Some(other)) => Self::Other(other.to_string()),
            ("in_progress", _) => Self::Running,
            ("queued", _) => Self::Queued,
            ("pending" | "waiting" | "requested", _) => Self::Pending,
            (other, _) => Self::Other(other.to_string()),
        }
    }

    /// Short label rendered in the Status column.
    pub fn label(&self) -> &str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "FAIL",
            Self::Running => "running",
            Self::Queued => "queued",
            Self::Pending => "pending",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
            Self::Other(s) => s,
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Fail)
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Self::Pass)
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Running | Self::Queued | Self::Pending)
    }
}

/// One dot in the "Recent" history column (newest first).
#[derive(Clone, Copy, Debug)]
pub enum Dot {
    Pass,
    Fail,
    Active,
    Other,
}

/// A single workflow's latest state plus derived history.
#[derive(Clone, Debug)]
pub struct WorkflowRow {
    pub workflow_name: String,
    /// The workflow's database id (for fetching its run history).
    pub workflow_id: u64,
    pub badge: Badge,
    pub started_at: Option<DateTime<Utc>>,
    /// Set only when the run has completed.
    pub finished_at: Option<DateTime<Utc>>,
    /// Estimated total run duration (seconds), from the most recent success.
    pub eta_total_secs: Option<i64>,
    /// Head commit SHA of the latest run (the commit that kicked it off).
    pub head_sha: Option<String>,
    /// Database id of the latest run, for re-runs.
    pub run_id: u64,
    /// Last few run results, newest first.
    pub recent: Vec<Dot>,
}

/// Result of fetching one repo. Either rows or an error message.
#[derive(Clone, Debug)]
pub struct RepoResult {
    pub repo: String,
    pub rows: Vec<WorkflowRow>,
    pub error: Option<String>,
}

/// A point-in-time snapshot of a repo's headline metrics.
#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub stars: i64,
    pub forks: i64,
    pub watchers: i64,
    pub issues: i64,
    pub prs: i64,
}

/// Freshly-fetched repo stats (canonical name + current snapshot).
#[derive(Clone, Debug)]
pub struct RepoStats {
    pub repo: String,
    pub snapshot: Snapshot,
    pub error: Option<String>,
}

/// A stats table row: current stats, the prior snapshot (for deltas), and the
/// recorded star history (for the chart).
#[derive(Clone, Debug)]
pub struct StatsRow {
    pub stats: RepoStats,
    pub prev: Option<Snapshot>,
    /// (date, stars) in ascending date order.
    pub trend: Vec<(String, i64)>,
}

/// One GitHub API rate-limit bucket (core, search, graphql, …).
#[derive(Clone, Debug)]
pub struct RateBucket {
    pub name: String,
    pub limit: i64,
    pub used: i64,
    pub remaining: i64,
    pub reset: DateTime<Utc>,
}

/// A rate-limit table row: the bucket plus its used-delta since the last refresh.
#[derive(Clone, Debug)]
pub struct RateRow {
    pub bucket: RateBucket,
    pub delta_used: Option<i64>,
}

/// One run in a workflow's history, for the detail chart.
#[derive(Clone, Debug)]
pub struct RunPoint {
    pub started: DateTime<Utc>,
    /// Wall-clock duration in seconds (0 while still running).
    pub duration_secs: i64,
    pub dot: Dot,
}

/// A workflow's run history over a recent time window (oldest → newest).
#[derive(Clone, Debug)]
pub struct WorkflowDetail {
    pub days: u32,
    pub runs: Vec<RunPoint>,
}
