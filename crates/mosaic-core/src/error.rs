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

// Convenience display of first 80 chars of raw line for diagnostics. Will be
// wired into error rendering in a later task.
#[allow(dead_code)]
pub(crate) fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
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
