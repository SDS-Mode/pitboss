//! `pitboss container-build` — synthesize a thin Dockerfile from the
//! manifest's `[container]` section, build a derived image tagged
//! deterministically, and skip the rebuild when the same tag already
//! exists locally.
//!
//! The derived tag is `pitboss-derived-<sha>:local`, where `<sha>` is
//! a SHA-256 over the build inputs that materially affect the image
//! contents — base image, sorted `extra_apt`, sorted `[[container.copy]]`
//! pairs (host bytes hashed in, not just the path). This means:
//!
//! - Re-running `container-build` with the same manifest is a no-op once
//!   the tag exists (idempotent fast path).
//! - `container-dispatch` can compute the same tag and pick it up
//!   without state coordination — see `dispatch/container.rs`.
//! - Mutating a copied host file rewrites the tag, so the rebuild is
//!   automatic on next build.
//!
//! Caching strategy: we use `--layers` for local layer reuse. The
//! `--cache-from` / `--cache-to` flags are remote-cache features (per
//! podman docs) and explicitly out of scope for Phase 2. Their slot is
//! reserved for a future remote-cache work item.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::manifest::schema::{ContainerConfig, CopySpec};

/// Default base image when `[container].image` is unset. Mirrors the
/// constant in `dispatch/container.rs`; kept duplicated rather than
/// re-exported to keep the module-graph DAG simple.
const DEFAULT_BASE_IMAGE: &str = "ghcr.io/sds-mode/pitboss-with-claude:latest";

/// Options bag for `run_container_build` — separate from
/// `ContainerConfig` because these are CLI-driven and don't belong in
/// the manifest schema.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// Force a rebuild even when the derived tag already exists.
    pub no_cache: bool,
    /// Print the synthesized Dockerfile to stdout and exit. Skips
    /// staging and the actual build.
    pub print_dockerfile: bool,
    /// Print the assembled `<runtime> build …` command and exit.
    pub dry_run: bool,
    /// Override the auto-detected runtime ("docker" / "podman").
    pub runtime_override: Option<String>,
}

/// Compute the deterministic derived-image tag for the given container
/// configuration. Same inputs always yield the same tag; any change to
/// the base image, `extra_apt`, or `[[container.copy]]` (including
/// host-file contents) flips it.
///
/// Returns `None` when the configuration declares no derived-image
/// inputs (no `extra_apt` and no `copy` entries). In that case the
/// operator's stock `[container].image` is used directly and there's
/// nothing to build.
pub fn derived_image_tag(container: &ContainerConfig) -> Result<Option<String>> {
    if container.extra_apt.is_empty() && container.copy.is_empty() {
        return Ok(None);
    }

    let base = container.image.as_deref().unwrap_or(DEFAULT_BASE_IMAGE);

    // Tag-scheme version. Bump to v2 when the set of hashed inputs
    // changes (e.g. adding a new field that materially affects the
    // derived image), so existing `pitboss-derived-…:local` tags get
    // rebuilt rather than silently reused with stale content.
    let mut hasher = Sha256::new();
    hasher.update(b"pitboss-derived-tag-v1\n");
    hasher.update(b"base=");
    hasher.update(base.as_bytes());
    hasher.update(b"\n");

    // Sort apt packages so `["jq","mdbook"]` and `["mdbook","jq"]` hash
    // identically — install order doesn't change the resulting layer.
    let mut apt_sorted: Vec<&String> = container.extra_apt.iter().collect();
    apt_sorted.sort();
    for pkg in apt_sorted {
        hasher.update(b"apt=");
        hasher.update(pkg.as_bytes());
        hasher.update(b"\n");
    }

    // Sort copy specs by container path (the deterministic key) so
    // operators can shuffle the manifest order without invalidating the
    // cache. Hash the host file's CONTENTS, not just the path — the
    // operator may edit the file, and the rebuild needs to fire.
    let mut copy_sorted: Vec<&CopySpec> = container.copy.iter().collect();
    copy_sorted.sort_by(|a, b| a.container.cmp(&b.container));
    for spec in copy_sorted {
        let host = expand_tilde(&spec.host);
        hasher.update(b"copy_container=");
        hasher.update(spec.container.to_string_lossy().as_bytes());
        hasher.update(b"\n");
        hasher.update(b"copy_host_bytes=");
        let mut file_hasher = Sha256::new();
        hash_path_into(&host, &mut file_hasher).with_context(|| {
            format!(
                "hashing copy source {} for derived-image tag",
                host.display()
            )
        })?;
        let file_digest = file_hasher.finalize();
        hasher.update(format!("{:x}", file_digest).as_bytes());
        hasher.update(b"\n");
    }

    let digest = hasher.finalize();
    // 12 hex chars = 48 bits — ~10^14 distinct tags. Plenty for a per-host
    // local cache; the leading byte cluster reads cleanly in image lists.
    let short = format!("{:x}", digest);
    let tag = format!("pitboss-derived-{}:local", &short[..12]);
    Ok(Some(tag))
}

