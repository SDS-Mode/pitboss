//! Helpers for shaping the finalize-time `RunSummary`. Lives outside
//! `runner.rs` and `hierarchical.rs` so both finalize paths agree on
//! the same logic — historical drift between them is what created
//! #221, and #227 is a smaller version of the same problem (each
//! finalize path independently set `manifest_name = resolved.name`,
//! which left consumers split when no `[run].name` was declared).

use std::path::Path;

/// Resolve the run's display name. Prefers the operator-declared
/// `[run].name`; falls back to the manifest filename's stem when that
/// field is omitted, so the runs index, the run-detail card, and
/// `/api/insights/runs` all show the same string for the same run.
///
/// Pre-fix (#227), `manifest_name` was set to `resolved.name.clone()`
/// directly; without `[run].name` the field landed in `summary.json`
/// as `null`, the runs list synthesized `<unnamed>`, and the insights
/// aggregator independently re-derived from the manifest filename.
/// Three different surfaces showed three different names for the same
/// run.
///
/// `manifest_path` is the absolute (or run-relative) path that the
/// dispatcher was invoked with; we only use its filename stem.
#[must_use]
pub fn resolve_manifest_display_name(
    declared: Option<&str>,
    manifest_path: &Path,
) -> Option<String> {
    if let Some(name) = declared {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    manifest_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_name_wins_over_filename() {
        let n = resolve_manifest_display_name(Some("nightly"), Path::new("/x/something.toml"));
        assert_eq!(n.as_deref(), Some("nightly"));
    }

    #[test]
    fn falls_back_to_filename_stem_when_declared_is_none() {
        let n = resolve_manifest_display_name(None, Path::new("/runs/smoke-test.toml"));
        assert_eq!(n.as_deref(), Some("smoke-test"));
    }

    #[test]
    fn falls_back_to_filename_stem_when_declared_is_empty() {
        let n = resolve_manifest_display_name(Some("   "), Path::new("/runs/nightly.toml"));
        assert_eq!(n.as_deref(), Some("nightly"));
    }

    #[test]
    fn returns_none_when_path_has_no_stem() {
        // `/`'s file_stem is None.
        assert_eq!(resolve_manifest_display_name(None, Path::new("/")), None);
    }

    #[test]
    fn handles_path_without_extension() {
        let n = resolve_manifest_display_name(None, Path::new("Pitboss"));
        assert_eq!(n.as_deref(), Some("Pitboss"));
    }

    #[test]
    fn trims_whitespace_around_declared_name() {
        let n = resolve_manifest_display_name(Some("  named  "), Path::new("/x.toml"));
        assert_eq!(n.as_deref(), Some("named"));
    }
}
