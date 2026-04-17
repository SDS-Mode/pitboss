use std::path::Path;

use anyhow::{bail, Result};
use tokio::process::Command;

/// Probe the claude CLI for its version string. Returns `None` when the binary
/// exists but the probe output is unparseable — a non-fatal degrade.
/// Returns `Err` when the binary is not executable or not found (fatal).
#[allow(dead_code)]
pub async fn probe_claude(binary: &Path) -> Result<Option<String>> {
    let output = Command::new(binary).arg("--version").output().await;
    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if text.is_empty() { Ok(None) } else { Ok(Some(text)) }
        }
        Ok(o) => {
            tracing::warn!(code = ?o.status.code(), "claude --version exited non-zero; proceeding without version");
            Ok(None)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("claude binary not found at {}", binary.display())
        }
        Err(e) => bail!("failed to probe claude: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn nonexistent_binary_is_fatal() {
        let err = probe_claude(&PathBuf::from("/nope/claude")).await.unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn echo_binary_succeeds_with_unparsed_output() {
        let v = probe_claude(&PathBuf::from("/bin/echo")).await.unwrap();
        assert!(v.is_some());
    }
}