/// Build a thin Dockerfile that derives a child image from
/// `[container].image` (or the default), installs `[container].extra_apt`
/// as a single layer, and `COPY`s `[[container.copy]]` entries. The
/// host paths are written as numbered relative paths (`copy0`, `copy1`,
/// …) so the staged build context layout is predictable.
pub fn synthesize_dockerfile(container: &ContainerConfig) -> String {
    let base = container.image.as_deref().unwrap_or(DEFAULT_BASE_IMAGE);
    let mut out = String::new();
    out.push_str("# Auto-generated by `pitboss container-build` — do not edit.\n");
    out.push_str(&format!("FROM {base}\n"));

    // Track the active USER so we don't emit redundant `USER root`
    // directives when both extra_apt and copy are present (each one
    // would otherwise stamp its own `USER root` line, layering a
    // no-op).
    let mut as_root = false;

    if !container.extra_apt.is_empty() {
        out.push_str("USER root\n");
        as_root = true;
        out.push_str("RUN apt-get update \\\n");
        out.push_str(" && apt-get install -y --no-install-recommends");
        for pkg in &container.extra_apt {
            out.push(' ');
            out.push_str(pkg);
        }
        out.push_str(" \\\n");
        out.push_str(" && rm -rf /var/lib/apt/lists/*\n");
    }

    if !container.copy.is_empty() {
        // COPY runs as root regardless of USER, but we want the file
        // ownership to settle as `pitboss:pitboss` so the runtime user
        // can write/exec it without surprise.
        if !as_root {
            out.push_str("USER root\n");
            as_root = true;
        }
        for (i, spec) in container.copy.iter().enumerate() {
            out.push_str(&format!(
                "COPY --chown=pitboss:pitboss copy{i} {}\n",
                spec.container.display()
            ));
        }
    }

    // Restore the canonical runtime user. Phase-1 dispatch logic
    // assumes the image leaves the runtime as the `pitboss` user;
    // re-asserting it here keeps the derived image consistent — but
    // only when we strayed from it.
    if as_root {
        out.push_str("USER pitboss\n");
    }
    out
}

/// Top-level entry called from main.rs for `pitboss container-build`.
pub fn run_container_build(container: &ContainerConfig, opts: &BuildOptions) -> Result<()> {
    let dockerfile = synthesize_dockerfile(container);

    if opts.print_dockerfile {
        print!("{dockerfile}");
        return Ok(());
    }

    let Some(tag) = derived_image_tag(container)? else {
        bail!(
            "[container] has no derived-image inputs (no extra_apt, no [[container.copy]]) — \
             nothing to build. Use `pitboss container-dispatch` against the stock image instead."
        );
    };

    let runtime = crate::dispatch::container::detect_runtime(
        opts.runtime_override.as_deref(),
        container.runtime.as_deref(),
    )?;

    // Idempotency: skip the build if the derived tag is already in the
    // local image store, unless the operator forced --no-cache.
    if !opts.no_cache && image_exists(&runtime, &tag)? {
        if !opts.dry_run {
            println!("derived image already exists: {tag} (--no-cache to force rebuild)");
        }
        return Ok(());
    }

    // Stage the build context: a temp dir holding the synthesized
    // Dockerfile and the numbered COPY sources.
    let context_dir = tempfile::Builder::new()
        .prefix("pitboss-build-context-")
        .tempdir()
        .context("creating temp build context dir")?;

    let dockerfile_path = context_dir.path().join("Dockerfile");
    std::fs::write(&dockerfile_path, &dockerfile)
        .with_context(|| format!("writing {}", dockerfile_path.display()))?;

    for (i, spec) in container.copy.iter().enumerate() {
        let host = expand_tilde(&spec.host);
        if !host.exists() {
            bail!(
                "[[container.copy]] host path does not exist: {}",
                host.display()
            );
        }
        let dest = context_dir.path().join(format!("copy{i}"));
        copy_path(&host, &dest).with_context(|| {
            format!(
                "staging copy source {} → {}",
                host.display(),
                dest.display()
            )
        })?;
    }

    let mut args: Vec<String> = vec![
        "build".into(),
        "--layers".into(),
        "-t".into(),
        tag.clone(),
        "-f".into(),
        dockerfile_path.display().to_string(),
        context_dir.path().display().to_string(),
    ];
    if opts.no_cache {
        // --no-cache implies "rebuild every layer" but is compatible
        // with --layers (the layer cache is still populated for the
        // *next* build). Without --layers, podman warns and downgrades.
        args.push("--no-cache".into());
    }

    if opts.dry_run {
        let cmd_str = std::iter::once(runtime.as_str())
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        println!("{cmd_str}");
        return Ok(());
    }

    let status = Command::new(&runtime)
        .args(&args)
        .status()
        .with_context(|| format!("invoking {runtime} build"))?;
    if !status.success() {
        bail!("{runtime} build exited with {status}");
    }

    println!("built derived image: {tag}");
    Ok(())
}

