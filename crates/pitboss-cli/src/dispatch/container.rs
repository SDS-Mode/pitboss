//! `pitboss container-dispatch` — assemble and exec a docker/podman run command
//! from the manifest's `[container]` section.
//!
//! The current process is replaced (exec-style on Unix) with the container
//! invocation so signal propagation and TTY passthrough work naturally.
//! On a dry run the assembled command is printed to stdout and the process
//! exits 0.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::manifest::schema::ContainerConfig;

const DEFAULT_IMAGE: &str = "ghcr.io/sds-mode/pitboss-with-claude:latest";
const PITBOSS_CONTAINER_USER_UID: u32 = 1000;

/// Entry point called from `main.rs` for `pitboss container-dispatch`.
///
/// Validates that the manifest has a `[container]` section, then builds and
/// execs the container run command.
pub fn run_container_dispatch(
    manifest_path: &Path,
    container: &ContainerConfig,
    run_dir_override: Option<PathBuf>,
    dry_run: bool,
    runtime_override: Option<&str>,
) -> Result<()> {
    let runtime = detect_runtime(runtime_override, container.runtime.as_deref())?;
    let manifest_abs = manifest_path
        .canonicalize()
        .with_context(|| format!("canonicalizing manifest path {}", manifest_path.display()))?;

    // Phase 2: when the manifest declares derived-image inputs, prefer
    // the locally-built derived tag if it exists. `[[container.copy]]`
    // is hard-required (the COPY contents only exist in the baked
    // image) — error loudly when it's missing rather than silently
    // falling back to a stock-image dispatch.
    let derived = super::container_build::derived_image_tag(container)?;
    let use_derived = match derived.as_ref() {
        Some(tag) => super::container_build::image_exists(&runtime, tag)?,
        None => false,
    };
    if !container.copy.is_empty() && !use_derived {
        bail!(
            "[[container.copy]] requires a built derived image. Run \
             `pitboss container-build {}` first to bake the COPY contents \
             into pitboss-derived-…:local.",
            manifest_path.display()
        );
    }
    let derived_image_override = if use_derived {
        derived.as_deref()
    } else {
        None
    };

    let args = build_run_args(
        &runtime,
        container,
        &manifest_abs,
        run_dir_override,
        derived_image_override,
    )?;

    if dry_run {
        // Print the full command the operator would run so they can inspect it.
        let cmd_str = std::iter::once(runtime.as_str())
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        println!("{}", cmd_str);
        return Ok(());
    }

    let err = Command::new(&runtime).args(&args).exec();
    // exec() only returns on failure.
    Err(err).with_context(|| format!("exec {runtime}: failed to launch container"))
}

