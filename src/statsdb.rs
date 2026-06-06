// SPDX-License-Identifier: Apache-2.0
//! `SQLite` persistence for repo stats — one row per (repo, UTC date), so we
//! accumulate long-term history GitHub itself doesn't keep (no stars/forks
//! history API; traffic only spans 14 days).

use std::path::Path;

use rusqlite::Connection;

use crate::error::{Error, Result};
use crate::model::Snapshot;

pub struct StatsDb {
    conn: Connection,
}

impl StatsDb {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .map_err(|e| Error::Db(format!("opening stats db at {}: {e}", path.display())))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS stats (
                 repo     TEXT NOT NULL,
                 date     TEXT NOT NULL,
                 stars    INTEGER NOT NULL,
                 forks    INTEGER NOT NULL,
                 watchers INTEGER NOT NULL,
                 issues   INTEGER NOT NULL,
                 prs      INTEGER NOT NULL,
                 PRIMARY KEY (repo, date)
             );",
        )
        .map_err(|e| Error::Db(format!("initializing stats schema: {e}")))?;
        Ok(Self { conn })
    }

    /// Upsert today's snapshot for a repo.
    pub fn record(&self, repo: &str, date: &str, s: &Snapshot) -> Result<()> {
        self.conn.execute(
            "INSERT INTO stats (repo, date, stars, forks, watchers, issues, prs)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(repo, date) DO UPDATE SET
                 stars=?3, forks=?4, watchers=?5, issues=?6, prs=?7",
            rusqlite::params![repo, date, s.stars, s.forks, s.watchers, s.issues, s.prs],
        )?;
        Ok(())
    }

    /// The most recent snapshot strictly before `date` (for day-over-day deltas).
    pub fn previous(&self, repo: &str, date: &str) -> Result<Option<Snapshot>> {
        let mut stmt = self.conn.prepare(
            "SELECT stars, forks, watchers, issues, prs FROM stats
             WHERE repo = ?1 AND date < ?2
             ORDER BY date DESC LIMIT 1",
        )?;
        let snap = stmt
            .query_row(rusqlite::params![repo, date], |r| {
                Ok(Snapshot {
                    stars: r.get(0)?,
                    forks: r.get(1)?,
                    watchers: r.get(2)?,
                    issues: r.get(3)?,
                    prs: r.get(4)?,
                })
            })
            .ok();
        Ok(snap)
    }

    /// (date, stars) for a repo, oldest→newest, capped to the last `limit` days.
    pub fn star_history(&self, repo: &str, limit: usize) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT date, stars FROM (
                 SELECT date, stars FROM stats WHERE repo = ?1
                 ORDER BY date DESC LIMIT ?2
             ) ORDER BY date ASC",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![repo, limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::Path;

    fn snap(stars: i64) -> Snapshot {
        Snapshot {
            stars,
            forks: 1,
            watchers: 2,
            issues: 3,
            prs: 0,
        }
    }

    #[test]
    fn delta_and_history() {
        let db = StatsDb::open(Path::new(":memory:")).unwrap();
        let r = "owner/repo";
        db.record(r, "2026-06-01", &snap(10)).unwrap();
        db.record(r, "2026-06-02", &snap(13)).unwrap();
        // Re-recording the same day is an upsert, not a duplicate.
        db.record(r, "2026-06-02", &snap(14)).unwrap();

        let prev = db.previous(r, "2026-06-02").unwrap().unwrap();
        assert_eq!(prev.stars, 10, "delta baseline is the prior day, not today");
        assert!(db.previous(r, "2026-06-01").unwrap().is_none());

        let hist = db.star_history(r, 10).unwrap();
        assert_eq!(
            hist,
            vec![
                ("2026-06-01".to_string(), 10),
                ("2026-06-02".to_string(), 14)
            ]
        );
    }
}
