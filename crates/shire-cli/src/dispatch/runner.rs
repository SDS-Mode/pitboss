use anyhow::Result;
use std::path::PathBuf;

use crate::manifest::resolve::ResolvedManifest;

#[allow(dead_code)]
pub async fn run_dispatch_inner(
    _resolved: ResolvedManifest,
    _claude_binary: PathBuf,
    _run_dir_override: Option<PathBuf>,
    _dry_run: bool,
) -> Result<i32> {
    anyhow::bail!("dispatch runner — Task 31+")
}