/// Build the full argument list for `<runtime> run …`.
///
/// `derived_image_override` is `Some(tag)` when `container-dispatch`
/// resolved a built derived image (apt + COPY already baked in). When
/// set, the Phase-1 apt-at-spin-up wrap is skipped — the work is
/// already in the image.
fn build_run_args(
    runtime: &str,
    container: &ContainerConfig,
    manifest_abs: &Path,
    run_dir_override: Option<PathBuf>,
    derived_image_override: Option<&str>,
) -> Result<Vec<String>> {
    let mut args: Vec<String> = vec!["run".into(), "--rm".into()];

    // TTY passthrough: only attach stdin/tty when the host stdout is a real
    // terminal (interactive session, TUI support). In headless CI the flags
    // are omitted so docker doesn't complain about a missing tty.
    if atty::is(atty::Stream::Stdout) {
        args.push("-it".into());
    }

    // ── UID alignment ────────────────────────────────────────────────────────
    // Rootless podman: --userns=keep-id maps host UID → same container UID so
    // mounted file ownership is transparent. Docker: no user namespace by
    // default; if the host UID isn't 1000 (the container pitboss user) we
    // pass -u host_uid:host_gid to align ownership.
    let is_podman = std::path::Path::new(runtime)
        .file_name()
        .and_then(|n| n.to_str())
        == Some("podman");
    if is_podman && is_rootless_podman() {
        args.push("--userns=keep-id".into());
    } else if !is_podman {
        let uid = unsafe { libc::getuid() };
        if uid != PITBOSS_CONTAINER_USER_UID {
            let gid = unsafe { libc::getgid() };
            args.push("-u".into());
            args.push(format!("{uid}:{gid}"));
        }
    }

    // ── User-declared mounts ─────────────────────────────────────────────────
    // Tracks container paths already covered so auto-inject logic can skip
    // duplicates without error.
    let mut covered_container_paths: Vec<PathBuf> = Vec::new();

    for spec in &container.mounts {
        let host = expand_tilde(&spec.host);
        let container_path = &spec.container;
        let options = if spec.readonly { "ro,z" } else { "rw,z" };
        args.push("-v".into());
        args.push(format!(
            "{}:{}:{options}",
            host.display(),
            container_path.display()
        ));
        covered_container_paths.push(container_path.clone());
    }

    // ── Auto-inject ~/.claude ─────────────────────────────────────────────────
    // Required for OAuth auth (Linux) unless the operator already declared
    // a mount targeting /home/pitboss/.claude.
    let claude_container = PathBuf::from("/home/pitboss/.claude");
    if !covered_container_paths.contains(&claude_container) {
        if let Some(home) = home_dir() {
            let host_claude = home.join(".claude");
            args.push("-v".into());
            args.push(format!(
                "{}:{}:rw,z",
                host_claude.display(),
                claude_container.display()
            ));
        }
    }

    // ── Auto-inject run_dir ───────────────────────────────────────────────────
    // Artifacts produced inside the container should persist on the host.
    // We mount the effective run_dir (override > default) to the same
    // absolute path inside the container.
    let effective_run_dir = run_dir_override.unwrap_or_else(default_run_dir);
    // Ensure the directory exists on the host so Docker doesn't create it
    // as root-owned when the mount target is absent.
    std::fs::create_dir_all(&effective_run_dir).ok();
    let run_dir_container = PathBuf::from("/home/pitboss/.local/share/pitboss/runs");
    if !covered_container_paths.contains(&run_dir_container) {
        args.push("-v".into());
        args.push(format!(
            "{}:{}:rw,z",
            effective_run_dir.display(),
            run_dir_container.display()
        ));
    }

    // ── Manifest ─────────────────────────────────────────────────────────────
    // Strip the [container] section before mounting: the inner `pitboss dispatch`
    // doesn't use it, and images built before this feature was added reject it
    // as an unknown field under deny_unknown_fields.
    let manifest_host = strip_container_section(manifest_abs)?;
    args.push("-v".into());
    args.push(format!(
        "{}:/run/pitboss.toml:ro,z",
        manifest_host.display()
    ));

    // ── Working directory ─────────────────────────────────────────────────────
    let workdir = container
        .workdir
        .clone()
        .or_else(|| container.mounts.first().map(|m| m.container.clone()))
        .unwrap_or_else(|| PathBuf::from("/home/pitboss"));
    args.push("-w".into());
    args.push(workdir.display().to_string());

    // ── Extra operator args ───────────────────────────────────────────────────
    args.extend(container.extra_args.clone());

    // ── extra_apt: validate + override user to root for the apt step ─────────
    // apt-get needs root. When `extra_apt` is non-empty AND no derived
    // image is available, we override `-u` to 0:0 here (last `-u` wins)
    // and rewrite the entrypoint below into a shell that installs the
    // packages, then `exec runuser -u pitboss -- pitboss dispatch …` so
    // the long-lived process drops to UID 1000 before workers spawn.
    // Tini stays as PID 1, so signal forwarding to the post-exec pitboss
    // is preserved.
    //
    // When a derived image is in play (Phase 2 / `pitboss container-build`),
    // apt is already baked into the image and this whole wrap is skipped —
    // dispatch runs as the canonical pitboss user from the start.
    //
    // Package names are joined verbatim into a shell command, so we
    // require each entry to match `[a-zA-Z0-9][a-zA-Z0-9.+-]*` and reject
    // anything else at dispatch time.
    let bootstrap_apt = !container.extra_apt.is_empty() && derived_image_override.is_none();
    if bootstrap_apt {
        for pkg in &container.extra_apt {
            if !is_valid_apt_pkg(pkg) {
                bail!(
                    "[container].extra_apt: invalid package name {pkg:?} \
                     (allowed: ASCII alphanumeric, `.`, `+`, `-`; must start \
                     with alphanumeric)"
                );
            }
        }
        args.push("-u".into());
        args.push("0:0".into());
    }

    // ── Image + pitboss command ───────────────────────────────────────────────
    // Derived image (built by `container-build`) wins over manifest
    // `image` when present — the manifest's `image` field is the BASE
    // for derivation, not the runtime image.
    let image = derived_image_override
        .map(String::from)
        .or_else(|| container.image.clone())
        .unwrap_or_else(|| DEFAULT_IMAGE.to_string());
    args.push(image);

    if bootstrap_apt {
        let pkg_list = container.extra_apt.join(" ");
        let cmd = format!(
            "apt-get update && \
             apt-get install -y --no-install-recommends {pkg_list} && \
             exec runuser -u pitboss -- pitboss dispatch /run/pitboss.toml"
        );
        args.push("sh".into());
        args.push("-c".into());
        args.push(cmd);
    } else {
        args.push("pitboss".into());
        args.push("dispatch".into());
        args.push("/run/pitboss.toml".into());
    }

    Ok(args)
}

