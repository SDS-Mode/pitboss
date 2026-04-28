#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("malformed stream-json line: {reason}")]
    Malformed { reason: String, raw: String },
}

impl ParseError {
    pub(crate) fn malformed(reason: impl Into<String>, raw: impl Into<String>) -> Self {
        Self::Malformed {
            reason: reason.into(),
            raw: raw.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    #[error("binary not found: {path}")]
    BinaryNotFound { path: String },

    #[error("io error during spawn: {0}")]
    Io(#[from] std::io::Error),

    #[error("spawn rejected: {reason}")]
    Rejected { reason: String },
}

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum SessionError {
    #[error("spawn failed: {0}")]
    Spawn(#[from] SpawnError),

    #[error("io during session: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("run not found: {0}")]
    NotFound(uuid::Uuid),

    /// Catch-all for store inconsistencies — partial writes, missing
    /// expected rows, schema-state mismatches. Kept as the existing
    /// catch-all so call sites that don't yet route through the typed
    /// variants below still compile. New code should prefer the typed
    /// variants.
    #[error("incomplete run: {0}")]
    Incomplete(String),

    /// `SQLite` reported `SQLITE_BUSY` / `SQLITE_LOCKED` — another writer is
    /// holding the database. Distinct from `Incomplete` because callers
    /// (especially the read-side TUI / web console) can retry on this
    /// without surfacing a hard error to the operator. (#149 L9)
    #[error("database busy/locked: {0}")]
    Busy(String),

    /// Schema is corrupt or unrecognised. The store's expected layout was
    /// not found at the cited identifier — a hand-edited DB or a
    /// half-applied migration. Distinct from `Incomplete` because the only
    /// remediation is operator intervention; retrying won't help. (#149 L9)
    #[error("schema corrupt or unrecognised: {0}")]
    Schema(String),

    /// Underlying rusqlite error that didn't fit the busy / schema buckets
    /// — IO from the `SQLite` layer, prepared-statement type mismatch, etc.
    /// Carries the source error so callers can downcast for diagnostic
    /// reporting. (#149 L9)
    #[error("sqlite: {0}")]
    Sqlite(String),
}

impl StoreError {
    /// Classify a `rusqlite::Error` into the most specific `StoreError`
    /// variant available. Lets sqlite call sites do
    /// `.map_err(StoreError::from_rusqlite)` instead of formatting the
    /// error into a generic `Incomplete(String)`. (#149 L9)
    ///
    /// Mapping:
    /// * `SqliteFailure(SqliteError { code: SQLITE_BUSY | SQLITE_LOCKED, .. }, _)`
    ///   → [`StoreError::Busy`]
    /// * `SqliteFailure(_, _)` with a SQL-state-style message hinting at
    ///   schema mismatch (`no such column`, `no such table`) →
    ///   [`StoreError::Schema`]
    /// * Anything else → [`StoreError::Sqlite`]
    #[must_use]
    pub fn from_rusqlite(err: &rusqlite::Error) -> Self {
        let msg = err.to_string();
        if let rusqlite::Error::SqliteFailure(code, _) = err {
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) {
                return StoreError::Busy(msg);
            }
        }
        if msg.contains("no such column")
            || msg.contains("no such table")
            || msg.contains("syntax error")
        {
            return StoreError::Schema(msg);
        }
        StoreError::Sqlite(msg)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("not inside a git work-tree: {path}")]
    NotInRepo { path: String },

    #[error("branch already checked out in another worktree: {branch}")]
    BranchConflict { branch: String },

    #[error("git error: {0}")]
    Git(#[from] git2::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
