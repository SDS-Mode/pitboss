//! `pitboss container-prune` — sweep stale `pitboss-derived-*:local`
//! image tags from the local image store.
//!
//! Each `pitboss container-build` produces a deterministic
//! `pitboss-derived-<sha>:local` tag (see `dispatch/container_build`).
//! Active operators iterating on `[[container.copy]]` contents or
//! `extra_apt` lists accumulate one tag per change. This subcommand
//! cross-references the live tag set against derived tags computed
//! from a caller-supplied manifest list and reports + optionally
//! removes the stragglers.
//!
//! Reference-set semantics: if no manifests are passed, the reference
//! set is empty — every derived tag is "stale" and removable. This is
//! the explicit "I want a clean slate" mode. Passing one or more
//! manifests narrows the removal set to tags NOT computed from any of
//! them.
//!
//! Time-based eviction (`--keep-recent N` etc.) is intentionally
//! deferred — see #267 for the design discussion.
//!
//! The subcommand never touches images outside the
//! `pitboss-derived-*:local` namespace. Operators with mixed local
//! image stores can invoke it without fear of nuking unrelated
//! content.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Context, Result};

/// CLI-driven options. Kept separate from the `<manifest>...`
/// positional list because the list shape changes more often than the
/// flag set.
#[derive(Debug, Clone, Default)]
pub struct PruneOptions {
    /// Apply the removals. Default is dry-run.
    pub apply: bool,
    /// Override the auto-detected runtime ("docker" / "podman").
    pub runtime_override: Option<String>,
}

/// Top-level entry called from `main.rs` for `pitboss container-prune`.
///
/// Resolves the runtime, lists derived tags, computes the reference
/// set from `manifests`, classifies each tag as `active` or `stale`,
/// emits a greppable two-column report on stdout, then (if
/// `opts.apply`) removes the stale ones via `<runtime> image rm`.
pub fn run_container_prune(manifests: &[PathBuf], opts: &PruneOptions) -> Result<()> {
    let runtime =
        crate::dispatch::container::detect_runtime(opts.runtime_override.as_deref(), None)?;

    let live_tags = list_derived_tags(&runtime)?;
    let active_set = compute_reference_set(manifests)?;

    let report = classify_tags(&live_tags, &active_set);
    print_report(&report);

    if !opts.apply {
        let stale_count = report
            .iter()
            .filter(|(_, s)| *s == TagStatus::Stale)
            .count();
        if stale_count > 0 {
            eprintln!("\n{stale_count} stale tag(s) listed above. Re-run with --apply to remove.");
        }
        return Ok(());
    }

    // Apply phase: remove every stale tag, continuing on per-tag
    // failures. `image rm` typically fails when a tag is referenced by
    // a stopped container or a child image — those are recoverable
    // (operator can clean up the user) and shouldn't abort the whole
    // sweep.
    let mut removed = 0usize;
    let mut failed = 0usize;
    for (tag, status) in &report {
        if *status != TagStatus::Stale {
            continue;
        }
        match remove_tag(&runtime, tag) {
            Ok(()) => {
                removed += 1;
                println!("removed {tag}");
            }
            Err(e) => {
                failed += 1;
                eprintln!("failed to remove {tag}: {e}");
            }
        }
    }
    println!(
        "\nsummary: {removed} removed, {failed} failed, {} kept",
        report
            .iter()
            .filter(|(_, s)| *s == TagStatus::Active)
            .count()
    );
    if failed > 0 {
        bail!("{failed} tag(s) could not be removed");
    }
    Ok(())
}

/// Classification of a tag in the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagStatus {
    /// Tag is computed from at least one manifest in the reference set.
    Active,
    /// Tag is not referenced by any manifest in the reference set.
    Stale,
}

impl TagStatus {
    fn label(self) -> &'static str {
        match self {
            TagStatus::Active => "active",
            TagStatus::Stale => "stale",
        }
    }
}

