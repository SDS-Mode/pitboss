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

    let args = build_run_args(&runtime, container, &manifest_abs, run_dir_override)?;

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
fn build_run_args(
    runtime: &str,
    container: &ContainerConfig,
    manifest_abs: &Path,
    run_dir_override: Option<PathBuf>,
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
    let is_podman = runtime.ends_with("podman");
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
    args.push("-v".into());
    args.push(format!(
        "{}:/run/pitboss.toml:ro,z",
        manifest_abs.display()
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

    // ── Image + pitboss command ───────────────────────────────────────────────
    let image = container
        .image
        .clone()
        .unwrap_or_else(|| DEFAULT_IMAGE.to_string());
    args.push(image);
    args.push("pitboss".into());
    args.push("dispatch".into());
    args.push("/run/pitboss.toml".into());

    Ok(args)
}

/// Detect the container runtime to use, in priority order:
///   1. `runtime_override` (CLI `--runtime` flag)
///   2. `container.runtime` from the manifest
///   3. `PITBOSS_CONTAINER_RUNTIME` env var
///   4. Auto-detect: prefer `podman`, fall back to `docker`
fn detect_runtime(
    runtime_override: Option<&str>,
    manifest_runtime: Option<&str>,
) -> Result<String> {
    let preferred = runtime_override
        .or(manifest_runtime)
        .or_else(|| std::env::var("PITBOSS_CONTAINER_RUNTIME").ok().as_deref().map(|_| ""))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::schema::{ContainerConfig, MountSpec};

    fn make_config(mounts: Vec<MountSpec>) -> ContainerConfig {
        ContainerConfig {
            image: None,
            runtime: None,
            extra_args: vec![],
            mounts,
            workdir: None,
        }
    }

    #[test]
    fn dry_run_includes_pitboss_dispatch() {
        let cfg = make_config(vec![]);
        let manifest = PathBuf::from("/tmp/test.toml");
        // Build args without calling exec.
        let args = build_run_args("podman", &cfg, &manifest, None).unwrap();
        let joined = args.join(" ");
        assert!(joined.contains("pitboss"), "should call pitboss: {joined}");
        assert!(joined.contains("dispatch"), "should call dispatch: {joined}");
        assert!(
            joined.contains("/run/pitboss.toml"),
            "manifest path: {joined}"
        );
    }

    #[test]
    fn default_image_used_when_none_specified() {
        let cfg = make_config(vec![]);
        let args = build_run_args("docker", &cfg, Path::new("/tmp/m.toml"), None).unwrap();
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
        let args = build_run_args("docker", &cfg, Path::new("/tmp/m.toml"), None).unwrap();
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
        let args = build_run_args("docker", &cfg, Path::new("/tmp/m.toml"), None).unwrap();
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
        let args = build_run_args("podman", &cfg, Path::new("/tmp/m.toml"), None).unwrap();
        let joined = args.join(" ");
        assert!(joined.contains("/ref:/ref:ro,z"), "readonly flag: {joined}");
    }

    #[test]
    fn auto_inject_claude_when_not_in_mounts() {
        let cfg = make_config(vec![]);
        let args = build_run_args("docker", &cfg, Path::new("/tmp/m.toml"), None).unwrap();
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
        let args = build_run_args("docker", &cfg, Path::new("/tmp/m.toml"), None).unwrap();
        // Count -v args that mount to /home/pitboss/.claude — should be exactly 1
        // (the declared mount). The -w workdir may also reference the path but is
        // not a duplicate mount injection.
        let mount_count = args
            .windows(2)
            .filter(|w| w[0] == "-v" && w[1].contains(":/home/pitboss/.claude:"))
            .count();
        assert_eq!(mount_count, 1, "claude mount should appear exactly once: {args:?}");
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
        let args = build_run_args("docker", &cfg, Path::new("/tmp/m.toml"), None).unwrap();
        // -w should be followed by /project
        let w_pos = args.iter().position(|a| a == "-w").expect("-w flag");
        assert_eq!(args[w_pos + 1], "/project", "workdir: {args:?}");
    }

    #[test]
    fn workdir_falls_back_to_home_pitboss_when_no_mounts() {
        let cfg = make_config(vec![]);
        let args = build_run_args("docker", &cfg, Path::new("/tmp/m.toml"), None).unwrap();
        let w_pos = args.iter().position(|a| a == "-w").expect("-w flag");
        assert_eq!(args[w_pos + 1], "/home/pitboss", "fallback workdir: {args:?}");
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
        let args = build_run_args("docker", &cfg, Path::new("/tmp/m.toml"), None).unwrap();
        let w_pos = args.iter().position(|a| a == "-w").expect("-w flag");
        assert_eq!(args[w_pos + 1], "/project/sub", "explicit workdir: {args:?}");
    }

    #[test]
    fn extra_args_appear_before_image() {
        let cfg = ContainerConfig {
            extra_args: vec!["--network=host".into(), "--cap-drop=ALL".into()],
            ..ContainerConfig::default()
        };
        let args = build_run_args("docker", &cfg, Path::new("/tmp/m.toml"), None).unwrap();
        let net_pos = args
            .iter()
            .position(|a| a == "--network=host")
            .expect("--network");
        let img_pos = args
            .iter()
            .position(|a| a == DEFAULT_IMAGE)
            .expect("image");
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
            build_run_args("docker", &cfg, Path::new("/tmp/m.toml"), Some(custom.clone()))
                .unwrap();
        let joined = args.join(" ");
        assert!(
            joined.contains("/tmp/my-runs"),
            "run_dir override in mounts: {joined}"
        );
    }
}