/// Best-effort tilde expansion. Mirrors the helper in
/// `dispatch/container.rs`; kept here too rather than re-exported to
/// keep the build module's import surface small.
fn expand_tilde(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match shellexpand::tilde(s.as_ref()) {
        std::borrow::Cow::Borrowed(_) => p.to_path_buf(),
        std::borrow::Cow::Owned(expanded) => PathBuf::from(expanded),
    }
}

/// Decide whether to surface a "derived image not built; using
/// slow-path apt install" warning to the operator.
///
/// Returns `Some(message)` when the operator declared `extra_apt`
/// inputs that warrant a derived image (so a `container-build` would
/// have helped) but no such image exists locally — `container-dispatch`
/// will fall through to the Phase 1 apt-at-spin-up path. The
/// `[[container.copy]]` case is excluded because the dispatcher already
/// hard-errors there (the COPY contents would be missing — see
/// `dispatch/container.rs:run_container_dispatch`).
///
/// Returns `None` when no warning is needed: the derived image is
/// already in use, no derived inputs were declared, or the copy-set
/// hard-error path will fire instead.
pub(crate) fn derived_fallback_warning(
    container: &ContainerConfig,
    derived_tag: Option<&str>,
    use_derived: bool,
    manifest_path: &Path,
) -> Option<String> {
    if use_derived {
        return None;
    }
    if container.extra_apt.is_empty() {
        return None;
    }
    if !container.copy.is_empty() {
        return None;
    }
    let tag = derived_tag?;
    Some(format!(
        "warning: derived image {tag} not found locally; falling back to apt-at-spin-up.\n\
         Re-run `pitboss container-build {}` to restore the cached fast-path.",
        manifest_path.display()
    ))
}

/// Check whether `tag` exists in the runtime's local image store.
/// Returns Ok(true) when the image is present, Ok(false) when absent,
/// and Err only on runtime invocation failures.
pub(crate) fn image_exists(runtime: &str, tag: &str) -> Result<bool> {
    let status = Command::new(runtime)
        .args(["image", "exists", tag])
        .status()
        .with_context(|| format!("invoking {runtime} image exists"))?;
    Ok(status.success())
}

/// Recursively hash a path's contents into the supplied hasher. For
/// directories we walk in sorted order so the hash is deterministic
/// across filesystems with different `readdir` orderings.
fn hash_path_into(path: &Path, hasher: &mut Sha256) -> std::io::Result<()> {
    let meta = std::fs::metadata(path)?;
    if meta.is_dir() {
        hasher.update(b"D|");
        hasher.update(path.file_name().unwrap_or_default().as_encoded_bytes());
        hasher.update(b"\n");
        let mut entries: BTreeMap<std::ffi::OsString, PathBuf> = BTreeMap::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            entries.insert(entry.file_name(), entry.path());
        }
        for (_, child) in entries {
            hash_path_into(&child, hasher)?;
        }
    } else if meta.is_file() {
        hasher.update(b"F|");
        hasher.update(path.file_name().unwrap_or_default().as_encoded_bytes());
        hasher.update(b"|len=");
        hasher.update(meta.len().to_le_bytes());
        hasher.update(b"|bytes=");
        let bytes = std::fs::read(path)?;
        hasher.update(&bytes);
        hasher.update(b"\n");
    } else {
        // Symlinks and other special files are unusual inputs for a
        // build COPY. Hash a stable marker so the user gets a tag, but
        // the build will likely fail at COPY time with a clearer error.
        hasher.update(b"S|");
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update(b"\n");
    }
    Ok(())
}