/// Validate a debian/ubuntu package name for shell-safe interpolation
/// into `apt-get install -y …`. Restrictive on purpose: must begin with
/// an ASCII alphanumeric and contain only `[a-zA-Z0-9.+-]` thereafter.
///
/// Re-used by `manifest::validate` so `pitboss validate` rejects bad names
/// in the same shape as dispatch.
pub(crate) fn is_valid_apt_pkg(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'))
}

/// Detect the container runtime to use, in priority order:
///   1. `runtime_override` (CLI `--runtime` flag)
///   2. `container.runtime` from the manifest
///   3. `PITBOSS_CONTAINER_RUNTIME` env var
///   4. Auto-detect: prefer `podman`, fall back to `docker`
///
/// Re-used by `dispatch/container_build` so both subcommands see the
/// same runtime selection rules.
pub(crate) fn detect_runtime(
    runtime_override: Option<&str>,
    manifest_runtime: Option<&str>,
) -> Result<String> {
    let preferred = runtime_override
        .or(manifest_runtime)
        .or_else(|| {
            std::env::var("PITBOSS_CONTAINER_RUNTIME")
                .ok()
                .as_deref()
                .map(|_| "")
        })
        .unwrap_or("auto");

    // Normalise the env var case separately (borrow-checker friendly).
    let env_val = std::env::var("PITBOSS_CONTAINER_RUNTIME").unwrap_or_default();
    let preferred = if preferred.is_empty() {
        env_val.as_str()
    } else {
        preferred
    };

    match preferred {
        "auto" | "" => {
            // Prefer podman; fall back to docker.
            if which("podman") {
                return Ok("podman".into());
            }
            if which("docker") {
                return Ok("docker".into());
            }
            bail!(
                "no container runtime found on PATH (tried podman, docker). \
                 Install one or set PITBOSS_CONTAINER_RUNTIME."
            );
        }
        "podman" => {
            if !which("podman") {
                bail!("container runtime 'podman' not found on PATH");
            }
            Ok("podman".into())
        }
        "docker" => {
            if !which("docker") {
                bail!("container runtime 'docker' not found on PATH");
            }
            Ok("docker".into())
        }
        other => bail!(
            "unknown container runtime '{}' — expected 'docker', 'podman', or 'auto'",
            other
        ),
    }
}

