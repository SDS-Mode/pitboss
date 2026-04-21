#![allow(dead_code)]

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::resolve::{resolve, resolve_single_lead, ResolvedManifest};
use super::schema::{Manifest, SingleLeadManifest};
use super::validate::validate;

/// Load, parse, resolve, and validate a manifest from disk.
///
/// Supports two TOML shapes:
/// - `[[lead]]` / `[[task]]` array form (`Manifest`) — the v0.3–v0.5 format.
/// - `[lead]` single-table form (`SingleLeadManifest`) — the v0.6 depth-2
///   convenience format.  Tried as a fallback when the primary parse fails.
pub fn load_manifest(path: &Path, env_max_parallel: Option<u32>) -> Result<ResolvedManifest> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading manifest at {}", path.display()))?;

    // Try the primary `[[lead]]`/`[[task]]` array format first.
    match toml::from_str::<Manifest>(&text) {
        Ok(mut manifest) => {
            expand_paths(&mut manifest)?;
            let resolved = resolve(manifest, env_max_parallel)?;
            validate(&resolved)?;
            Ok(resolved)
        }
        Err(primary_err) => {
            // Fall back to the v0.6 single-table `[lead]` format ONLY when the
            // error is the characteristic "map, expected a sequence" mismatch
            // that occurs when the author writes `[lead]` instead of `[[lead]]`.
            // All other errors (unknown keys, missing required fields, etc.) are
            // reported as-is so they don't silently produce surprising results.
            let err_str = primary_err.to_string();
            let is_lead_type_mismatch =
                err_str.contains("expected a sequence") || err_str.contains("invalid type: map");
            if !is_lead_type_mismatch {
                return Err(primary_err)
                    .with_context(|| format!("parsing manifest at {}", path.display()));
            }
            let single: SingleLeadManifest = toml::from_str(&text).with_context(|| {
                format!(
                    "parsing manifest at {} as single-lead format (primary error: {primary_err})",
                    path.display()
                )
            })?;
            let resolved = resolve_single_lead(single, env_max_parallel)?;
            // The full validate() pipeline assumes a real git work-tree and a
            // populated lead id, neither of which the single-lead form
            // guarantees (it uses CWD + an empty id sentinel). Skip the
            // structure-shape checks but run the manifest-shape checks that
            // are universally applicable — currently just the sublead-defaults
            // adequacy check, which catches the
            // "allow_subleads = true with no fallback" footgun that would
            // otherwise blow up at the first spawn_sublead call.
            //
            // Add similar shape-only checks here as they get factored out of
            // validate() — the goal is parity between the two manifest forms
            // for everything that doesn't require real on-disk state.
            super::validate::validate_sublead_defaults_adequate(&resolved)?;
            Ok(resolved)
        }
    }
}

/// Load, parse, and resolve a v0.6 single-table `[lead]` manifest from a TOML
/// string. Skips `validate` so callers (integration tests) can work without
/// real git work-trees and real filesystem directories.
///
/// Use this when testing manifest parsing/resolution in isolation. For
/// production use, prefer `load_manifest` which also validates the result.
pub fn load_manifest_from_str(toml_src: &str) -> Result<ResolvedManifest> {
    let manifest: SingleLeadManifest =
        toml::from_str(toml_src).with_context(|| "parsing single-lead manifest from string")?;
    resolve_single_lead(manifest, None)
}

fn expand_paths(m: &mut Manifest) -> Result<()> {
    for t in &mut m.tasks {
        t.directory = expand(&t.directory)?;
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