/// Match a single tag string against the `pitboss-derived-<hex>:local`
/// shape, tolerating an optional `localhost/` prefix that podman
/// inserts for locally-built images. Returns `true` iff the tag is
/// pitboss-owned.
pub(crate) fn is_pitboss_derived_tag(tag: &str) -> bool {
    let stripped = tag.strip_prefix("localhost/").unwrap_or(tag);
    let Some(rest) = stripped.strip_prefix("pitboss-derived-") else {
        return false;
    };
    let Some(hex) = rest.strip_suffix(":local") else {
        return false;
    };
    !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// Strip an optional `localhost/` prefix so a podman-listed
/// `localhost/pitboss-derived-abc:local` matches the bare
/// `pitboss-derived-abc:local` returned by `derived_image_tag`.
pub(crate) fn canonicalize_derived_tag(tag: &str) -> &str {
    tag.strip_prefix("localhost/").unwrap_or(tag)
}

/// Run `<runtime> image ls --format '{{.Repository}}:{{.Tag}}'` and
/// filter the output for pitboss-derived tags. Returns the raw tag
/// strings as the runtime emitted them (i.e. with any `localhost/`
/// prefix preserved) so subsequent `image rm` calls can pass them back
/// verbatim.
pub(crate) fn list_derived_tags(runtime: &str) -> Result<Vec<String>> {
    let out = Command::new(runtime)
        .args(["image", "ls", "--format", "{{.Repository}}:{{.Tag}}"])
        .output()
        .with_context(|| format!("invoking {runtime} image ls"))?;
    if !out.status.success() {
        bail!(
            "{runtime} image ls exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| is_pitboss_derived_tag(l))
        .map(String::from)
        .collect())
}

/// Compute the set of derived tags expected by the supplied manifests.
/// A manifest with no derived inputs (no `extra_apt`, no
/// `[[container.copy]]`) contributes nothing — `derived_image_tag`
/// returns `None` for those.
pub(crate) fn compute_reference_set(manifests: &[PathBuf]) -> Result<BTreeSet<String>> {
    let mut set = BTreeSet::new();
    for path in manifests {
        let resolved = crate::manifest::load_manifest_skip_dir_check(path, None)
            .with_context(|| format!("loading manifest {}", path.display()))?;
        let Some(container) = resolved.container.as_ref() else {
            continue;
        };
        if let Some(tag) = crate::dispatch::container_build::derived_image_tag(container)? {
            set.insert(tag);
        }
    }
    Ok(set)
}

/// Build the active/stale status for each live tag against the
/// reference set. Pure — `live_tags` is the raw output (possibly with
/// `localhost/` prefix); the reference set holds canonical tags.
pub(crate) fn classify_tags(
    live_tags: &[String],
    active_set: &BTreeSet<String>,
) -> Vec<(String, TagStatus)> {
    let mut out: Vec<(String, TagStatus)> = live_tags
        .iter()
        .map(|tag| {
            let canon = canonicalize_derived_tag(tag);
            let status = if active_set.contains(canon) {
                TagStatus::Active
            } else {
                TagStatus::Stale
            };
            (tag.clone(), status)
        })
        .collect();
    // Sort active first, then alphabetically — keeps the report stable
    // and groups removable tags together at the bottom for easy visual
    // scan.
    out.sort_by(|a, b| match (a.1, b.1) {
        (TagStatus::Active, TagStatus::Stale) => std::cmp::Ordering::Less,
        (TagStatus::Stale, TagStatus::Active) => std::cmp::Ordering::Greater,
        _ => a.0.cmp(&b.0),
    });
    out
}

/// Two-column report on stdout. Tab-separated for greppability /
/// scriptability — `pitboss container-prune | awk '$1=="stale"'` works.
fn print_report(report: &[(String, TagStatus)]) {
    if report.is_empty() {
        println!("(no pitboss-derived tags found in local image store)");
        return;
    }
    println!("STATUS\tTAG");
    for (tag, status) in report {
        println!("{}\t{tag}", status.label());
    }
}

fn remove_tag(runtime: &str, tag: &str) -> Result<()> {
    let out = Command::new(runtime)
        .args(["image", "rm", tag])
        .output()
        .with_context(|| format!("invoking {runtime} image rm"))?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_bare_pitboss_derived_tag() {
        assert!(is_pitboss_derived_tag("pitboss-derived-abc123:local"));
        assert!(is_pitboss_derived_tag("pitboss-derived-465c3910a5c2:local"));
    }

    #[test]
    fn matches_localhost_prefixed_tag() {
        assert!(is_pitboss_derived_tag(
            "localhost/pitboss-derived-abc123:local"
        ));
    }

    #[test]
    fn rejects_other_tags() {
        for bad in [
            "ghcr.io/sds-mode/pitboss-with-claude:0.9.1",
            "pitboss:local",
            "pitboss-derived-:local",          // empty hex
            "pitboss-derived-NOTHEX:local",    // non-hex
            "pitboss-derived-abc123",          // missing :local
            "pitboss-derived-abc123:latest",   // wrong tag suffix
            "myorg/pitboss-derived-abc:local", // foreign registry
            "",
            ":local",
        ] {
            assert!(
                !is_pitboss_derived_tag(bad),
                "must reject {bad:?} as a pitboss-derived tag"
            );
        }
    }

    #[test]
    fn canonicalize_strips_localhost_prefix_only() {
        assert_eq!(
            canonicalize_derived_tag("localhost/pitboss-derived-x:local"),
            "pitboss-derived-x:local"
        );
        assert_eq!(
            canonicalize_derived_tag("pitboss-derived-x:local"),
            "pitboss-derived-x:local"
        );
        // Non-localhost prefixes stay (rejected upstream by
        // is_pitboss_derived_tag, but canonicalize itself is dumb).
        assert_eq!(canonicalize_derived_tag("ghcr.io/foo:1"), "ghcr.io/foo:1");
    }

    #[test]
    fn classify_marks_referenced_tag_active_and_other_stale() {
        let live = vec![
            "localhost/pitboss-derived-aaaa:local".to_string(),
            "pitboss-derived-bbbb:local".to_string(),
            "pitboss-derived-cccc:local".to_string(),
        ];
        let mut active = BTreeSet::new();
        active.insert("pitboss-derived-aaaa:local".to_string());
        active.insert("pitboss-derived-cccc:local".to_string());

        let report = classify_tags(&live, &active);
        // Active first (sorted alphabetically by tag), then stale.
        let labels: Vec<&str> = report.iter().map(|(_, s)| s.label()).collect();
        assert_eq!(labels, vec!["active", "active", "stale"]);
        let tags: Vec<&str> = report.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            tags,
            vec![
                "localhost/pitboss-derived-aaaa:local",
                "pitboss-derived-cccc:local",
                "pitboss-derived-bbbb:local",
            ]
        );
    }

    #[test]
    fn classify_marks_everything_stale_when_reference_set_empty() {
        let live = vec![
            "pitboss-derived-aaaa:local".to_string(),
            "pitboss-derived-bbbb:local".to_string(),
        ];
        let active = BTreeSet::new();
        let report = classify_tags(&live, &active);
        assert!(
            report.iter().all(|(_, s)| *s == TagStatus::Stale),
            "empty reference set must mark all tags stale: {report:?}"
        );
    }

    #[test]
    fn compute_reference_set_skips_manifests_without_derived_inputs() {
        // Manifest with [container] but no extra_apt / copy → derived_image_tag
        // returns None → contributes nothing to the reference set.
        let dir = tempfile::tempdir().unwrap();
        let stock_path = dir.path().join("stock.toml");
        std::fs::write(
            &stock_path,
            r#"
[run]
name = "stock"
[container]
image = "ghcr.io/sds-mode/pitboss-with-claude:0.9.1"
[[task]]
id = "t"
directory = "/home/pitboss"
prompt = "p"
"#,
        )
        .unwrap();
        let set = compute_reference_set(&[stock_path]).unwrap();
        assert!(set.is_empty(), "stock-image manifest contributes no tags");
    }

    #[test]
    fn compute_reference_set_collects_derived_tag_for_extra_apt_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apt.toml");
        std::fs::write(
            &path,
            r#"
[run]
name = "apt"
[container]
image = "ghcr.io/sds-mode/pitboss-with-claude:0.9.1"
extra_apt = ["mdbook"]
[[task]]
id = "t"
directory = "/home/pitboss"
prompt = "p"
"#,
        )
        .unwrap();
        let set = compute_reference_set(&[path]).unwrap();
        assert_eq!(set.len(), 1, "exactly one derived tag expected: {set:?}");
        let tag = set.iter().next().unwrap();
        assert!(
            tag.starts_with("pitboss-derived-") && tag.ends_with(":local"),
            "tag shape: {tag}"
        );
    }
}
