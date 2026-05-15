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

/// Cheap runtime probe for "are we inside a Docker/Podman container?".
/// Both runtimes write a marker file at one of these paths during
/// container init; the absence of both is reliable for "host process".
pub(crate) fn detect_in_container() -> bool {
    Path::new("/run/.containerenv").exists() || Path::new("/.dockerenv").exists()
}

/// Additional CLI args to pass to every `claude … -p` spawn so the host
/// operator's user-scope `~/.claude/settings.json` (hooks, etc.) doesn't
/// leak into containerized runs.
///
/// `--setting-sources project,local` excludes the `user` scope where
/// host hooks live, while keeping project- and local-scope settings
/// active. OAuth/keychain credentials are not loaded via setting-sources
/// so the bind-mounted `~/.claude/credentials.json` continues to work.
///
/// Returns an empty vec on the host so flat-mode `pitboss dispatch` is
/// unchanged. (#426)
pub(crate) fn claude_setting_sources_args(in_container: bool) -> Vec<String> {
    if in_container {
        vec!["--setting-sources".into(), "project,local".into()]
    } else {
        Vec::new()
    }
}

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
    // #266: surface the silent slow-path fallback. When extra_apt is
    // declared but no derived image exists, dispatch is correct but
    // slower than the operator likely intends. A single stderr warning
    // points at the fix (`pitboss container-build`) without changing
    // behavior.
    if let Some(msg) = super::container_build::derived_fallback_warning(
        container,
        derived.as_deref(),
        use_derived,
        manifest_path,
    ) {
        eprintln!("{msg}");
    }
    let derived_image_override = if use_derived {
        derived.as_deref()
    } else {
        None
    };

    let audit_runs_base = run_dir_override.clone().unwrap_or_else(default_run_dir);
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

    // Best-effort audit log: write the resolved container argv to
    // `<runs_base_dir>/container-dispatch.log` (NDJSON, one record per
    // invocation) before exec replaces the process. This is the only
    // post-facto trail of what flags the container actually received —
    // the inner `pitboss dispatch` writes its own run dir, but it
    // doesn't see the host-side container flags. Failure to write is
    // logged but does not abort dispatch (the operator's run shouldn't
    // hinge on the audit log being writable). (#476 / F-SEC-9)
    if let Err(e) =
        append_container_dispatch_audit(&audit_runs_base, &runtime, &args, &manifest_abs)
    {
        eprintln!("pitboss container-dispatch: warning: could not write argv audit log: {e}");
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
    // Required for OAuth auth (Linux) unless the operator already
    // declared a mount targeting /home/pitboss/.claude.
    //
    // F-SEC-11: the auto-inject defaults to `ro,z`. The host's
    // `~/.claude` holds OAuth credentials, `settings.json` (which may
    // include hooks), and other long-lived state. Mounting it
    // read-write into every container run means a compromised or
    // buggy worker can rewrite host credentials or inject malicious
    // hooks that would fire the next time claude runs on the host.
    // Default to read-only and provide an opt-in
    // (`[container].claude_mount_rw = true`) for the OAuth-refresh
    // scenario. An explicit `[[container.mount]]` for
    // /home/pitboss/.claude continues to win.
    let claude_container = PathBuf::from("/home/pitboss/.claude");
    if !covered_container_paths.contains(&claude_container) {
        if let Some(home) = home_dir() {
            let host_claude = home.join(".claude");
            let mode = if container.claude_mount_rw {
                "rw,z"
            } else {
                "ro,z"
            };
            args.push("-v".into());
            args.push(format!(
                "{}:{}:{mode}",
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

    // ── Control-bridge TCP forward (#474) ────────────────────────────────────
    // `pitboss-web` running on the host can't reach the dispatcher's
    // in-container AF_UNIX control socket on macOS+Podman (virtiofs's
    // `bind()` returns EINVAL; the `XDG_RUNTIME_DIR=/tmp` workaround
    // keeps the socket on container-overlay, which is unreachable from
    // the host). Auto-publish a host-loopback TCP forward instead: pick
    // a free 127.0.0.1 port, map it to the same port inside the
    // container, and tell the dispatcher to bind there via
    // PITBOSS_CONTROL_TCP_PORT. Harmless on Linux — the UNIX socket
    // continues to work and pitboss-web only consults the TCP arm when
    // meta.json's `control_tcp_addr` is populated.
    if let Some(port) = pick_free_loopback_port() {
        args.push("-p".into());
        args.push(format!("127.0.0.1:{port}:{port}"));
        args.push("-e".into());
        args.push(format!("PITBOSS_CONTROL_TCP_PORT={port}"));
    } else {
        eprintln!(
            "pitboss container-dispatch: warning: could not allocate a host \
             loopback port for the control bridge TCP forward; pitboss-web \
             Live/Graph/controls will be unavailable for this run."
        );
    }

    // ── macOS+Podman virtiofs AF_UNIX EINVAL workaround (#550) ───────────────
    // On macOS+Podman the auto-mounted runs dir is virtiofs-backed.
    // `control_socket_path` (and the per-run MCP socket path) fall back
    // to `<run_dir>/<run-id>/{control,mcp}.sock` when `$XDG_RUNTIME_DIR`
    // is unset inside the container — and the `pitboss-with-claude`
    // image doesn't set it. The fallback path is on virtiofs, where
    // `bind(AF_UNIX)` returns EINVAL. Redirect XDG_RUNTIME_DIR to
    // container-overlay `/tmp` so the sockets land somewhere bindable.
    // The TCP-forward auto-inject above (#474/#546) makes the control
    // bridge reachable from the host despite the socket being on
    // container-overlay; the MCP socket only needs to be in-container
    // reachable so this just works.
    //
    // Operator can override by setting `-e XDG_RUNTIME_DIR=...` in
    // `[container].extra_args` — we skip the auto-inject in that case.
    // Linux dispatches are untouched (host-Linux operators get their
    // systemd-provided XDG_RUNTIME_DIR forwarded through the runtime
    // and don't hit the virtiofs trap).
    #[cfg(target_os = "macos")]
    if !extra_args_overrides_env(&container.extra_args, "XDG_RUNTIME_DIR") {
        args.push("-e".into());
        args.push("XDG_RUNTIME_DIR=/tmp".into());
    }

    // ── Extra operator args ───────────────────────────────────────────────────
    // Defense in depth: `validate_extra_args` is also called from
    // `manifest::validate::validate_container`, but a test or
    // programmatic caller that skipped validate must not be able to
    // smuggle in `--privileged` (etc.). Validate before extending. (#476)
    validate_extra_args(&container.extra_args)?;
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

/// Pick an unused TCP port on 127.0.0.1 by asking the kernel for one.
/// Returns `None` on bind failure (e.g., the loopback interface is
/// somehow unavailable — should not happen in practice).
///
/// There is an inherent TOCTOU race between the listener drop and
/// podman's `-p` bind on the host side, but the window is short and the
/// alternative (parsing podman's verbose error output to retry) is much
/// more complex. If a collision does happen, the run starts but the
/// control-bridge TCP forward is broken — pitboss-web falls back to the
/// AF_UNIX path (which on macOS will still be unreachable, but that's a
/// pre-existing limitation we're trying to fix here, not regress). (#474)
fn pick_free_loopback_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

/// Check whether `extra_args` already sets an env var via the common
/// `-e KEY=VAL` / `-e KEY` / `--env KEY=VAL` / `--env=KEY=VAL` /
/// `-e=KEY=VAL` shapes. Used by the macOS+Podman `XDG_RUNTIME_DIR=/tmp`
/// auto-inject (#550) to honour explicit operator overrides without
/// emitting a duplicate `-e` in the audit log. Exotic forms (`--env-file`,
/// env vars set indirectly via wrapper shells) are not recognised — the
/// auto-inject just lays its default before `extra_args`, and Podman's
/// "last `-e` wins" semantics ensure a later operator-supplied
/// `-e XDG_RUNTIME_DIR=...` still overrides at runtime.
///
/// The function body is platform-agnostic (pure string parsing) and the
/// unit tests below run on every platform so the helper's behavior is
/// validated everywhere. Its only **production** caller lives under
/// `#[cfg(target_os = "macos")]` in `build_run_args`, so on non-macOS
/// lib builds the function is dead code from clippy's perspective —
/// silenced narrowly via `cfg_attr` rather than a blanket
/// `#[allow(dead_code)]` so a future regression on macOS (no remaining
/// caller) still fires the warning.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn extra_args_overrides_env(extra_args: &[String], key: &str) -> bool {
    let prefix = format!("{key}=");
    let mut iter = extra_args.iter();
    while let Some(arg) = iter.next() {
        // `-e KEY=VAL` / `-e KEY` / `--env KEY=VAL` / `--env KEY`
        if arg == "-e" || arg == "--env" {
            if let Some(next) = iter.next() {
                if next == key || next.starts_with(&prefix) {
                    return true;
                }
            }
            continue;
        }
        // `-e=KEY=VAL` / `--env=KEY=VAL`
        for joined_prefix in ["-e=", "--env="] {
            if let Some(rest) = arg.strip_prefix(joined_prefix) {
                if rest == key || rest.starts_with(&prefix) {
                    return true;
                }
            }
        }
    }
    false
}

/// Reject `extra_args` entries that defeat the container sandbox or
/// override pitboss-managed settings.
///
/// Validation runs in two places: at `pitboss validate` time (via
/// `manifest::validate::validate_container`) and again defensively in
/// `build_run_args` so a manifest that skipped validate (test fixtures,
/// programmatic callers) cannot bypass the gate.
///
/// The denylist enumerates **specific dangerous tokens** rather than
/// trying to whitelist the whole `docker run` flag surface. It covers
/// the four classes that matter:
///
/// 1. **Namespace breakouts** — `--pid=host`, `--ipc=host`, `--uts=host`,
///    `--userns=host`, `--cgroupns=host` and the `--pid=container:<id>`
///    join form. Each one defeats one of the namespaces the container
///    runtime is set up to provide.
/// 2. **Capability / privilege escalation** — `--privileged`,
///    `--cap-add=ALL`, `--cap-add=SYS_ADMIN`, `--cap-add=SYS_PTRACE`,
///    `--cap-add=SYS_MODULE` (parsed out of comma-separated lists).
///    `--security-opt={seccomp,apparmor}=unconfined` and
///    `--security-opt=label=disable` disable LSM enforcement.
/// 3. **Pitboss-managed flags** — `-v` / `--volume` / `--mount` go around
///    the validated `[[container.mount]]` path; `-u` / `--user` overrides
///    the UID alignment logic that the apt-bootstrap path depends on;
///    `--entrypoint` bypasses the `pitboss dispatch` entrypoint.
/// 4. **Host device passthrough** — `--device=…`, `--device-cgroup-rule=…`.
///
/// (#476 / F-SEC-9)
pub(crate) fn validate_extra_args(extra_args: &[String]) -> Result<()> {
    // (pattern, why) — exact-match denylist. Reasons appear verbatim in
    // the error so operators see why a flag was rejected.
    const EXACT: &[(&str, &str)] = &[
        ("--privileged", "grants the container near-root on the host"),
        (
            "-v",
            "use [[container.mount]] for bind mounts (path-validated)",
        ),
        (
            "--volume",
            "use [[container.mount]] for bind mounts (path-validated)",
        ),
        (
            "--mount",
            "use [[container.mount]] for bind mounts (path-validated)",
        ),
        (
            "-u",
            "pitboss controls UID alignment; do not override -u in extra_args",
        ),
        (
            "--user",
            "pitboss controls UID alignment; do not override --user in extra_args",
        ),
        (
            "--entrypoint",
            "the pitboss dispatch entrypoint must remain in place",
        ),
        ("--pid=host", "shares the host PID namespace"),
        ("--ipc=host", "shares the host IPC namespace"),
        ("--uts=host", "shares the host UTS namespace"),
        ("--userns=host", "disables UID namespacing"),
        ("--cgroupns=host", "shares the host cgroup namespace"),
        (
            "--security-opt=seccomp=unconfined",
            "disables seccomp filtering",
        ),
        ("--security-opt=apparmor=unconfined", "disables AppArmor"),
        ("--security-opt=label=disable", "disables SELinux labels"),
        ("--security-opt=label:disable", "disables SELinux labels"),
    ];
    // Prefix-match denylist for flags with attached values.
    const PREFIX: &[(&str, &str)] = &[
        (
            "--pid=container:",
            "joins another container's PID namespace",
        ),
        ("--device=", "exposes a host device into the container"),
        ("--device-cgroup-rule=", "rewrites the device cgroup rules"),
        (
            "--entrypoint=",
            "the pitboss dispatch entrypoint must remain in place",
        ),
        (
            "--volume=",
            "use [[container.mount]] for bind mounts (path-validated)",
        ),
        (
            "--user=",
            "pitboss controls UID alignment; do not override --user in extra_args",
        ),
    ];
    // `--cap-add` accepts comma-separated capability lists. Parse out
    // the individual caps so `--cap-add=NET_ADMIN,SYS_ADMIN` is caught.
    const FORBIDDEN_CAPS: &[&str] = &["ALL", "SYS_ADMIN", "SYS_PTRACE", "SYS_MODULE"];

    for arg in extra_args {
        for (pat, why) in EXACT {
            if arg == pat {
                bail!(
                    "[container].extra_args: {arg:?} is forbidden ({why}). \
                     If you genuinely need this, invoke docker/podman directly \
                     rather than through `pitboss container-dispatch`."
                );
            }
        }
        for (pat, why) in PREFIX {
            if arg.starts_with(pat) {
                bail!("[container].extra_args: {arg:?} is forbidden ({why}).");
            }
        }
        if let Some(rest) = arg.strip_prefix("--cap-add=") {
            for cap in rest.split(',') {
                let cap = cap.trim();
                if FORBIDDEN_CAPS.iter().any(|c| c.eq_ignore_ascii_case(cap)) {
                    bail!(
                        "[container].extra_args: --cap-add includes {cap:?} \
                         which is too broad. Forbidden caps: {FORBIDDEN_CAPS:?}. \
                         Use narrower caps (e.g. NET_ADMIN) or invoke docker/podman \
                         directly."
                    );
                }
            }
        }
    }
    Ok(())
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

/// Append a single NDJSON record to `<runs_base>/container-dispatch.log`
/// describing the about-to-exec container invocation. Used as a post-facto
/// audit trail for `extra_args` and other host-side container flags.
///
/// Format:
/// ```jsonc
/// {"at":"2026-05-14T12:34:56Z","manifest":"/abs/path/pitboss.toml",
///  "runtime":"podman","argv":["run","--rm",…]}
/// ```
fn append_container_dispatch_audit(
    runs_base: &Path,
    runtime: &str,
    args: &[String],
    manifest_abs: &Path,
) -> Result<()> {
    use std::io::Write;

    std::fs::create_dir_all(runs_base).with_context(|| {
        format!(
            "creating runs base dir for audit log: {}",
            runs_base.display()
        )
    })?;
    let log_path = runs_base.join("container-dispatch.log");

    let record = serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "manifest": manifest_abs.display().to_string(),
        "runtime": runtime,
        "argv": args,
    });

    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening audit log {}", log_path.display()))?;
    writeln!(f, "{record}")
        .with_context(|| format!("appending to audit log {}", log_path.display()))?;
    Ok(())
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

    #[test]
    fn claude_setting_sources_args_in_container_excludes_user_scope() {
        let argv = claude_setting_sources_args(true);
        assert_eq!(
            argv,
            vec!["--setting-sources".to_string(), "project,local".to_string()],
            "in-container claude spawn must drop user-scope settings (#426)"
        );
    }

    #[test]
    fn claude_setting_sources_args_on_host_is_empty() {
        // Flat-mode `pitboss dispatch` is unchanged — the operator's
        // host-side ~/.claude/settings.json continues to apply.
        assert!(claude_setting_sources_args(false).is_empty());
    }

    fn make_config(mounts: Vec<MountSpec>) -> ContainerConfig {
        ContainerConfig {
            mounts,
            ..ContainerConfig::default()
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
    fn auto_inject_claude_defaults_to_read_only() {
        // F-SEC-11: the auto-injected ~/.claude mount must be read-only
        // by default. A compromised worker cannot rewrite host
        // credentials or inject malicious hooks via the rw mount.
        let cfg = make_config(vec![]);
        let args = build_run_args("docker", &cfg, &temp_manifest(), None, None).unwrap();
        let claude_mount = args
            .windows(2)
            .find(|w| w[0] == "-v" && w[1].contains(":/home/pitboss/.claude:"))
            .expect("auto-injected claude mount must be present");
        assert!(
            claude_mount[1].ends_with(":ro,z"),
            "auto-injected ~/.claude mount must default to ro,z, got: {}",
            claude_mount[1]
        );
    }

    #[test]
    fn auto_inject_claude_uses_rw_when_opted_in() {
        // The operator can opt back into the pre-v0.15 rw mount for
        // OAuth-refresh scenarios via `[container].claude_mount_rw =
        // true`. This keeps the escape valve documented and testable.
        let cfg = ContainerConfig {
            claude_mount_rw: true,
            ..ContainerConfig::default()
        };
        let args = build_run_args("docker", &cfg, &temp_manifest(), None, None).unwrap();
        let claude_mount = args
            .windows(2)
            .find(|w| w[0] == "-v" && w[1].contains(":/home/pitboss/.claude:"))
            .expect("auto-injected claude mount must be present");
        assert!(
            claude_mount[1].ends_with(":rw,z"),
            "opt-in must produce rw,z, got: {}",
            claude_mount[1]
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
    fn extra_args_rejects_privileged() {
        // F-SEC-9: --privileged is the canonical sandbox-break flag.
        let cfg = ContainerConfig {
            extra_args: vec!["--privileged".into()],
            ..ContainerConfig::default()
        };
        let err = build_run_args("podman", &cfg, &temp_manifest(), None, None)
            .expect_err("--privileged must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("--privileged") && msg.contains("forbidden"),
            "error must name the flag and reason: {msg}"
        );
    }

    #[test]
    fn extra_args_rejects_namespace_host_flags() {
        // Each namespace-breakout flag rejected independently.
        for bad in [
            "--pid=host",
            "--ipc=host",
            "--uts=host",
            "--userns=host",
            "--cgroupns=host",
        ] {
            let cfg = ContainerConfig {
                extra_args: vec![bad.into()],
                ..ContainerConfig::default()
            };
            let err = build_run_args("podman", &cfg, &temp_manifest(), None, None)
                .expect_err(&format!("{bad} must be rejected"));
            assert!(
                err.to_string().contains(bad),
                "rejection must name the flag: {err}"
            );
        }
    }

    #[test]
    fn extra_args_rejects_v_mount_in_extra_args() {
        // -v / --volume / --mount are pitboss-managed via [[container.mount]];
        // operators may not smuggle a raw mount through extra_args.
        for bad in ["-v", "--volume", "--mount"] {
            let cfg = ContainerConfig {
                extra_args: vec![bad.into(), "/etc:/etc:rw".into()],
                ..ContainerConfig::default()
            };
            let err = build_run_args("podman", &cfg, &temp_manifest(), None, None)
                .expect_err(&format!("{bad} must be rejected"));
            assert!(
                err.to_string().contains("container.mount"),
                "rejection must point at the [[container.mount]] alternative: {err}"
            );
        }
    }

    #[test]
    fn extra_args_rejects_fused_volume_flag() {
        // The fused `--volume=src:dst` form is just as dangerous as the
        // standalone `-v src:dst` form — both must fail closed.
        let cfg = ContainerConfig {
            extra_args: vec!["--volume=/etc:/etc:rw".into()],
            ..ContainerConfig::default()
        };
        let err = build_run_args("podman", &cfg, &temp_manifest(), None, None)
            .expect_err("--volume=src:dst must be rejected");
        assert!(
            err.to_string().contains("--volume=/etc:/etc:rw"),
            "rejection must echo the offending arg: {err}"
        );
    }

    #[test]
    fn extra_args_rejects_dangerous_cap_add() {
        // --cap-add=SYS_ADMIN gives the container near-root authority.
        // Validation must split comma-separated lists so a sneaky
        // `NET_ADMIN,SYS_ADMIN` is caught.
        for bad in [
            "--cap-add=ALL",
            "--cap-add=SYS_ADMIN",
            "--cap-add=NET_ADMIN,SYS_ADMIN",
            "--cap-add=sys_admin",
        ] {
            let cfg = ContainerConfig {
                extra_args: vec![bad.into()],
                ..ContainerConfig::default()
            };
            let err = build_run_args("podman", &cfg, &temp_manifest(), None, None)
                .expect_err(&format!("{bad} must be rejected"));
            assert!(
                err.to_string().contains("cap-add"),
                "rejection must mention cap-add: {err}"
            );
        }
    }

    #[test]
    fn extra_args_rejects_user_override() {
        // -u / --user overrides the UID alignment that the apt-bootstrap
        // path depends on. The fused `--user=0:0` form must also fail.
        for bad in ["-u", "--user"] {
            let cfg = ContainerConfig {
                extra_args: vec![bad.into(), "0:0".into()],
                ..ContainerConfig::default()
            };
            assert!(
                build_run_args("podman", &cfg, &temp_manifest(), None, None).is_err(),
                "{bad} must be rejected"
            );
        }
        let cfg = ContainerConfig {
            extra_args: vec!["--user=0:0".into()],
            ..ContainerConfig::default()
        };
        assert!(
            build_run_args("podman", &cfg, &temp_manifest(), None, None).is_err(),
            "--user=0:0 must be rejected"
        );
    }

    #[test]
    fn extra_args_rejects_entrypoint_override() {
        for bad in ["--entrypoint", "--entrypoint=/bin/sh"] {
            let cfg = ContainerConfig {
                extra_args: vec![bad.into()],
                ..ContainerConfig::default()
            };
            assert!(
                build_run_args("podman", &cfg, &temp_manifest(), None, None).is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn extra_args_rejects_security_opt_unconfined() {
        for bad in [
            "--security-opt=seccomp=unconfined",
            "--security-opt=apparmor=unconfined",
            "--security-opt=label=disable",
        ] {
            let cfg = ContainerConfig {
                extra_args: vec![bad.into()],
                ..ContainerConfig::default()
            };
            assert!(
                build_run_args("podman", &cfg, &temp_manifest(), None, None).is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn extra_args_rejects_device_passthrough() {
        for bad in ["--device=/dev/kvm", "--device-cgroup-rule=a *:* rwm"] {
            let cfg = ContainerConfig {
                extra_args: vec![bad.into()],
                ..ContainerConfig::default()
            };
            assert!(
                build_run_args("podman", &cfg, &temp_manifest(), None, None).is_err(),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn extra_args_accepts_realistic_safe_flags() {
        // Common legitimate flags must pass: dns config, narrow caps,
        // resource limits, network override (commonly required in corp
        // firewall scenarios — broad but the operator chose this manifest).
        let cfg = ContainerConfig {
            extra_args: vec![
                "--network=host".into(),
                "--dns=10.0.0.53".into(),
                "--cap-drop=ALL".into(),
                "--cap-add=NET_ADMIN".into(),
                "--memory=4g".into(),
                "--cpus=2".into(),
                "--add-host=internal:10.0.0.1".into(),
            ],
            ..ContainerConfig::default()
        };
        build_run_args("podman", &cfg, &temp_manifest(), None, None)
            .expect("realistic safe extra_args must pass validation");
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

    // ── #550: XDG_RUNTIME_DIR=/tmp auto-inject on macOS ───────────────────

    /// Helper: count occurrences of an env `KEY=VAL` (or any value for
    /// the key) however it was passed — `-e KEY=VAL`, `--env KEY=VAL`,
    /// `-e=KEY=VAL`, or `--env=KEY=VAL`. Mirrors the shapes recognised
    /// by `extra_args_overrides_env` so suppression tests can assert
    /// "exactly one occurrence reached the final argv" no matter which
    /// form the operator used.
    fn count_env_inject(args: &[String], key: &str) -> usize {
        let prefix = format!("{key}=");
        let mut count = 0usize;
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if arg == "-e" || arg == "--env" {
                if let Some(next) = args.get(i + 1) {
                    if next == key || next.starts_with(&prefix) {
                        count += 1;
                    }
                }
                i += 2;
                continue;
            }
            for joined_prefix in ["-e=", "--env="] {
                if let Some(rest) = arg.strip_prefix(joined_prefix) {
                    if rest == key || rest.starts_with(&prefix) {
                        count += 1;
                    }
                }
            }
            i += 1;
        }
        count
    }

    /// On macOS hosts, `build_run_args` injects `-e XDG_RUNTIME_DIR=/tmp`
    /// so the in-container AF_UNIX bind for control + MCP sockets lands
    /// on container-overlay instead of the virtiofs-mounted runs dir.
    /// Without this, every default macOS+Podman dispatch fails with bare
    /// `Invalid argument (os error 22)`. (#550)
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_auto_injects_xdg_runtime_dir() {
        let cfg = ContainerConfig::default();
        let args = build_run_args("podman", &cfg, &temp_manifest(), None, None).unwrap();
        assert_eq!(
            count_env_inject(&args, "XDG_RUNTIME_DIR"),
            1,
            "macOS dispatch must inject exactly one XDG_RUNTIME_DIR=/tmp env to dodge \
             virtiofs+AF_UNIX EINVAL; argv: {args:?}"
        );
        // Land in the right shape, not just the right count.
        let pos = args
            .iter()
            .position(|a| a == "XDG_RUNTIME_DIR=/tmp")
            .expect("XDG_RUNTIME_DIR=/tmp present");
        assert_eq!(args[pos - 1], "-e", "must be preceded by `-e`");
    }

    /// On Linux hosts the same path must NOT inject — the operator
    /// expects their systemd-provided `$XDG_RUNTIME_DIR` to flow
    /// through, and Linux+Podman doesn't have the virtiofs trap. Forcing
    /// `/tmp` would shift the socket location and silently break
    /// host-side observability tools that look at the canonical path.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn linux_does_not_auto_inject_xdg_runtime_dir() {
        let cfg = ContainerConfig::default();
        let args = build_run_args("podman", &cfg, &temp_manifest(), None, None).unwrap();
        assert_eq!(
            count_env_inject(&args, "XDG_RUNTIME_DIR"),
            0,
            "Linux dispatch must NOT inject XDG_RUNTIME_DIR — operator's environment wins"
        );
    }

    /// An operator-supplied `XDG_RUNTIME_DIR` in `extra_args` wins —
    /// the auto-inject must not append a duplicate. Honours the
    /// audit-log expectation (one `-e XDG_RUNTIME_DIR=...` in the
    /// recorded argv) and prevents the audit reader from being
    /// surprised by a self-inflicted duplicate.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_auto_inject_respects_extra_args_override() {
        let cfg = ContainerConfig {
            extra_args: vec!["-e".into(), "XDG_RUNTIME_DIR=/run/custom".into()],
            ..ContainerConfig::default()
        };
        let args = build_run_args("podman", &cfg, &temp_manifest(), None, None).unwrap();
        assert_eq!(
            count_env_inject(&args, "XDG_RUNTIME_DIR"),
            1,
            "operator override in extra_args must suppress the auto-inject; argv: {args:?}"
        );
        // And the surviving value is the operator's, not our default.
        assert!(
            args.iter().any(|a| a == "XDG_RUNTIME_DIR=/run/custom"),
            "operator value must survive: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "XDG_RUNTIME_DIR=/tmp"),
            "default value must not appear: {args:?}"
        );
    }

    /// `--env KEY=VAL` (long form) is recognised by the override-detector.
    /// Same as the `-e` case — must suppress the auto-inject. Pin all
    /// the shapes the detector handles so a future shrinkage of the
    /// detector doesn't silently regress override support.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_auto_inject_respects_long_form_env_override() {
        let cfg = ContainerConfig {
            extra_args: vec!["--env".into(), "XDG_RUNTIME_DIR=/x".into()],
            ..ContainerConfig::default()
        };
        let args = build_run_args("podman", &cfg, &temp_manifest(), None, None).unwrap();
        assert_eq!(count_env_inject(&args, "XDG_RUNTIME_DIR"), 1);
        assert!(args.iter().any(|a| a == "XDG_RUNTIME_DIR=/x"));
    }

    /// `-e=KEY=VAL` (joined form, less common but legal) — also
    /// recognised. Some operators write env this way; the auto-inject
    /// must not duplicate.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_auto_inject_respects_joined_form_env_override() {
        let cfg = ContainerConfig {
            extra_args: vec!["-e=XDG_RUNTIME_DIR=/joined".into()],
            ..ContainerConfig::default()
        };
        let args = build_run_args("podman", &cfg, &temp_manifest(), None, None).unwrap();
        assert!(
            !args.iter().any(|a| a == "XDG_RUNTIME_DIR=/tmp"),
            "default must be suppressed by `-e=` joined override: {args:?}"
        );
    }

    /// Unit-test the override detector directly so its specific shape
    /// recognition is verifiable independent of `build_run_args`. The
    /// caller's invariant: setting a different env var (not the
    /// target key) does NOT suppress the auto-inject.
    #[test]
    fn extra_args_overrides_env_only_matches_the_target_key() {
        assert!(extra_args_overrides_env(
            &["-e".into(), "FOO=bar".into()],
            "FOO"
        ));
        assert!(extra_args_overrides_env(
            &["--env".into(), "FOO=bar".into()],
            "FOO"
        ));
        assert!(extra_args_overrides_env(&["-e=FOO=bar".into()], "FOO"));
        assert!(extra_args_overrides_env(&["--env=FOO=bar".into()], "FOO"));
        // Passthrough form: `-e FOO` (value taken from host env)
        // is also an override — operator clearly cares about this var.
        assert!(extra_args_overrides_env(
            &["-e".into(), "FOO".into()],
            "FOO"
        ));
        // Negative: a different key must not match.
        assert!(!extra_args_overrides_env(
            &["-e".into(), "OTHER=v".into()],
            "FOO"
        ));
        // Negative: empty extra_args.
        assert!(!extra_args_overrides_env(&[], "FOO"));
        // Negative: prefix collision (`FOOBAR` is not `FOO`).
        assert!(!extra_args_overrides_env(
            &["-e".into(), "FOOBAR=v".into()],
            "FOO"
        ));
    }
}
