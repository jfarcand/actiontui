// SPDX-License-Identifier: Apache-2.0
//! Structured error type — replaces `anyhow` (per the clippy `disallowed-methods`
//! policy). Foreign errors convert in via `From`, so `?` still works.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// Configuration / settings resolution failure.
    Config(String),
    /// GitHub API or client failure.
    GitHub(String),
    /// Stats database (`SQLite`) failure.
    Db(String),
    /// Filesystem / terminal I/O failure.
    Io(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(m) => write!(f, "config: {m}"),
            Self::GitHub(m) => write!(f, "github: {m}"),
            Self::Db(m) => write!(f, "stats db: {m}"),
            Self::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Self::GitHub(e.to_string())
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e.to_string())
    }
}

impl From<toml::de::Error> for Error {
    fn from(e: toml::de::Error) -> Self {
        Self::Config(e.to_string())
    }
}
