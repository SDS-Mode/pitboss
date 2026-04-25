#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::resolve::{resolve, ResolvedManifest};
use super::schema::Manifest;
use super::validate::{translate_legacy_parse_error, validate, validate_skip_dir_check};

/// Load, parse, resolve, and validate a manifest from disk.
///
/// v0.9: a single canonical TOML shape (`[lead]` single-table for hierarchical
/// mode; `[[task]]` array for flat mode). The v0.8 `[[lead]]` array form is
/// gone; manifests using it are rejected with a migration message.
pub fn load_manifest(path: &Path, env_max_parallel_tasks: Option<u32>) -> Result<ResolvedManifest> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading manifest at {}", path.display()))?;
    let mut manifest: Manifest = toml::from_str(&text).map_err(|e| {
        translate_legacy_parse_error(&e, &text)
            .unwrap_or_else(|| anyhow::anyhow!("parsing manifest at {}: {e}", path.display()))
    })?;
    expand_paths(&mut manifest)?;
    let resolved = resolve(manifest, env_max_parallel_tasks)?;
    validate(&resolved)?;
    Ok(resolved)
}

/// Like `load_manifest` but skips the directory-existence check.
/// Used by `pitboss container-dispatch` where task/lead `directory` fields
/// are container-side paths that don't exist on the host.
pub fn load_manifest_skip_dir_check(
    path: &Path,
    env_max_parallel_tasks: Option<u32>,
) -> Result<ResolvedManifest> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading manifest at {}", path.display()))?;
    let mut manifest: Manifest = toml::from_str(&text).map_err(|e| {
        translate_legacy_parse_error(&e, &text)
            .unwrap_or_else(|| anyhow::anyhow!("parsing manifest at {}: {e}", path.display()))
    })?;
    expand_paths(&mut manifest)?;
    let resolved = resolve(manifest, env_max_parallel_tasks)?;
    validate_skip_dir_check(&resolved)?;
    Ok(resolved)
}

/// Load, parse, and resolve a manifest from a TOML string. Skips `validate`
/// so callers (integration tests) can work without real git work-trees and
/// real filesystem directories.
///
/// Use this when testing manifest parsing/resolution in isolation. For
/// production use, prefer `load_manifest` which also validates the result.
pub fn load_manifest_from_str(toml_src: &str) -> Result<ResolvedManifest> {
    let manifest: Manifest = toml::from_str(toml_src).map_err(|e| {
        translate_legacy_parse_error(&e, toml_src)
            .unwrap_or_else(|| anyhow::anyhow!("parsing manifest from string: {e}"))
    })?;
    resolve(manifest, None)
}

fn expand_paths(m: &mut Manifest) -> Result<()> {
    for t in &mut m.tasks {
        t.directory = expand(&t.directory)?;
    }
    if let Some(l) = &mut m.lead {
        l.directory = expand(&l.directory)?;
    }
    if let Some(dir) = &m.run.run_dir {
        m.run.run_dir = Some(expand(dir)?);
    }
    Ok(())
}

fn expand(p: &Path) -> Result<PathBuf> {
    let s = p.to_string_lossy();
    let expanded = shellexpand::full(&s).with_context(|| format!("expanding path {s}"))?;
    Ok(PathBuf::from(expanded.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    #[test]
    fn loads_valid_manifest_from_disk() {
        let dir = TempDir::new().unwrap();
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@t.x"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        std::fs::write(dir.path().join("r"), "").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "i"])
            .current_dir(dir.path())
            .status()
            .unwrap();

        let manifest = dir.path().join("pitboss.toml");
        std::fs::write(
            &manifest,
            format!(
                r#"
[[task]]
id = "only"
directory = "{}"
prompt = "hi"
"#,
                dir.path().display()
            ),
        )
        .unwrap();

        let r = load_manifest(&manifest, None).unwrap();
        assert_eq!(r.tasks[0].id, "only");
    }
}
