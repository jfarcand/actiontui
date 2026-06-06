// SPDX-License-Identifier: MIT OR Apache-2.0
//! Persisted last-known conclusions, for green↔red transition detection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::{Badge, RepoResult};

/// `repo → (workflow_name → "success"|"failure")`.
type Store = HashMap<String, HashMap<String, String>>;

pub struct State {
    path: PathBuf,
    store: Store,
}

/// A detected change worth notifying about.
pub enum Transition {
    /// A workflow went green → red.
    Failure { repo: String, workflow: String },
    /// A workflow went red → green.
    Recovery { repo: String, workflow: String },
}

impl State {
    pub fn load(path: &Path) -> Self {
        let store = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Self {
            path: path.to_path_buf(),
            store,
        }
    }

    /// Compare the freshly-fetched results against stored state, returning the
    /// transitions to notify. Does not mutate; call [`commit`] to persist.
    pub fn diff(&self, results: &[RepoResult]) -> Vec<Transition> {
        let mut transitions = Vec::new();
        for repo in results {
            let prev = self.store.get(&repo.repo);
            for row in &repo.rows {
                let prev_conclusion = prev
                    .and_then(|m| m.get(&row.workflow_name))
                    .map(String::as_str);
                match (&row.badge, prev_conclusion) {
                    (b, Some("success")) if b.is_failure() => {
                        transitions.push(Transition::Failure {
                            repo: repo.repo.clone(),
                            workflow: row.workflow_name.clone(),
                        });
                    }
                    (b, Some("failure")) if b.is_success() => {
                        transitions.push(Transition::Recovery {
                            repo: repo.repo.clone(),
                            workflow: row.workflow_name.clone(),
                        });
                    }
                    _ => {}
                }
            }
        }
        transitions
    }

    /// Fold the latest pass/fail conclusions into the store and write to disk.
    /// Only definitive (pass/fail) results are recorded; in-progress runs leave
    /// the previous conclusion intact so a transition isn't lost mid-run.
    pub fn commit(&mut self, results: &[RepoResult]) {
        for repo in results {
            if repo.error.is_some() {
                continue;
            }
            let entry = self.store.entry(repo.repo.clone()).or_default();
            for row in &repo.rows {
                let value = match row.badge {
                    Badge::Pass => "success",
                    Badge::Fail => "failure",
                    _ => continue,
                };
                entry.insert(row.workflow_name.clone(), value.to_string());
            }
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.store) {
            let _ = std::fs::write(&self.path, json);
        }
    }
}
