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
    pub fn from_run(status: &str, conclusion: Option<&str>) -> Badge {
        match (status, conclusion) {
            ("completed", Some("success")) => Badge::Pass,
            ("completed", Some("failure")) => Badge::Fail,
            ("completed", Some("cancelled")) => Badge::Cancelled,
            ("completed", Some("skipped")) => Badge::Skipped,
            ("completed", Some("timed_out")) => Badge::Fail,
            ("completed", Some(other)) => Badge::Other(other.to_string()),
            ("in_progress", _) => Badge::Running,
            ("queued", _) => Badge::Queued,
            ("pending", _) | ("waiting", _) | ("requested", _) => Badge::Pending,
            (other, _) => Badge::Other(other.to_string()),
        }
    }

    /// Short label rendered in the Status column.
    pub fn label(&self) -> &str {
        match self {
            Badge::Pass => "pass",
            Badge::Fail => "FAIL",
            Badge::Running => "running",
            Badge::Queued => "queued",
            Badge::Pending => "pending",
            Badge::Cancelled => "cancelled",
            Badge::Skipped => "skipped",
            Badge::Other(s) => s,
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Badge::Fail)
    }

    pub fn is_success(&self) -> bool {
        matches!(self, Badge::Pass)
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Badge::Running | Badge::Queued | Badge::Pending)
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