/// Copy `src` into `dest`. For files this is a single std::fs::copy;
/// for directories we recurse so the build context layout mirrors the
/// host tree under the synthesized name (`copyN`).
fn copy_path(src: &Path, dest: &Path) -> std::io::Result<()> {
    let meta = std::fs::metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let from = entry.path();
            let to = dest.join(entry.file_name());
            copy_path(&from, &to)?;
        }
    } else if meta.is_file() {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dest)?;
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "[[container.copy]] sources must be regular files or directories: {}",
                src.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cfg() -> ContainerConfig {
        ContainerConfig::default()
    }

    #[test]
    fn no_inputs_yields_no_tag() {
        let c = cfg();
        assert_eq!(derived_image_tag(&c).unwrap(), None);
    }

    #[test]
    fn extra_apt_alone_yields_a_tag() {
        let c = ContainerConfig {
            extra_apt: vec!["mdbook".into()],
            ..cfg()
        };
        let tag = derived_image_tag(&c).unwrap().expect("tag for apt-only");
        assert!(tag.starts_with("pitboss-derived-"));
        assert!(tag.ends_with(":local"), "tag: {tag}");
    }

    #[test]
    fn tag_is_stable_across_apt_ordering() {
        let a = ContainerConfig {
            extra_apt: vec!["jq".into(), "mdbook".into()],
            ..cfg()
        };
        let b = ContainerConfig {
            extra_apt: vec!["mdbook".into(), "jq".into()],
            ..cfg()
        };
        assert_eq!(
            derived_image_tag(&a).unwrap(),
            derived_image_tag(&b).unwrap(),
            "manifest apt order must not change the derived tag"
        );
    }

    #[test]
    fn tag_changes_when_apt_changes() {
        let a = ContainerConfig {
            extra_apt: vec!["mdbook".into()],
            ..cfg()
        };
        let b = ContainerConfig {
            extra_apt: vec!["mdbook".into(), "jq".into()],
            ..cfg()
        };
        assert_ne!(
            derived_image_tag(&a).unwrap(),
            derived_image_tag(&b).unwrap()
        );
    }

    #[test]
    fn tag_changes_when_base_image_changes() {
        let a = ContainerConfig {
            extra_apt: vec!["mdbook".into()],
            ..cfg()
        };
        let b = ContainerConfig {
            image: Some("custom/image:1".into()),
            extra_apt: vec!["mdbook".into()],
            ..cfg()
        };
        assert_ne!(
            derived_image_tag(&a).unwrap(),
            derived_image_tag(&b).unwrap()
        );
    }

    #[test]
    fn tag_changes_when_copy_source_bytes_change() {
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("script.sh");
        std::fs::write(&host, b"echo hello\n").unwrap();
        let a = ContainerConfig {
            copy: vec![CopySpec {
                host: host.clone(),
                container: PathBuf::from("/opt/script.sh"),
            }],
            ..cfg()
        };
        let tag_a = derived_image_tag(&a).unwrap().unwrap();

        // Same path, different bytes — tag must flip so a rebuild fires.
        std::fs::write(&host, b"echo goodbye\n").unwrap();
        let tag_b = derived_image_tag(&a).unwrap().unwrap();
        assert_ne!(
            tag_a, tag_b,
            "edits to a COPY source must invalidate the derived tag"
        );
    }

    #[test]
    fn dockerfile_contains_apt_install_when_extra_apt_set() {
        let c = ContainerConfig {
            extra_apt: vec!["mdbook".into(), "jq".into()],
            ..cfg()
        };
        let df = synthesize_dockerfile(&c);
        assert!(df.contains("FROM "), "FROM line: {df}");
        assert!(
            df.contains("apt-get install -y --no-install-recommends mdbook jq"),
            "apt install line: {df}"
        );
        assert!(df.contains("USER root"), "needs root for apt: {df}");
        assert!(
            df.trim_end().ends_with("USER pitboss"),
            "must drop privs at end: {df}"
        );
    }

    #[test]
    fn dockerfile_contains_copy_when_copy_set() {
        let c = ContainerConfig {
            copy: vec![CopySpec {
                host: PathBuf::from("/host/script.sh"),
                container: PathBuf::from("/opt/script.sh"),
            }],
            ..cfg()
        };
        let df = synthesize_dockerfile(&c);
        assert!(
            df.contains("COPY --chown=pitboss:pitboss copy0 /opt/script.sh"),
            "copy line: {df}"
        );
    }

    #[test]
    fn dockerfile_emits_user_root_only_once_when_both_set() {
        // With both `extra_apt` and `copy`, the Dockerfile previously
        // emitted two adjacent `USER root` directives — one for the
        // RUN apt-get block, another for the COPY block. The second is
        // redundant (apt already left us as root) and produces a no-op
        // image layer. Verify we collapse to a single `USER root`.
        let c = ContainerConfig {
            extra_apt: vec!["jq".into()],
            copy: vec![CopySpec {
                host: PathBuf::from("/host/x"),
                container: PathBuf::from("/opt/x"),
            }],
            ..cfg()
        };
        let df = synthesize_dockerfile(&c);
        let user_root_count = df.lines().filter(|l| l.trim() == "USER root").count();
        assert_eq!(
            user_root_count, 1,
            "expected exactly one `USER root` directive, got {user_root_count}: {df}"
        );
        assert!(
            df.trim_end().ends_with("USER pitboss"),
            "must drop privs at end: {df}"
        );
    }

    #[test]
    fn fallback_warning_emits_when_extra_apt_set_and_image_missing() {
        let c = ContainerConfig {
            extra_apt: vec!["mdbook".into()],
            ..cfg()
        };
        let manifest = PathBuf::from("/tmp/manifest.toml");
        let out =
            derived_fallback_warning(&c, Some("pitboss-derived-abc123:local"), false, &manifest);
        let msg = out.expect("warning should fire");
        assert!(
            msg.contains("pitboss-derived-abc123:local"),
            "missing tag: {msg}"
        );
        assert!(
            msg.contains("apt-at-spin-up"),
            "missing fallback hint: {msg}"
        );
        assert!(
            msg.contains("pitboss container-build /tmp/manifest.toml"),
            "missing rebuild hint with manifest path: {msg}"
        );
    }

    #[test]
    fn fallback_warning_suppressed_when_derived_image_in_use() {
        let c = ContainerConfig {
            extra_apt: vec!["mdbook".into()],
            ..cfg()
        };
        let out = derived_fallback_warning(
            &c,
            Some("pitboss-derived-abc123:local"),
            true, // built image is in use; no fallback in effect
            &PathBuf::from("/tmp/manifest.toml"),
        );
        assert!(out.is_none(), "no warning when derived image is used");
    }

    #[test]
    fn fallback_warning_suppressed_when_no_extra_apt() {
        // Operator declared no derived inputs — there's no slow-path
        // they're missing out on. Stay quiet.
        let c = ContainerConfig::default();
        let out = derived_fallback_warning(&c, None, false, &PathBuf::from("/tmp/manifest.toml"));
        assert!(out.is_none());
    }

    #[test]
    fn fallback_warning_suppressed_when_copy_set() {
        // The dispatcher already hard-errors when copy is set without
        // a built image — we'd be redundantly warning about a path
        // that's about to bail.
        let c = ContainerConfig {
            extra_apt: vec!["mdbook".into()],
            copy: vec![CopySpec {
                host: PathBuf::from("/host/x"),
                container: PathBuf::from("/opt/x"),
            }],
            ..cfg()
        };
        let out = derived_fallback_warning(
            &c,
            Some("pitboss-derived-abc123:local"),
            false,
            &PathBuf::from("/tmp/manifest.toml"),
        );
        assert!(out.is_none(), "copy-set is the hard-error path, not warn");
    }

    #[test]
    fn fallback_warning_suppressed_when_derived_tag_is_none() {
        // Defensive: if no tag was computed (shouldn't happen given
        // extra_apt is non-empty, but the type permits it), stay quiet
        // rather than emit an empty-tag warning.
        let c = ContainerConfig {
            extra_apt: vec!["mdbook".into()],
            ..cfg()
        };
        let out = derived_fallback_warning(&c, None, false, &PathBuf::from("/tmp/manifest.toml"));
        assert!(out.is_none());
    }

    #[test]
    fn dockerfile_skips_apt_block_when_only_copy_set() {
        let c = ContainerConfig {
            copy: vec![CopySpec {
                host: PathBuf::from("/host/script.sh"),
                container: PathBuf::from("/opt/script.sh"),
            }],
            ..cfg()
        };
        let df = synthesize_dockerfile(&c);
        assert!(!df.contains("apt-get"), "no apt block expected: {df}");
        assert!(df.contains("COPY"));
    }
}