/// Returns `true` if `podman info` reports the daemon is running rootless.
/// On failure (not podman, podman not running, etc.) returns `false` — the
/// safe default is to omit `--userns=keep-id` rather than fail hard.
fn is_rootless_podman() -> bool {
    Command::new("podman")
        .args(["info", "--format", "{{.Host.Security.Rootless}}"])
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

/// Returns `true` if `name` is present and executable on `$PATH`.
fn which(name: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {name}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Expand a leading `~` to the home directory.
fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    } else if s == "~" {
        if let Some(home) = home_dir() {
            return home;
        }
    }
    path.to_path_buf()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn default_run_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".local/share/pitboss/runs")
}

/// Read `manifest_abs`, remove the `[container]` key, and write the result to
/// a temp file. Returns the temp file path, which is mounted read-only into the
/// container in place of the original manifest.
///
/// Stripping is necessary because older container images (built before
/// `container-dispatch` was introduced) reject `[container]` as an unknown
/// field when parsing with `deny_unknown_fields`. The inner `pitboss dispatch`
/// has no use for the section anyway — it is host-side metadata only.
///
/// The temp file is written to /tmp with the host PID in the name. Because
/// this process is replaced by exec() there is no Drop-based cleanup; the
/// file persists until /tmp is next cleared (acceptable for a small TOML file).
fn strip_container_section(manifest_abs: &Path) -> Result<PathBuf> {
    let text = std::fs::read_to_string(manifest_abs)
        .with_context(|| format!("reading manifest {}", manifest_abs.display()))?;

    let mut val: toml::Value =
        toml::from_str(&text).with_context(|| "parsing manifest to strip [container] section")?;

    if let toml::Value::Table(ref mut table) = val {
        table.remove("container");
    }

    let stripped =
        toml::to_string_pretty(&val).with_context(|| "re-serialising stripped manifest")?;

    let tmp = std::env::temp_dir().join(format!("pitboss-manifest-{}.toml", std::process::id()));

    std::fs::write(&tmp, stripped)
        .with_context(|| format!("writing stripped manifest to {}", tmp.display()))?;

    Ok(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::schema::{ContainerConfig, MountSpec};
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn make_config(mounts: Vec<MountSpec>) -> ContainerConfig {
        ContainerConfig {
            image: None,
            runtime: None,
            extra_args: vec![],
            extra_apt: vec![],
            mounts,
            copy: vec![],
            workdir: None,
        }
    }

    /// Write a minimal valid TOML manifest to a temp file and return the path.
    /// `build_run_args` now reads the manifest (to strip [container]), so tests
    /// that previously passed a nonexistent path need a real file.
    fn temp_manifest() -> PathBuf {
        let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pitboss-test-manifest-{}-{n}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"
[[task]]
id = "t"
directory = "/tmp"
prompt = "hi"
"#,
        )
        .expect("write test manifest");
        path
    }

    #[test]
    fn dry_run_includes_pitboss_dispatch() {
        let cfg = make_config(vec![]);
        let manifest = temp_manifest();
        // Build args without calling exec.
        let args = build_run_args("podman", &cfg, &manifest, None, None).unwrap();
        let joined = args.join(" ");
        assert!(joined.contains("pitboss"), "should call pitboss: {joined}");
        assert!(
            joined.contains("dispatch"),
            "should call dispatch: {joined}"
        );
        assert!(
            joined.contains("/run/pitboss.toml"),
            "manifest path: {joined}"
        );
    }

    #[test]
    fn default_image_used_when_none_specified() {
        let cfg = make_config(vec![]);
        let args = build_run_args("docker", &cfg, &temp_manifest(), None, None).unwrap();
        let joined = args.join(" ");
        assert!(
            joined.contains(DEFAULT_IMAGE),
            "should use default image: {joined}"
        );
    }

    #[test]
    fn custom_image_overrides_default() {
        let cfg = ContainerConfig {
            image: Some("my-org/pitboss:latest".into()),
            ..ContainerConfig::default()
        };
        let args = build_run_args("docker", &cfg, &temp_manifest(), None, None).unwrap();
        let joined = args.join(" ");
        assert!(
            joined.contains("my-org/pitboss:latest"),
            "should use custom image: {joined}"
        );
        assert!(
            !joined.contains(DEFAULT_IMAGE),
            "should not contain default image: {joined}"
        );
    }

    #[test]
    fn user_mount_appears_before_image() {
        let cfg = ContainerConfig {
            mounts: vec![MountSpec {
                host: PathBuf::from("/home/alice/project"),
                container: PathBuf::from("/project"),
                readonly: false,
            }],
            ..ContainerConfig::default()
        };
        let args = build_run_args("docker", &cfg, &temp_manifest(), None, None).unwrap();
        let joined = args.join(" ");
        assert!(
            joined.contains("/home/alice/project:/project:rw,z"),
            "user mount: {joined}"
        );
    }

    #[test]
    fn readonly_mount_uses_ro_flag() {
        let cfg = ContainerConfig {
            mounts: vec![MountSpec {
                host: PathBuf::from("/ref"),
                container: PathBuf::from("/ref"),
                readonly: true,
            }],
            ..ContainerConfig::default()
        };
        let args = build_run_args("podman", &cfg, &temp_manifest(), None, None).unwrap();
        let joined = args.join(" ");
        assert!(joined.contains("/ref:/ref:ro,z"), "readonly flag: {joined}");
    }

    #[test]
    fn auto_inject_claude_when_not_in_mounts() {
        let cfg = make_config(vec![]);
        let args = build_run_args("docker", &cfg, &temp_manifest(), None, None).unwrap();
        let joined = args.join(" ");
        assert!(
            joined.contains("/home/pitboss/.claude"),
            "auto-inject claude: {joined}"
        );
    }

    #[test]
    fn skip_claude_auto_inject_when_already_declared() {
        let cfg = ContainerConfig {
            mounts: vec![MountSpec {
                host: PathBuf::from("/my/claude"),
                container: PathBuf::from("/home/pitboss/.claude"),
                readonly: false,
            }],
            ..ContainerConfig::default()
        };
        let args = build_run_args("docker", &cfg, &temp_manifest(), None, None).unwrap();
        // Count -v args that mount to /home/pitboss/.claude — should be exactly 1
        // (the declared mount). The -w workdir may also reference the path but is
        // not a duplicate mount injection.
        let mount_count = args
            .windows(2)
            .filter(|w| w[0] == "-v" && w[1].contains(":/home/pitboss/.claude:"))
            .count();
        assert_eq!(
            mount_count, 1,
            "claude mount should appear exactly once: {args:?}"
        );
    }

    #[test]
    fn workdir_defaults_to_first_mount_container_path() {
        let cfg = ContainerConfig {
            mounts: vec![MountSpec {
                host: PathBuf::from("/project"),
                container: PathBuf::from("/project"),
                readonly: false,
            }],
            ..ContainerConfig::default()
        };
        let args = build_run_args("docker", &cfg, &temp_manifest(), None, None).unwrap();
        // -w should be followed by /project
        let w_pos = args.iter().position(|a| a == "-w").expect("-w flag");
        assert_eq!(args[w_pos + 1], "/project", "workdir: {args:?}");
    }

    #[test]
    fn workdir_falls_back_to_home_pitboss_when_no_mounts() {
        let cfg = make_config(vec![]);
        let args = build_run_args("docker", &cfg, &temp_manifest(), None, None).unwrap();
        let w_pos = args.iter().position(|a| a == "-w").expect("-w flag");
        assert_eq!(
            args[w_pos + 1],
            "/home/pitboss",
            "fallback workdir: {args:?}"
        );
    }

    #[test]
    fn explicit_workdir_overrides_mount_default() {
        let cfg = ContainerConfig {
            mounts: vec![MountSpec {
                host: PathBuf::from("/project"),
                container: PathBuf::from("/project"),
                readonly: false,
            }],
            workdir: Some(PathBuf::from("/project/sub")),
            ..ContainerConfig::default()
        };
        let args = build_run_args("docker", &cfg, &temp_manifest(), None, None).unwrap();
        let w_pos = args.iter().position(|a| a == "-w").expect("-w flag");
        assert_eq!(
            args[w_pos + 1],
            "/project/sub",
            "explicit workdir: {args:?}"
        );
    }

    #[test]
    fn extra_args_appear_before_image() {
        let cfg = ContainerConfig {
            extra_args: vec!["--network=host".into(), "--cap-drop=ALL".into()],
            ..ContainerConfig::default()
        };
        let args = build_run_args("docker", &cfg, &temp_manifest(), None, None).unwrap();
        let net_pos = args
            .iter()
            .position(|a| a == "--network=host")
            .expect("--network");
        let img_pos = args.iter().position(|a| a == DEFAULT_IMAGE).expect("image");
        assert!(net_pos < img_pos, "extra_args before image: {args:?}");
    }

    #[test]
    fn detect_runtime_rejects_unknown() {
        let err = detect_runtime(Some("containerd"), None);
        assert!(err.is_err(), "should reject unknown runtime");
    }

    #[test]
    fn detect_runtime_uses_override_before_manifest() {
        // Both say different things; CLI override wins.
        // We can't actually exec podman/docker in tests, so just confirm the
        // returned value matches the override when the binary is present.
        // If neither binary exists on the test host, skip gracefully.
        if which("docker") {
            let r = detect_runtime(Some("docker"), Some("podman")).unwrap();
            assert_eq!(r, "docker");
        }
    }

    #[test]
    fn run_dir_mount_uses_override() {
        let custom = PathBuf::from("/tmp/my-runs");
        let cfg = make_config(vec![]);
        let args =
            build_run_args("docker", &cfg, &temp_manifest(), Some(custom.clone()), None).unwrap();
        let joined = args.join(" ");
        assert!(
            joined.contains("/tmp/my-runs"),
            "run_dir override in mounts: {joined}"
        );
    }

    #[test]
    fn empty_extra_apt_keeps_direct_pitboss_entrypoint() {
        // Sanity: with no extra_apt the entrypoint args are still the bare
        // `pitboss dispatch /run/pitboss.toml` triplet — no shell wrap.
        let cfg = make_config(vec![]);
        let args = build_run_args("podman", &cfg, &temp_manifest(), None, None).unwrap();
        assert!(
            !args.iter().any(|a| a == "sh"),
            "no sh wrapper expected: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "0:0"),
            "no root override expected: {args:?}"
        );
        let dispatch_pos = args
            .iter()
            .position(|a| a == "dispatch")
            .expect("dispatch present");
        assert_eq!(args[dispatch_pos - 1], "pitboss", "args: {args:?}");
        assert_eq!(
            args[dispatch_pos + 1],
            "/run/pitboss.toml",
            "args: {args:?}"
        );
    }

    #[test]
    fn extra_apt_wraps_entrypoint_with_apt_install_and_runuser() {
        let cfg = ContainerConfig {
            extra_apt: vec!["mdbook".into(), "jq".into()],
            ..ContainerConfig::default()
        };
        let args = build_run_args("podman", &cfg, &temp_manifest(), None, None).unwrap();

        // -u 0:0 must appear (so apt-get can run as root).
        let u_pos = args
            .windows(2)
            .position(|w| w[0] == "-u" && w[1] == "0:0")
            .expect("expected -u 0:0 override: {args:?}");

        // …and must come AFTER any earlier -u from the existing user-alignment
        // logic, so it wins as the last -u in argv.
        let last_u = args
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == "-u")
            .map(|(i, _)| i)
            .next_back()
            .expect("at least one -u");
        assert_eq!(last_u, u_pos, "extra_apt -u 0:0 must be the last -u");

        // The image should be followed by `sh -c <cmd>`, not pitboss directly.
        let img_pos = args.iter().position(|a| a == DEFAULT_IMAGE).expect("image");
        assert_eq!(args[img_pos + 1], "sh", "wrapped entrypoint: {args:?}");
        assert_eq!(args[img_pos + 2], "-c", "wrapped entrypoint: {args:?}");

        let cmd = &args[img_pos + 3];
        assert!(
            cmd.contains("apt-get update"),
            "apt-get update missing: {cmd}"
        );
        assert!(
            cmd.contains("apt-get install -y --no-install-recommends mdbook jq"),
            "apt install line missing or mis-shaped: {cmd}"
        );
        assert!(
            cmd.contains("exec runuser -u pitboss -- pitboss dispatch /run/pitboss.toml"),
            "runuser drop missing: {cmd}"
        );
    }

    #[test]
    fn extra_apt_rejects_shell_metacharacters() {
        // Anything outside [a-zA-Z0-9][a-zA-Z0-9.+-]* must fail validation
        // before any shell command is constructed.
        for bad in [
            "mdbook;rm -rf /",
            "$(curl evil)",
            "pkg name",
            "--flag",
            ".bad",
        ] {
            let cfg = ContainerConfig {
                extra_apt: vec![bad.into()],
                ..ContainerConfig::default()
            };
            let result = build_run_args("podman", &cfg, &temp_manifest(), None, None);
            assert!(
                result.is_err(),
                "expected rejection for {bad:?}, got: {result:?}"
            );
            let msg = result.unwrap_err().to_string();
            assert!(
                msg.contains("extra_apt"),
                "error should mention extra_apt: {msg}"
            );
        }
    }

    #[test]
    fn derived_image_override_wins_over_manifest_image() {
        // When `container-dispatch` resolves a built derived tag, it
        // takes precedence over the operator's [container].image — the
        // manifest image is the base for derivation, not the runtime.
        let cfg = ContainerConfig {
            image: Some("base/image:1".into()),
            extra_apt: vec!["mdbook".into()],
            ..ContainerConfig::default()
        };
        let args = build_run_args(
            "podman",
            &cfg,
            &temp_manifest(),
            None,
            Some("pitboss-derived-abc123:local"),
        )
        .unwrap();
        assert!(
            args.iter().any(|a| a == "pitboss-derived-abc123:local"),
            "derived tag must appear: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "base/image:1"),
            "base image must NOT appear at runtime: {args:?}"
        );
    }

    #[test]
    fn derived_image_override_skips_entrypoint_apt_wrap() {
        // With a derived image, apt is already installed at build time —
        // the spin-up shell wrap and -u 0:0 root override should both be
        // suppressed so dispatch runs as pitboss with bare entrypoint.
        let cfg = ContainerConfig {
            extra_apt: vec!["mdbook".into(), "jq".into()],
            ..ContainerConfig::default()
        };
        let args = build_run_args(
            "podman",
            &cfg,
            &temp_manifest(),
            None,
            Some("pitboss-derived-deadbeef:local"),
        )
        .unwrap();
        assert!(
            !args.iter().any(|a| a == "0:0"),
            "no root override expected when derived image is used: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "sh"),
            "no shell wrap expected when derived image is used: {args:?}"
        );
        // Bare pitboss/dispatch entrypoint should be present instead.
        let dispatch_pos = args
            .iter()
            .position(|a| a == "dispatch")
            .expect("dispatch arg present");
        assert_eq!(args[dispatch_pos - 1], "pitboss");
        assert_eq!(args[dispatch_pos + 1], "/run/pitboss.toml");
    }

    #[test]
    fn extra_apt_accepts_realistic_package_names() {
        // Names with `.`, `+`, `-` and digits are common in the apt index
        // (libssl3, g++-12, python3.11) — make sure validation lets them through.
        let cfg = ContainerConfig {
            extra_apt: vec![
                "libssl3".into(),
                "g++-12".into(),
                "python3.11".into(),
                "pandoc".into(),
            ],
            ..ContainerConfig::default()
        };
        let args = build_run_args("podman", &cfg, &temp_manifest(), None, None)
            .expect("realistic package names should validate");
        let img_pos = args.iter().position(|a| a == DEFAULT_IMAGE).expect("image");
        let cmd = &args[img_pos + 3];
        assert!(
            cmd.contains("libssl3 g++-12 python3.11 pandoc"),
            "expected joined package list, got: {cmd}"
        );
    }
}
