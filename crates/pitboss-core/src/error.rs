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

    #[error("incomplete run: {0}")]
    Incomplete(String),
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
