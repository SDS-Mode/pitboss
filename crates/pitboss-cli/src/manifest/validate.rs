#![allow(dead_code)]

use std::collections::HashSet;
use std::path::Path;

use anyhow::{anyhow, bail, Error, Result};

use super::resolve::ResolvedManifest;

/// Run all manifest validations. Call after [`crate::manifest::resolve::resolve`].
pub fn validate(resolved: &ResolvedManifest) -> Result<()> {
    validate_inner(resolved, false)
}

/// Like `validate` but skips the directory-existence check.
/// Used by `container-dispatch` where task `directory` fields are
/// container-side paths that don't exist on the host.
pub fn validate_skip_dir_check(resolved: &ResolvedManifest) -> Result<()> {
    validate_inner(resolved, true)
}

fn validate_inner(resolved: &ResolvedManifest, skip_dir_check: bool) -> Result<()> {
    validate_mode(resolved)?;
    for cfg in &resolved.notifications {
        crate::notify::config::validate(cfg)?;
    }
    validate_lifecycle(resolved)?;
    validate_container(resolved)?;
    validate_communication(resolved)?;
    validate_actor_types(resolved)?;
    validate_block_untyped_requires_profiles(resolved)?;
    validate_mcp_server_scopes(resolved)?;
    validate_mcp_server_tools(resolved)?;
    validate_mcp_tool_consistency(resolved)?;
    validate_agent_profiles(resolved)?;
    validate_claude_setting_sources(resolved)?;
    if resolved.lead.is_some() {
        validate_lead(resolved, skip_dir_check)?;
        validate_hierarchical_ranges(resolved)?;
        validate_sublead_defaults_adequate(resolved)?;
    } else {
        validate_ids(resolved)?;
        if !skip_dir_check {
            validate_directories(resolved)?;
        }
        validate_branch_conflicts(resolved)?;
        validate_ranges(resolved)?;
    }
    Ok(())
}

/// Validate `[[worker_type]]` / `[[sublead_type]]` profile invariants.
///
/// - `id` must match `^[A-Za-z0-9_-]+$`. The id appears in error
///   messages returned to the model and in `summary.jsonl`; restricting
///   to a portable charset avoids quoting puzzles in both surfaces.
/// - Ids must be unique within their kind (worker_type / sublead_type).
///   A duplicate would silently shadow the earlier definition since
///   the lookup is "first match wins"; rejecting at validate time
///   surfaces the typo loudly.
/// - `require_actor_type = true` requires AT LEAST ONE profile of the
///   relevant kind to exist (otherwise the run can spawn nothing).
///   Flat-mode ignores both surfaces — profiles only apply to the
///   hierarchical spawn path.
///
/// (#252)
fn validate_actor_types(r: &ResolvedManifest) -> Result<()> {
    fn is_valid_id(s: &str) -> bool {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }

    let mut worker_ids: HashSet<&str> = HashSet::new();
    for wt in &r.worker_types {
        if !is_valid_id(&wt.id) {
            bail!(
                "[[worker_type]].id {:?}: must match ^[A-Za-z0-9_-]+$ \
                 (allowed: ASCII alphanumeric, underscore, hyphen)",
                wt.id
            );
        }
        if !worker_ids.insert(&wt.id) {
            bail!(
                "[[worker_type]].id {:?}: duplicate; ids must be unique \
                 within `[[worker_type]]` (the lookup is first-match-wins, \
                 a duplicate would silently shadow the earlier entry)",
                wt.id
            );
        }
    }

    let mut sublead_ids: HashSet<&str> = HashSet::new();
    for st in &r.sublead_types {
        if !is_valid_id(&st.id) {
            bail!(
                "[[sublead_type]].id {:?}: must match ^[A-Za-z0-9_-]+$ \
                 (allowed: ASCII alphanumeric, underscore, hyphen)",
                st.id
            );
        }
        if !sublead_ids.insert(&st.id) {
            bail!(
                "[[sublead_type]].id {:?}: duplicate; ids must be unique \
                 within `[[sublead_type]]`",
                st.id
            );
        }
    }

    if r.require_actor_type {
        // The flag is only meaningful in hierarchical mode (it gates
        // spawn_worker / spawn_sublead). In flat mode the flag is
        // inert — we don't reject it, but we do warn so an operator
        // who set it expecting any effect notices.
        if r.lead.is_none() {
            tracing::warn!(
                "[run].require_actor_type = true is ignored in flat mode \
                 (no [lead] declared); the flag only applies to hierarchical \
                 spawn_worker / spawn_sublead calls"
            );
        } else if r.worker_types.is_empty() && r.sublead_types.is_empty() {
            bail!(
                "[run].require_actor_type = true but no `[[worker_type]]` \
                 or `[[sublead_type]]` declared; the flag would reject every \
                 spawn call. Declare at least one profile or set \
                 require_actor_type = false."
            );
        }
    }

    Ok(())
}

/// Validate `[[agent_profile]]` invariants and references.
///
/// - Charset: every id must match `^[A-Za-z0-9_/-]+$`. The id appears
///   in error messages and in `pitboss schema --format=agent-profiles`
///   output; restricting the charset keeps quoting/escaping
///   predictable across both surfaces.
/// - Namespace: the `pitboss/` prefix is reserved for bundled
///   built-ins. A manifest may *shadow* a built-in by matching its id
///   exactly, but may not introduce novel `pitboss/<other>` ids.
/// - References: every `agent_profile = "<id>"` mention on
///   `[[worker_type]]` / `[[sublead_type]]` must resolve to a built-in
///   or a manifest-declared id. Dangling refs on `[lead]` / `[[task]]`
///   are caught earlier at resolve time (where the lookup happens
///   inline); the worker_type / sublead_type refs survive in the
///   resolved manifest and are checked here.
///
/// Duplicate manifest ids are caught at resolve time so the catalogue
/// HashMap can't silently swallow them.
fn validate_agent_profiles(r: &ResolvedManifest) -> Result<()> {
    fn is_valid_id(s: &str) -> bool {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '/')
    }

    let builtin_ids: HashSet<String> = crate::manifest::builtin_profiles::load_builtins()
        .into_iter()
        .map(|p| p.id)
        .collect();

    for id in r.agent_profiles.keys() {
        if !is_valid_id(id) {
            bail!(
                "[[agent_profile]].id {:?}: must match ^[A-Za-z0-9_/-]+$ \
                 (allowed: ASCII alphanumeric, underscore, hyphen, forward slash)",
                id
            );
        }
        if id.starts_with("pitboss/") && !builtin_ids.contains(id) {
            let mut ids: Vec<&str> = builtin_ids.iter().map(String::as_str).collect();
            ids.sort();
            bail!(
                "[[agent_profile]].id {:?}: the `pitboss/` namespace is \
                 reserved for built-in profiles. A manifest may shadow a \
                 built-in by matching its id exactly, but may not introduce \
                 novel `pitboss/<other>` ids. Known built-ins: {}",
                id,
                ids.join(", ")
            );
        }
    }

    let known: HashSet<&str> = r.agent_profiles.keys().map(String::as_str).collect();
    let check_ref = |surface: &str, maybe_ref: Option<&str>| -> Result<()> {
        let Some(id) = maybe_ref else {
            return Ok(());
        };
        if !known.contains(id) {
            let mut ids: Vec<&str> = known.iter().copied().collect();
            ids.sort();
            bail!(
                "{surface}: agent_profile {id:?} not found. Declared profiles \
                 (built-ins + manifest): {}. See `pitboss schema --format=agent-profiles`.",
                ids.join(", ")
            );
        }
        Ok(())
    };
    for wt in &r.worker_types {
        check_ref(
            &format!("[[worker_type]] id={:?}", wt.id),
            wt.agent_profile.as_deref(),
        )?;
    }
    for st in &r.sublead_types {
        check_ref(
            &format!("[[sublead_type]] id={:?}", st.id),
            st.agent_profile.as_deref(),
        )?;
    }
    Ok(())
}

/// Validate `[[mcp_server]].scope` strings against the declared profile
/// catalogue (#252 Phase 1.5).
///
/// - Unset → always valid (server injects into all actors, the legacy
///   v0.11 default).
/// - Set → must parse as `type:<id>` exactly. The `<id>` must reference
///   either a `[[worker_type]]` or a `[[sublead_type]]`. Any other prefix
///   form is reserved for future expansion (`role:`, `actor:`, ...) and
///   rejected today.
///
/// We reject loudly at validate time because a typo here would silently
/// strip the server from every typed actor (the runtime filter would
/// fail-closed against an unknown id), and operators would see "MCP
/// server X never injected" with no obvious cause.
fn validate_mcp_server_scopes(r: &ResolvedManifest) -> Result<()> {
    let mut known: HashSet<&str> = HashSet::new();
    for wt in &r.worker_types {
        known.insert(&wt.id);
    }
    for st in &r.sublead_types {
        known.insert(&st.id);
    }

    for s in &r.mcp_servers {
        let Some(raw) = s.scope.as_deref() else {
            continue;
        };
        let Some(target) = raw.strip_prefix("type:") else {
            bail!(
                "[[mcp_server]].scope {:?} on server {:?}: only the \
                 \"type:<id>\" form is currently supported",
                raw,
                s.id
            );
        };
        if target.is_empty() {
            bail!(
                "[[mcp_server]].scope {:?} on server {:?}: empty id \
                 after \"type:\" prefix",
                raw,
                s.id
            );
        }
        if !known.contains(target) {
            bail!(
                "[[mcp_server]].scope {:?} on server {:?}: id {:?} does \
                 not match any `[[worker_type]]` or `[[sublead_type]]`. \
                 Declare the profile or fix the typo.",
                raw,
                s.id,
                target
            );
        }
    }

    Ok(())
}

/// Validate the per-server `[[mcp_server]].tools` allowlist shape
/// (#391 slice 4 / #397 — Path 2). This is the manifest-time enforcement
/// half: shape checks only. Runtime enforcement (intersecting with the
/// actor's `--allowedTools` at spawn time, or denying through Path B's
/// `permission_prompt`) is a follow-up — see #397.
///
/// - Unset → no restriction (the v0.12 default).
/// - `Some([])` → reject as self-defeating: the operator declared the
///   `[[mcp_server]]` block then declared zero tools, which is
///   indistinguishable in intent from "remove the server entirely" but
///   silent. Force the explicit form.
/// - Empty / whitespace tool names → reject; would never match a real
///   tool and likely indicates a typo'd quote pair.
/// - Duplicate tool names within one server → reject; ambiguous intent
///   ("did the operator mean to allowlist the tool twice or is one a
///   typo?") and the dedup would silently lose information either way.
fn validate_mcp_server_tools(r: &ResolvedManifest) -> Result<()> {
    for s in &r.mcp_servers {
        let Some(tools) = s.tools.as_ref() else {
            continue;
        };
        if tools.is_empty() {
            bail!(
                "[[mcp_server]].tools on server {:?}: empty list is \
                 self-defeating (allowlists no tools but keeps the server \
                 declared). Either remove the [[mcp_server]] block or list \
                 the tools you want to admit.",
                s.id
            );
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for tool in tools {
            if tool.trim().is_empty() {
                bail!(
                    "[[mcp_server]].tools on server {:?}: empty / whitespace \
                     tool name is not allowed (likely a typo'd quote pair).",
                    s.id
                );
            }
            if !seen.insert(tool.as_str()) {
                bail!(
                    "[[mcp_server]].tools on server {:?}: duplicate tool {:?} \
                     in allowlist; remove the duplicate so the manifest's \
                     intent is unambiguous.",
                    s.id,
                    tool
                );
            }
        }
    }
    Ok(())
}

/// Validate that every actor-declared MCP tool reference is consistent
/// with the per-server `tools` allowlist (#391 slice 4).
///
/// Walks `[lead].tools`, `[[task]].tools`, `[[worker_type]].tools`, and
/// `[[sublead_type]].tools`. For any entry that resolves via
/// [`crate::manifest::mcp_tools::parse_mcp_tool_name`] to a server with
/// a declared `tools` allowlist, the parsed tool name must be in the
/// allowlist. Otherwise reject with a message naming both the actor
/// surface and the offending allowlist — operators see exactly which
/// line to fix.
///
/// The parsing logic (longest-prefix-match against declared server ids)
/// is shared with the spawn-time filter and the runtime
/// `permission_prompt` short-circuit so the three sites cannot drift.
fn validate_mcp_tool_consistency(r: &ResolvedManifest) -> Result<()> {
    use crate::manifest::mcp_tools::parse_mcp_tool_name;

    let mut surfaces: Vec<(String, &[String])> = Vec::new();
    if let Some(lead) = &r.lead {
        surfaces.push(("[lead].tools".to_string(), &lead.tools));
    }
    for t in &r.tasks {
        surfaces.push((format!("[[task]] {:?}.tools", t.id), &t.tools));
    }
    for wt in &r.worker_types {
        surfaces.push((format!("[[worker_type]] {:?}.tools", wt.id), &wt.tools));
    }
    for st in &r.sublead_types {
        surfaces.push((format!("[[sublead_type]] {:?}.tools", st.id), &st.tools));
    }

    for (surface, tools) in surfaces {
        for entry in tools {
            let Some(parsed) = parse_mcp_tool_name(entry, &r.mcp_servers) else {
                continue;
            };
            if parsed.is_admitted() {
                continue;
            }
            // Safe to unwrap: `is_admitted` returned false, which only
            // happens when the allowlist is `Some(_)`.
            let allow = parsed.allowlist.unwrap();
            bail!(
                "{surface}: tool {entry:?} references mcp_server {:?} \
                 which declares tools = {allow:?}. Tool {:?} is \
                 not in that allowlist. Either add {:?} to the \
                 server's `tools = […]`, remove the entry from {surface}, \
                 or drop the server's `tools` allowlist.",
                parsed.server_id,
                parsed.tool_name,
                parsed.tool_name
            );
        }
    }
    Ok(())
}

/// Reject zero ceilings on `[communication]`. Zero would silently disable
/// the corresponding cap, which is a footgun (a max-bytes value of 0
/// would reject every send). Operators wanting to disable the surface
/// entirely set `mode = "disabled"`, not zero ceilings.
fn validate_communication(r: &ResolvedManifest) -> Result<()> {
    if r.communication.max_message_bytes == 0 {
        bail!("[communication].max_message_bytes must be > 0");
    }
    if r.communication.max_artifact_bytes == 0 {
        bail!("[communication].max_artifact_bytes must be > 0");
    }
    if r.communication.max_artifacts_per_actor == 0 {
        bail!("[communication].max_artifacts_per_actor must be > 0");
    }
    Ok(())
}

/// Validate `[run].claude_setting_sources` (#555). The string must be a
/// comma-separated subset of `user`, `project`, `local`. Empty string,
/// unknown tokens, and duplicate tokens are rejected with a hint. An
/// omitted (`None`) field passes through — that's the back-compat path
/// where pitboss applies its built-in defaults (filter in container,
/// no filter on host).
fn validate_claude_setting_sources(r: &ResolvedManifest) -> Result<()> {
    let Some(value) = r.claude_setting_sources.as_deref() else {
        return Ok(());
    };
    if value.is_empty() {
        bail!(
            "[run].claude_setting_sources: empty string is not a valid \
             value; either omit the field (pitboss applies its default) \
             or set a comma-separated subset of \"user\", \"project\", \
             \"local\""
        );
    }
    // Reject whitespace inside the value. The string is forwarded
    // verbatim to claude as `--setting-sources <value>`; a space-
    // padded form like `"project, local"` would be passed through as
    // a literal flag value and either rejected by claude or silently
    // misparsed. Rejecting at validate time keeps the contract simple
    // (the value the operator wrote is the value claude receives) and
    // surfaces the typo immediately rather than at dispatch.
    if value.chars().any(char::is_whitespace) {
        bail!(
            "[run].claude_setting_sources = {value:?}: contains whitespace; \
             use the canonical comma-separated form with no spaces \
             (e.g. \"project,local\"), not \"project, local\""
        );
    }
    let mut seen = std::collections::BTreeSet::new();
    for token in value.split(',') {
        // No `.trim()` — whitespace is already rejected above; if a
        // token is empty here, it's because of a `,,` or trailing/
        // leading comma, which is the empty-token case below.
        if token.is_empty() {
            bail!(
                "[run].claude_setting_sources = {value:?}: contains an empty \
                 token (e.g. trailing comma); use a comma-separated subset of \
                 \"user\", \"project\", \"local\" with no empty entries"
            );
        }
        match token {
            "user" | "project" | "local" => {}
            other => bail!(
                "[run].claude_setting_sources = {value:?}: unknown token \
                 {other:?}; valid tokens are \"user\", \"project\", \"local\" \
                 (claude-code's --setting-sources flag)"
            ),
        }
        if !seen.insert(token.to_string()) {
            bail!(
                "[run].claude_setting_sources = {value:?}: token {token:?} \
                 appears more than once"
            );
        }
    }
    Ok(())
}

/// Reject manifest-level container misconfigurations that would otherwise
/// only surface at dispatch / build time. Covers:
/// - `[container].extra_apt` package names must be shell-safe.
/// - `[[container.copy]].container` must be an absolute path (Dockerfile
///   `COPY <src> <dest>` rejects relative dests when WORKDIR is unset for
///   the FROM line, and we want the schema to be unambiguous).
/// - `[[container.copy]].host` should be an absolute path or a tilde-
///   prefixed path; relative host paths can't be resolved without a
///   manifest-relative anchor that pitboss doesn't currently track.
fn validate_container(r: &ResolvedManifest) -> Result<()> {
    let Some(container) = &r.container else {
        return Ok(());
    };
    for pkg in &container.extra_apt {
        if !crate::dispatch::container::is_valid_apt_pkg(pkg) {
            bail!(
                "[container].extra_apt: invalid package name {pkg:?} \
                 (allowed: ASCII alphanumeric, `.`, `+`, `-`; must start \
                 with alphanumeric)"
            );
        }
    }
    // F-SEC-9: deny dangerous container runtime flags. Same helper that
    // `build_run_args` re-checks at dispatch time so test fixtures and
    // programmatic callers cannot bypass.
    crate::dispatch::container::validate_extra_args(&container.extra_args)?;
    for spec in &container.copy {
        if !spec.container.is_absolute() {
            bail!(
                "[[container.copy]].container must be an absolute path, got {:?}",
                spec.container
            );
        }
        // Host path: accept tilde-prefixed paths (expanded at build time)
        // OR absolute paths. Reject relative paths since there is no
        // canonical anchor (the manifest may live anywhere).
        let host_str = spec.host.to_string_lossy();
        let is_tilde = host_str.starts_with('~');
        if !spec.host.is_absolute() && !is_tilde {
            bail!(
                "[[container.copy]].host must be an absolute or tilde-prefixed path, got {:?}",
                spec.host
            );
        }
    }
    // F-SEC-10: validate [[container.mount]] entries the same way as
    // [[container.copy]] above. The previous code only validated copy.
    for spec in &container.mounts {
        if !spec.container.is_absolute() {
            bail!(
                "[[container.mount]].container must be an absolute path, got {:?}",
                spec.container
            );
        }
        // Host: same rule as copy — absolute or tilde-prefixed only.
        // Relative paths have no canonical anchor in the manifest schema.
        let host_str = spec.host.to_string_lossy();
        let is_tilde = host_str.starts_with('~');
        if !spec.host.is_absolute() && !is_tilde {
            bail!(
                "[[container.mount]].host must be an absolute or tilde-prefixed path, got {:?}",
                spec.host
            );
        }
        // Hard-reject host paths that would defeat the container
        // sandbox. The denylist is short: each entry is either a known
        // container-escape vector (docker/podman socket → root on the
        // host) or a wholesale namespace bypass (/, /proc, /sys).
        //
        // /etc, /var/log, /home/<other-user> are NOT in the denylist —
        // they are sensitive but operators have legitimate use cases
        // (config reads, log capture, peer-user collaboration). The
        // line is "would a buggy/malicious worker get host-root or
        // host-namespace visibility from this mount?".
        check_mount_host_path(&host_str)?;
    }
    Ok(())
}

/// Reject host paths that defeat container isolation. Called per-mount
/// from `validate_container`. (#524 / F-SEC-10)
///
/// The check is exact-match (after lossy stringification) for socket
/// paths and full-tree mounts. Substring matching would over-block
/// legitimate workspace paths like `/data/proc-recordings`.
fn check_mount_host_path(host: &str) -> Result<()> {
    // Trim any trailing slash so `/proc` and `/proc/` are equivalent.
    let trimmed = host.trim_end_matches('/');
    let denylist: &[(&str, &str)] = &[
        (
            "/var/run/docker.sock",
            "the docker socket grants root on the host",
        ),
        (
            "/run/docker.sock",
            "the docker socket grants root on the host",
        ),
        (
            "/var/run/podman/podman.sock",
            "the podman socket grants root on the host",
        ),
        (
            "/run/podman/podman.sock",
            "the podman socket grants root on the host",
        ),
        ("/proc", "exposes the host process namespace"),
        (
            "/sys",
            "exposes host kernel sysfs (a common container-breakout vector)",
        ),
        ("/", "mounts the entire host root filesystem"),
        ("/etc/shadow", "exposes the host root password hashes"),
    ];
    for (bad, why) in denylist {
        if trimmed == *bad || trimmed == bad.trim_end_matches('/') {
            bail!(
                "[[container.mount]].host = {host:?} is forbidden ({why}). \
                 If you genuinely need this mount, run docker/podman \
                 directly rather than through `pitboss container-dispatch`."
            );
        }
    }
    Ok(())
}

/// Enforce the `[lifecycle]` coupling rule (issue #133): a manifest that
/// declares `survive_parent = true` must declare at least one notification
/// target so the orchestrator can observe the run's outcome after losing
/// process-level control of it. Either an inline `[lifecycle].notify` or a
/// top-level `[[notification]]` section satisfies the rule.
///
/// Validates the inline notify spec (when present) using the same SSRF /
/// scheme rules as `[[notification]]`. The error explicitly points at the
/// fix so an operator using only `PITBOSS_PARENT_NOTIFY_URL` knows to add a
/// no-cost `kind = "log"` notification block.
fn validate_lifecycle(r: &ResolvedManifest) -> Result<()> {
    let Some(lifecycle) = &r.lifecycle else {
        return Ok(());
    };
    if let Some(notify) = &lifecycle.notify {
        crate::notify::config::validate(notify)?;
    }
    if lifecycle.survive_parent {
        let has_inline_notify = lifecycle.notify.is_some();
        let has_top_level_notify = !r.notifications.is_empty();
        if !has_inline_notify && !has_top_level_notify {
            bail!(
                "[lifecycle] survive_parent = true requires a notification target so the \
                 orchestrator can observe run completion. Add either:\n  \
                 - a [lifecycle.notify] inline sink (same shape as [[notification]]), or\n  \
                 - at least one top-level [[notification]] section.\n\n\
                 If you intend to deliver lifecycle events via the \
                 PITBOSS_PARENT_NOTIFY_URL env var, add a no-cost log block to satisfy \
                 this validate-time gate (the env-var sink is configured at dispatch-time \
                 and validate cannot see it):\n\n    \
                 [[notification]]\n    \
                 kind = \"log\"\n"
            );
        }
    }
    Ok(())
}

fn validate_mode(r: &ResolvedManifest) -> Result<()> {
    if !r.tasks.is_empty() && r.lead.is_some() {
        bail!("cannot combine [[task]] and [lead] in the same manifest");
    }
    if r.tasks.is_empty() && r.lead.is_none() {
        bail!("empty manifest: define either [[task]] entries or exactly one [lead]");
    }
    Ok(())
}

/// Returns a one-line warning message when Path B is in effect but the
/// manifest declares no `[[worker_type]]` / `[[sublead_type]]` profiles.
///
/// The runtime gate (#388) only fast-paths typed actors via their
/// profile's `tools` allowlist; un-typed callers still bridge-route
/// every `permission_prompt`. Operators who selected Path B without
/// profiles are not getting the headline payoff of the routing change
/// — they're just paying the bridge round-trip on every tool that
/// claude can't pre-approve via `--allowedTools`. Pure nudge: returns
/// `None` (no warn) when at least one profile is declared, so a
/// partially-migrated manifest doesn't keep firing a stale advisory.
///
/// Returned as a string rather than emitted via `tracing::warn!` so
/// unit tests can assert the trigger conditions without capturing a
/// global subscriber. The validate-time caller logs it.
pub(crate) fn path_b_profile_migration_warning(r: &ResolvedManifest) -> Option<String> {
    if !r.worker_types.is_empty() || !r.sublead_types.is_empty() {
        return None;
    }
    Some(
        "`permission_routing = \"path_b\"` is selected but the manifest declares \
         no `[[worker_type]]` / `[[sublead_type]]` profiles. Typed actors get a \
         fast-path auto-approve via their `tools` allowlist (#388); un-typed \
         actors route every per-tool check through the operator approval bridge \
         on first use. Declare profiles to fast-path common tools and audit \
         out-of-policy requests as `denied_by_profile`. \
         Run `pitboss schema --format=migration` for a starter scaffold."
            .to_string(),
    )
}

/// Reject `[run].untyped_actor_policy = "block"` when no profiles are
/// declared. The combination is self-defeating: every un-typed call
/// site (which under `block` is also every spawn unless every spawn
/// names a profile) would auto-deny via the synthetic empty profile,
/// so the lead can spawn nothing useful. Bailing at validate time
/// saves a confused operator from a dispatch that produces only
/// `denied_by_profile` events.
fn validate_block_untyped_requires_profiles(r: &ResolvedManifest) -> Result<()> {
    use crate::manifest::schema::UntypedActorPolicy;
    if !matches!(r.untyped_actor_policy, UntypedActorPolicy::Block) {
        return Ok(());
    }
    if r.worker_types.is_empty() && r.sublead_types.is_empty() {
        bail!(
            "[run].untyped_actor_policy = \"block\" requires at least one \
             `[[worker_type]]` or `[[sublead_type]]` profile to be declared. \
             Without any profiles, every spawn_worker / spawn_sublead call \
             would auto-deny via `denied_by_profile`. Either declare a \
             profile (run `pitboss schema --format=migration` for a \
             starter) or set `untyped_actor_policy = \"bridge\"` (the \
             default) to keep the operator approval bridge in the loop."
        );
    }
    Ok(())
}

fn validate_lead(r: &ResolvedManifest, skip_dir_check: bool) -> Result<()> {
    let lead = r.lead.as_ref().unwrap();

    // Path B is the default routing as of v0.12. The migration warning
    // still fires (#389) when no profiles are declared, since that's
    // the actionable advisory — operators see "you're on Path B
    // without profiles, here's how to declare them" exactly when they
    // need it. The previous "Path B is in soak" warning was retired
    // when Path B became the default; it would have fired on every
    // run.
    if matches!(
        lead.permission_routing,
        crate::manifest::schema::PermissionRouting::PathB
    ) {
        if let Some(msg) = path_b_profile_migration_warning(r) {
            tracing::warn!("{msg}");
        }
    }

    // A lead with an empty prompt starts, receives no `-p` argument, and exits
    // with code 1 in ~700ms. Common cause: the `prompt =` key appears after a
    // subtable declaration in the TOML source, which moves it out of the
    // `[lead]` scope — TOML silently assigns it to the subtable or drops it.
    if lead.prompt.trim().is_empty() {
        bail!(
            "lead '{}': prompt is required but is empty. Ensure `prompt =` \
             appears before any subtable declaration (e.g. `[lead.env]`) in \
             the TOML source.",
            lead.id
        );
    }
    if lead.id.is_empty() {
        bail!("lead id is required");
    }
    if !lead
        .id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!(
            "lead id '{}' contains invalid characters (allowed: a-zA-Z0-9_-)",
            lead.id
        );
    }
    if !skip_dir_check && !lead.directory.is_dir() {
        bail!(
            "lead directory does not exist: {}",
            lead.directory.display()
        );
    }
    // skip_dir_check also gates the git-repo probe — container-dispatch
    // manifests use container-side paths (`/workspace/foo`) that don't
    // exist on the host, so `git2::Repository::discover` would always
    // fail and reject every hierarchical container manifest with the
    // default `use_worktree = true`. Mirrors the existing flat-mode
    // gating in `validate_directories`.
    if !skip_dir_check && lead.use_worktree && !is_in_git_repo(&lead.directory) {
        bail!(
            "lead has use_worktree=true but directory is not a git work-tree: {}",
            lead.directory.display()
        );
    }
    if lead.timeout_secs == 0 {
        bail!("[lead].timeout_secs must be > 0 (per-actor wall-clock cap)");
    }
    Ok(())
}

/// If `[lead] allow_subleads = true`, the manifest must also either supply
/// both `sublead_defaults.budget_usd` + `sublead_defaults.max_workers` (for
/// Owned-envelope default), or set `sublead_defaults.read_down = true` (for
/// SharedPool default). Otherwise a runtime `spawn_sublead` call that omits
/// those fields will fail with "budget_usd required when read_down=false"
/// mid-dispatch.
///
/// Note: this is a *required-defaults* check, distinct from the depth-2
/// invariant proper (which lives in [`crate::dispatch::depth`]). It runs
/// only when sub-lead spawning is *enabled* by the manifest; the depth-2
/// caller/capability checks fire on every `spawn_sublead` invocation.
pub fn validate_sublead_defaults_adequate(r: &ResolvedManifest) -> Result<()> {
    let Some(lead) = r.lead.as_ref() else {
        return Ok(());
    };
    if !lead.allow_subleads {
        return Ok(());
    }
    let owned_default_ok = lead
        .sublead_defaults
        .as_ref()
        .is_some_and(|d| d.budget_usd.is_some() && d.max_workers.is_some());
    let shared_pool_default_ok = lead.sublead_defaults.as_ref().is_some_and(|d| d.read_down);
    if owned_default_ok || shared_pool_default_ok {
        return Ok(());
    }

    bail!(
        "`[lead] allow_subleads = true` requires `[sublead_defaults]` to \
         specify either (a) both `budget_usd` and `max_workers` for an \
         Owned-envelope default, or (b) `read_down = true` for a SharedPool \
         default. Without this, any `spawn_sublead` call from the lead that \
         omits those fields will fail at dispatch time with \
         \"budget_usd required when read_down=false\". \
         Pick the one that matches your intent:\n  \
         [sublead_defaults]\n  \
         read_down = true                  # share root's budget + worker pool\n\n\
         or\n\n  \
         [sublead_defaults]\n  \
         budget_usd = 1.00                 # per-sub-lead cap\n  \
         max_workers = 4"
    );
}

fn validate_hierarchical_ranges(r: &ResolvedManifest) -> Result<()> {
    if let Some(mw) = r.max_workers {
        if mw == 0 || mw > 16 {
            bail!("[lead].max_workers must be between 1 and 16 inclusive");
        }
    }
    if let Some(b) = r.budget_usd {
        if b <= 0.0 {
            bail!("[lead].budget_usd must be > 0");
        }
    }
    if let Some(t) = r.lead_timeout_secs {
        if t == 0 {
            bail!("[lead].lead_timeout_secs must be > 0");
        }
    }
    // The sub-lead caps below are gates: a value of 0 makes spawn_sublead
    // fail-closed for every call (e.g. `max_subleads = 0` rejects every
    // spawn because `current + 1 > 0` is unconditionally true). That's a
    // non-functional config that the runtime would only surface on the
    // first spawn attempt — catch it at validate time instead so the
    // operator sees a typed manifest error. These caps live on the inner
    // `[lead]` block, not surfaced to the top level like `max_workers` /
    // `budget_usd` / `lead_timeout_secs`.
    if let Some(lead) = r.lead.as_ref() {
        if let Some(ms) = lead.max_subleads {
            if ms == 0 {
                bail!("[lead].max_subleads must be > 0 (omit the field to leave uncapped)");
            }
        }
        if let Some(mt) = lead.max_total_workers {
            if mt == 0 {
                bail!("[lead].max_total_workers must be > 0 (omit the field to leave uncapped)");
            }
        }
        if let Some(b) = lead.max_sublead_budget_usd {
            // f64 admits negatives (unlike the u32 caps above where serde
            // rejects them at parse). The runtime cap check
            // (`budget_usd > cap`) on a negative cap rejects every positive
            // requested budget, so the manifest is non-functional.
            if b <= 0.0 {
                bail!(
                    "[lead].max_sublead_budget_usd must be > 0 (omit the field to leave uncapped)"
                );
            }
        }
    }
    // No `max_parallel_tasks == 0` check here: that's a flat-mode
    // ([[task]]) concurrency cap. In hierarchical mode the lead's own
    // `[lead].max_workers` governs concurrency, and the resolved
    // ResolvedManifest.max_parallel_tasks always has the resolver
    // default (DEFAULT_MAX_PARALLEL_TASKS) applied, so an `== 0`
    // assertion here would never fire. The legitimate range check
    // lives in validate_ranges (flat-mode path) instead.
    Ok(())
}

fn validate_ids(r: &ResolvedManifest) -> Result<()> {
    let mut seen = HashSet::new();
    for t in &r.tasks {
        if !seen.insert(&t.id) {
            bail!(
                "duplicate task id: '{}'; each [[task]].id must be unique within the manifest",
                t.id
            );
        }
        if t.id.is_empty() {
            bail!("empty task id");
        }
        if !t
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            bail!(
                "task id '{}' contains invalid characters (allowed: a-zA-Z0-9_-)",
                t.id
            );
        }
    }
    Ok(())
}

fn validate_directories(r: &ResolvedManifest) -> Result<()> {
    for t in &r.tasks {
        if !t.directory.is_dir() {
            bail!(
                "task '{}' directory does not exist or is not a directory: {}",
                t.id,
                t.directory.display()
            );
        }
        if t.use_worktree && !is_in_git_repo(&t.directory) {
            bail!(
                "task '{}' has use_worktree=true but directory is not a git work-tree: {}",
                t.id,
                t.directory.display()
            );
        }
    }
    Ok(())
}

fn validate_branch_conflicts(r: &ResolvedManifest) -> Result<()> {
    let mut seen: HashSet<(std::path::PathBuf, String)> = HashSet::new();
    for t in &r.tasks {
        if !t.use_worktree {
            continue;
        }
        if let Some(b) = &t.branch {
            let canon = std::fs::canonicalize(&t.directory).unwrap_or_else(|_| t.directory.clone());
            if !seen.insert((canon, b.clone())) {
                bail!(
                    "two [[task]] entries target the same git directory + branch '{}'; \
                     give each task a unique branch (via `branch = \"...\"`) or point them \
                     at separate directories",
                    b
                );
            }
        }
    }
    Ok(())
}

fn validate_ranges(r: &ResolvedManifest) -> Result<()> {
    // #320: max_parallel_tasks is None in hierarchical mode (the lead's
    // max_workers caps concurrency there). Only validate when present.
    if let Some(0) = r.max_parallel_tasks {
        bail!("[run].max_parallel_tasks must be > 0");
    }
    for t in &r.tasks {
        if t.timeout_secs == 0 {
            bail!("task '{}': timeout_secs must be > 0", t.id);
        }
    }
    Ok(())
}

fn is_in_git_repo(path: &Path) -> bool {
    git2::Repository::discover(path).is_ok()
}

/// Inspect a TOML parse error against the manifest source text and, if it
/// matches a known v0.8→v0.9 migration footprint, return a tailored error
/// with explicit migration guidance. Returns `None` if no legacy pattern is
/// detected (the original parse error stands).
///
/// This is a pure-text scan over the source — it doesn't try to fully parse
/// the file (which already failed). It looks for the table/field markers that
/// changed in v0.9 and emits one error per matched legacy pattern.
pub fn translate_legacy_parse_error(parse_err: &toml::de::Error, src: &str) -> Option<Error> {
    let mut migrations: Vec<String> = Vec::new();

    // [[lead]] array form removed in v0.9.
    if has_top_level_array_table(src, "lead") {
        migrations.push(
            "  • `[[lead]]` (array form) was removed in v0.9. Use `[lead]` \
             (single-table) instead. Pitboss only ever supported one root \
             lead, so the array form was removable without losing capability."
                .to_string(),
        );
    }

    // [run] no longer carries the lead caps.
    let run_field_migrations = [
        ("max_workers", "[lead].max_workers"),
        ("budget_usd", "[lead].budget_usd"),
        ("lead_timeout_secs", "[lead].lead_timeout_secs"),
    ];
    for (old, new) in run_field_migrations {
        if has_field_in_section(src, "run", old) {
            migrations.push(format!(
                "  • `[run].{old}` moved to `{new}` in v0.9 — these are \
                 lead-level caps, not run-wide settings."
            ));
        }
    }

    // [run].max_parallel renamed to [run].max_parallel_tasks.
    if has_field_in_section(src, "run", "max_parallel")
        && !has_field_in_section(src, "run", "max_parallel_tasks")
    {
        migrations.push(
            "  • `[run].max_parallel` renamed to `[run].max_parallel_tasks` \
             in v0.9 (clarifies it's the flat-mode task concurrency cap)."
                .to_string(),
        );
    }

    // [run].approval_policy renamed to [run].default_approval_policy.
    if has_field_in_section(src, "run", "approval_policy")
        && !has_field_in_section(src, "run", "default_approval_policy")
    {
        migrations.push(
            "  • `[run].approval_policy` renamed to \
             `[run].default_approval_policy` in v0.9 (disambiguates from the \
             `[[approval_policy]]` rule array)."
                .to_string(),
        );
    }

    // [lead].max_workers_across_tree renamed to [lead].max_total_workers.
    if has_field_in_section(src, "lead", "max_workers_across_tree") {
        migrations.push(
            "  • `[lead].max_workers_across_tree` renamed to \
             `[lead].max_total_workers` in v0.9 (shorter and clearer)."
                .to_string(),
        );
    }

    // [lead.sublead_defaults] promoted to top-level [sublead_defaults].
    if has_subtable(src, "lead", "sublead_defaults") {
        migrations.push(
            "  • `[lead.sublead_defaults]` promoted to top-level \
             `[sublead_defaults]` in v0.9 (form-friendly; eliminates the \
             prompt-ordering footgun where keys after the subtable header \
             silently moved scope)."
                .to_string(),
        );
    }

    if migrations.is_empty() {
        return None;
    }

    Some(anyhow!(
        "manifest uses pre-v0.9 schema. Migrate the following:\n{}\n\nOriginal parse error: {}",
        migrations.join("\n"),
        parse_err
    ))
}

/// Returns true iff the source contains a `[[<name>]]` array-of-tables marker
/// (with optional whitespace) at the start of a line. Skips comment lines so
/// `# [[lead]] (legacy)` in a docstring doesn't false-positive.
fn has_top_level_array_table(src: &str, name: &str) -> bool {
    let needle = format!("[[{name}]]");
    src.lines().any(|line| {
        let trimmed = line.trim_start();
        !trimmed.starts_with('#') && trimmed.starts_with(&needle)
    })
}

/// Returns true iff the source contains a top-level `[<section>]` table
/// containing a `<field> =` assignment before the next `[` line. Comment
/// lines (`# …`) are skipped entirely so a commented-out legacy field
/// (`# approval_policy = "block"`) doesn't trigger a false migration hint
/// and `# [run]` doesn't mistakenly enter or leave the target section.
fn has_field_in_section(src: &str, section: &str, field: &str) -> bool {
    let header = format!("[{section}]");
    let mut in_section = false;
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('[') {
            // Entering a new table — true if we hit the target header,
            // false on any other table (including subtables of the section).
            in_section =
                trimmed.starts_with(&header) && trimmed.chars().nth(header.len()) != Some('.');
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix(field) {
            if rest.starts_with(' ') || rest.starts_with('=') || rest.starts_with('\t') {
                return true;
            }
        }
    }
    false
}

/// Returns true iff the source contains a `[<parent>.<child>]` subtable
/// header. Skips comment lines (see [`has_top_level_array_table`]).
fn has_subtable(src: &str, parent: &str, child: &str) -> bool {
    let needle = format!("[{parent}.{child}]");
    src.lines().any(|line| {
        let trimmed = line.trim_start();
        !trimmed.starts_with('#') && trimmed.starts_with(&needle)
    })
}

#[cfg(test)]
mod tests {
    use super::super::resolve::ResolvedTask;
    use super::super::schema::{Effort, WorktreeCleanup};
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;
    use tempfile::TempDir;

    fn with_tmp_repo(use_git: bool) -> TempDir {
        let d = TempDir::new().unwrap();
        if use_git {
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(d.path())
                .status()
                .unwrap();
            Command::new("git")
                .args(["config", "user.email", "t@t.x"])
                .current_dir(d.path())
                .status()
                .unwrap();
            Command::new("git")
                .args(["config", "user.name", "t"])
                .current_dir(d.path())
                .status()
                .unwrap();
            std::fs::write(d.path().join("r"), "").unwrap();
            Command::new("git")
                .args(["add", "."])
                .current_dir(d.path())
                .status()
                .unwrap();
            Command::new("git")
                .args(["commit", "-q", "-m", "i"])
                .current_dir(d.path())
                .status()
                .unwrap();
        }
        d
    }

    fn rt(id: &str, dir: PathBuf, use_worktree: bool, branch: Option<&str>) -> ResolvedTask {
        ResolvedTask {
            id: id.into(),
            directory: dir,
            prompt: "p".into(),
            branch: branch.map(str::to_string),
            model: "m".into(),
            effort: Effort::High,
            tools: vec![],
            timeout_secs: 60,
            use_worktree,
            env: Default::default(),
            resume_session_id: None,
        }
    }

    fn rm(tasks: Vec<ResolvedTask>) -> ResolvedManifest {
        ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks,
            lead: None,
            max_workers: None,
            budget_usd: None,
            lead_budget_usd: None,
            lead_timeout_secs: None,
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        }
    }

    fn rm_with<F>(f: F) -> ResolvedManifest
    where
        F: FnOnce(&mut ResolvedManifest),
    {
        let d = with_tmp_repo(true);
        let mut m = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: Some(rl("lead", d.path().to_path_buf())),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_budget_usd: None,
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        f(&mut m);
        m
    }

    #[test]
    fn rejects_duplicate_ids() {
        let d = with_tmp_repo(true);
        let r = rm(vec![
            rt("a", d.path().to_path_buf(), false, None),
            rt("a", d.path().to_path_buf(), false, None),
        ]);
        assert!(validate(&r).unwrap_err().to_string().contains("duplicate"));
    }

    #[test]
    fn rejects_missing_directory() {
        let r = rm(vec![rt("a", PathBuf::from("/no/such/path"), false, None)]);
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_non_git_directory_with_worktree_true() {
        let d = with_tmp_repo(false);
        let r = rm(vec![rt("a", d.path().to_path_buf(), true, Some("b"))]);
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("not a git"));
    }

    #[test]
    fn accepts_non_git_directory_with_worktree_false() {
        let d = with_tmp_repo(false);
        let r = rm(vec![rt("a", d.path().to_path_buf(), false, None)]);
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn rejects_branch_dir_duplicates() {
        let d = with_tmp_repo(true);
        let r = rm(vec![
            rt("a", d.path().to_path_buf(), true, Some("shared")),
            rt("b", d.path().to_path_buf(), true, Some("shared")),
        ]);
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_zero_max_parallel_tasks() {
        let d = with_tmp_repo(true);
        let mut r = rm(vec![rt("a", d.path().to_path_buf(), false, None)]);
        r.max_parallel_tasks = Some(0);
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_invalid_id_chars() {
        let d = with_tmp_repo(true);
        let r = rm(vec![rt("has spaces", d.path().to_path_buf(), false, None)]);
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_mixing_tasks_and_lead() {
        let d = with_tmp_repo(true);
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![rt("t1", d.path().to_path_buf(), false, None)],
            lead: Some(rl("lead", d.path().to_path_buf())),
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_budget_usd: None,
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        let err = validate(&r).unwrap_err().to_string();
        assert!(
            err.contains("cannot combine [[task]] and [lead]"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_max_workers_out_of_range() {
        let d = with_tmp_repo(true);
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: Some(rl("l", d.path().to_path_buf())),
            max_workers: Some(17),
            budget_usd: Some(1.0),
            lead_budget_usd: None,
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_non_positive_budget() {
        let d = with_tmp_repo(true);
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: Some(rl("l", d.path().to_path_buf())),
            max_workers: Some(4),
            budget_usd: Some(0.0),
            lead_budget_usd: None,
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        assert!(validate(&r).is_err());
    }

    /// Regression for ISSUE-manifest-2: the sub-lead caps live on the
    /// inner `[lead]` block (not promoted to ResolvedManifest's top
    /// level), and `validate_hierarchical_ranges` previously left them
    /// unchecked — so a config like `max_subleads = 0` parsed cleanly
    /// but produced a runtime gate that rejected every spawn (because
    /// `current + 1 > 0` is unconditionally true). Pin the now-explicit
    /// validate-time error.
    fn manifest_with_lead_caps(
        d: &TempDir,
        max_subleads: Option<u32>,
        max_total_workers: Option<u32>,
        max_sublead_budget_usd: Option<f64>,
    ) -> ResolvedManifest {
        let mut lead = rl("l", d.path().to_path_buf());
        lead.max_subleads = max_subleads;
        lead.max_total_workers = max_total_workers;
        lead.max_sublead_budget_usd = max_sublead_budget_usd;
        ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_budget_usd: None,
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        }
    }

    #[test]
    fn rejects_zero_max_subleads() {
        let d = with_tmp_repo(true);
        let r = manifest_with_lead_caps(&d, Some(0), None, None);
        let err = validate(&r).unwrap_err().to_string();
        assert!(
            err.contains("[lead].max_subleads must be > 0"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_zero_max_total_workers() {
        let d = with_tmp_repo(true);
        let r = manifest_with_lead_caps(&d, None, Some(0), None);
        let err = validate(&r).unwrap_err().to_string();
        assert!(
            err.contains("[lead].max_total_workers must be > 0"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_zero_max_sublead_budget_usd() {
        let d = with_tmp_repo(true);
        let r = manifest_with_lead_caps(&d, None, None, Some(0.0));
        let err = validate(&r).unwrap_err().to_string();
        assert!(
            err.contains("[lead].max_sublead_budget_usd must be > 0"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_negative_max_sublead_budget_usd() {
        // f64 admits negatives where u32 doesn't — the runtime cap check
        // would silently accept any positive requested budget against a
        // negative cap. Validate-time error covers that gap.
        let d = with_tmp_repo(true);
        let r = manifest_with_lead_caps(&d, None, None, Some(-1.0));
        let err = validate(&r).unwrap_err().to_string();
        assert!(
            err.contains("[lead].max_sublead_budget_usd must be > 0"),
            "got: {err}"
        );
    }

    #[test]
    fn accepts_omitted_sublead_caps() {
        // None on all three is the "uncapped" form and must remain valid.
        let d = with_tmp_repo(true);
        let r = manifest_with_lead_caps(&d, None, None, None);
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn accepts_positive_sublead_caps() {
        let d = with_tmp_repo(true);
        let r = manifest_with_lead_caps(&d, Some(3), Some(16), Some(5.0));
        assert!(validate(&r).is_ok());
    }

    fn hierarchical_with_subleads(
        allow_subleads: bool,
        sublead_defaults: Option<super::super::resolve::ResolvedSubleadDefaults>,
    ) -> (TempDir, ResolvedManifest) {
        let d = with_tmp_repo(true);
        let mut lead = rl("l", d.path().to_path_buf());
        lead.allow_subleads = allow_subleads;
        lead.sublead_defaults = sublead_defaults;
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_budget_usd: None,
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        (d, r)
    }

    #[test]
    fn accepts_allow_subleads_false_without_sublead_defaults() {
        let (_d, r) = hierarchical_with_subleads(false, None);
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn rejects_allow_subleads_true_without_sublead_defaults() {
        let (_d, r) = hierarchical_with_subleads(true, None);
        let err = validate(&r).unwrap_err();
        assert!(
            err.to_string()
                .contains("allow_subleads = true` requires `[sublead_defaults]`"),
            "expected explanatory error, got: {err}"
        );
    }

    #[test]
    fn rejects_allow_subleads_true_with_empty_sublead_defaults() {
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::ResolvedSubleadDefaults {
                budget_usd: None,
                lead_budget_usd: None,
                max_workers: None,
                lead_timeout_secs: None,
                read_down: false,
            }),
        );
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_allow_subleads_true_with_partial_owned_defaults() {
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::ResolvedSubleadDefaults {
                budget_usd: Some(1.0),
                lead_budget_usd: None,
                max_workers: None,
                lead_timeout_secs: None,
                read_down: false,
            }),
        );
        assert!(validate(&r).is_err());
    }

    #[test]
    fn accepts_allow_subleads_true_with_complete_owned_defaults() {
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::ResolvedSubleadDefaults {
                budget_usd: Some(2.0),
                lead_budget_usd: None,
                max_workers: Some(4),
                lead_timeout_secs: Some(1800),
                read_down: false,
            }),
        );
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn accepts_allow_subleads_true_with_read_down() {
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::ResolvedSubleadDefaults {
                budget_usd: None,
                lead_budget_usd: None,
                max_workers: None,
                lead_timeout_secs: None,
                read_down: true,
            }),
        );
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn accepts_allow_subleads_true_with_read_down_and_explicit_defaults() {
        let (_d, r) = hierarchical_with_subleads(
            true,
            Some(super::super::resolve::ResolvedSubleadDefaults {
                budget_usd: Some(2.0),
                lead_budget_usd: None,
                max_workers: Some(4),
                lead_timeout_secs: None,
                read_down: true,
            }),
        );
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn rejects_manifest_with_no_tasks_and_no_lead() {
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: None,
            max_workers: None,
            budget_usd: None,
            lead_budget_usd: None,
            lead_timeout_secs: None,
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("empty manifest"), "got: {err}");
    }

    fn rl(id: &str, dir: PathBuf) -> super::super::resolve::ResolvedLead {
        super::super::resolve::ResolvedLead {
            id: id.into(),
            directory: dir,
            prompt: "p".into(),
            branch: None,
            model: "m".into(),
            effort: super::super::schema::Effort::High,
            tools: vec![],
            timeout_secs: 600,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
            permission_routing: Default::default(),
            allow_subleads: false,
            max_subleads: None,
            max_sublead_budget_usd: None,
            max_total_workers: None,
            sublead_defaults: None,
        }
    }

    #[test]
    fn rejects_lead_with_empty_prompt() {
        let d = with_tmp_repo(true);
        let mut lead = rl("l", d.path().to_path_buf());
        lead.prompt = String::new();
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_budget_usd: None,
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        let err = validate(&r).unwrap_err().to_string();
        assert!(
            err.contains("prompt is required but is empty"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_lead_with_whitespace_only_prompt() {
        let d = with_tmp_repo(true);
        let mut lead = rl("l", d.path().to_path_buf());
        lead.prompt = "   \t\n  ".to_string();
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_budget_usd: None,
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_webhook_without_url() {
        let m = rm_with(|m: &mut ResolvedManifest| {
            m.notifications
                .push(crate::notify::config::NotificationConfig {
                    kind: crate::notify::config::SinkKind::Webhook,
                    url: None,
                    events: None,
                    severity_min: crate::notify::Severity::Info,
                    request_timeout_secs: None,
                });
        });
        let err = validate(&m).unwrap_err();
        assert!(err.to_string().contains("requires a non-empty 'url'"));
    }

    #[test]
    fn rejects_unknown_event() {
        let m = rm_with(|m: &mut ResolvedManifest| {
            m.notifications
                .push(crate::notify::config::NotificationConfig {
                    kind: crate::notify::config::SinkKind::Log,
                    url: None,
                    events: Some(vec!["hallucinate".into()]),
                    severity_min: crate::notify::Severity::Info,
                    request_timeout_secs: None,
                });
        });
        let err = validate(&m).unwrap_err();
        assert!(err.to_string().contains("unknown event"));
    }

    // ── Migration translator tests ───────────────────────────────────────

    #[test]
    fn translator_flags_legacy_array_lead() {
        let src = "[[lead]]\nid = \"x\"\ndirectory = \"/tmp\"\nprompt = \"p\"\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        let err = translate_legacy_parse_error(&parse_err.unwrap_err(), src).unwrap();
        let s = err.to_string();
        assert!(s.contains("[[lead]]"), "got: {s}");
        assert!(s.contains("v0.9"), "got: {s}");
    }

    #[test]
    fn translator_flags_run_max_workers() {
        let src = "[run]\nmax_workers = 4\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        let err = translate_legacy_parse_error(&parse_err.unwrap_err(), src).unwrap();
        assert!(err.to_string().contains("[run].max_workers"));
        assert!(err.to_string().contains("[lead].max_workers"));
    }

    #[test]
    fn translator_flags_max_parallel_rename() {
        let src = "[run]\nmax_parallel = 4\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        let err = translate_legacy_parse_error(&parse_err.unwrap_err(), src).unwrap();
        assert!(err.to_string().contains("max_parallel_tasks"));
    }

    #[test]
    fn translator_flags_nested_sublead_defaults() {
        let src = "[lead.sublead_defaults]\nread_down = true\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        let err = translate_legacy_parse_error(&parse_err.unwrap_err(), src).unwrap();
        assert!(err.to_string().contains("[sublead_defaults]"));
    }

    #[test]
    fn translator_returns_none_for_unrelated_errors() {
        // A v0.9-shaped manifest with an unrelated error (unknown top-level
        // key) should not be hijacked by the migration translator.
        let src = "wibble = \"x\"\n[lead]\nid = \"a\"\ndirectory = \"/tmp\"\nprompt = \"p\"\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        let translated = translate_legacy_parse_error(&parse_err.unwrap_err(), src);
        assert!(translated.is_none());
    }

    fn log_notification() -> crate::notify::config::NotificationConfig {
        crate::notify::config::NotificationConfig {
            kind: crate::notify::config::SinkKind::Log,
            url: None,
            events: None,
            severity_min: crate::notify::Severity::Info,
            request_timeout_secs: None,
        }
    }

    #[test]
    fn lifecycle_survive_parent_with_top_level_notification_is_ok() {
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.notifications.push(log_notification());
        m.lifecycle = Some(crate::manifest::schema::Lifecycle {
            survive_parent: true,
            notify: None,
        });
        validate(&m).expect("top-level notification satisfies the coupling");
    }

    #[test]
    fn lifecycle_survive_parent_with_inline_notify_is_ok() {
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.lifecycle = Some(crate::manifest::schema::Lifecycle {
            survive_parent: true,
            notify: Some(log_notification()),
        });
        validate(&m).expect("inline lifecycle.notify satisfies the coupling");
    }

    #[test]
    fn lifecycle_survive_parent_without_any_notification_is_rejected() {
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.lifecycle = Some(crate::manifest::schema::Lifecycle {
            survive_parent: true,
            notify: None,
        });
        let err = validate(&m).expect_err("naked survive_parent must fail");
        let msg = format!("{err:?}");
        assert!(msg.contains("survive_parent = true"), "msg: {msg}");
        assert!(msg.contains("notification target"), "msg: {msg}");
        assert!(msg.contains("PITBOSS_PARENT_NOTIFY_URL"), "msg: {msg}");
    }

    #[test]
    fn lifecycle_survive_parent_false_does_not_require_notify() {
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.lifecycle = Some(crate::manifest::schema::Lifecycle {
            survive_parent: false,
            notify: None,
        });
        validate(&m).expect("survive_parent=false has no coupling requirement");
    }

    #[test]
    fn lifecycle_inline_notify_runs_through_ssrf_guard() {
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.lifecycle = Some(crate::manifest::schema::Lifecycle {
            survive_parent: true,
            notify: Some(crate::notify::config::NotificationConfig {
                kind: crate::notify::config::SinkKind::Webhook,
                url: Some("http://localhost/hook".to_string()),
                events: None,
                severity_min: crate::notify::Severity::Info,
                request_timeout_secs: None,
            }),
        });
        // The inline notify gets validated by the same logic as
        // [[notification]] — http:// + loopback should be refused. Operators
        // who need loopback should use PITBOSS_PARENT_NOTIFY_URL.
        let err = validate(&m).expect_err("inline notify must obey SSRF rules");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("https://") || msg.contains("loopback"),
            "msg: {msg}"
        );
    }

    #[test]
    fn lifecycle_omitted_section_is_unchanged_behavior() {
        let d = with_tmp_repo(true);
        let m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        validate(&m).expect("manifests without [lifecycle] continue to validate");
    }

    #[test]
    fn skip_dir_check_skips_git_repo_probe_for_lead() {
        // Regression for #142: container-dispatch hierarchical manifests
        // carry container-side directories (`/workspace/foo`) that don't
        // exist on the host, so the lead's git-repo probe must be gated
        // on `skip_dir_check` just like the directory-existence check.
        // Pre-fix this validate path would fail with
        // "directory is not a git work-tree" for every container manifest.
        let mut lead = rl("lead", PathBuf::from("/workspace/does-not-exist-on-host"));
        lead.use_worktree = true;
        let mut m = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_budget_usd: None,
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        // Sanity: with skip_dir_check=false the validator rejects (the
        // host-side path is bogus).
        assert!(validate(&m).is_err());
        // The container-dispatch entry point must accept this manifest:
        // the probe is gated on skip_dir_check so a host-side absence
        // doesn't leak into validation.
        validate_skip_dir_check(&m)
            .expect("skip_dir_check must skip the git-repo probe for container-dispatch manifests");
        // Same when use_worktree is false — the gate covers both paths.
        m.lead.as_mut().unwrap().use_worktree = false;
        validate_skip_dir_check(&m).expect("skip_dir_check skips dir checks unconditionally");
    }

    #[test]
    fn translator_ignores_legacy_field_in_comment_lines() {
        // #155: a commented-out v0.8 field name in [run] must not trigger
        // a false-positive migration hint. Pre-fix the line scanner did
        // a prefix match without skipping `#` lines.
        let src = "# previously: max_workers = 4\n[run]\n# approval_policy = \"block\"\nmax_parallel_tasks = 2\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        // The [[run]] body is otherwise valid v0.9, so toml may or may
        // not error — only run the scanner if it did.
        if let Err(e) = parse_err {
            let translated = translate_legacy_parse_error(&e, src);
            assert!(
                translated.is_none(),
                "comment-only mentions of legacy fields must not be translated as migration hints: {translated:?}"
            );
        }
    }

    #[test]
    fn translator_scanner_skips_commented_section_brackets() {
        // `# [[lead]]` in a leading comment used to satisfy
        // has_top_level_array_table; now it must not.
        let src =
            "# legacy syntax: [[lead]]\n[lead]\nid = \"x\"\ndirectory = \"/tmp\"\nprompt = \"p\"\n";
        let parse_err: Result<super::super::schema::Manifest, _> = toml::from_str(src);
        if let Err(e) = parse_err {
            let translated = translate_legacy_parse_error(&e, src);
            assert!(
                translated.is_none(),
                "commented-out [[lead]] must not be flagged as legacy: {translated:?}"
            );
        }
    }

    #[test]
    fn approval_rule_rejects_unknown_fields() {
        // #155: ApprovalRuleSpec lacked deny_unknown_fields, so a typo'd
        // sub-field would silently pass through.
        let src = r#"actoin = "auto_approve""#;
        let err: Result<super::super::schema::ApprovalRuleSpec, _> = toml::from_str(src);
        assert!(err.is_err(), "unknown field 'actoin' must be rejected");
    }

    #[test]
    fn approval_match_rejects_unknown_fields() {
        // The classic typo from the audit: `catagory` instead of `category`.
        // Without deny_unknown_fields, the rule parses with no match filter
        // (every field None) and silently matches every event.
        let src = r#"catagory = "tool_use""#;
        let err: Result<super::super::schema::ApprovalMatchSpec, _> = toml::from_str(src);
        assert!(err.is_err(), "unknown field 'catagory' must be rejected");
    }

    #[test]
    fn extra_apt_invalid_package_name_fails_validate() {
        // pitboss validate must reject the same names that build_run_args
        // rejects — operators expect schema-level validation to be
        // authoritative, not deferred to dispatch.
        use super::super::schema::ContainerConfig;
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.container = Some(ContainerConfig {
            extra_apt: vec!["mdbook;rm -rf /".into()],
            ..ContainerConfig::default()
        });
        let err = validate(&m).expect_err("invalid extra_apt name must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("extra_apt"),
            "error must reference extra_apt: {msg}"
        );
    }

    #[test]
    fn extra_apt_valid_package_names_pass_validate() {
        use super::super::schema::ContainerConfig;
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.container = Some(ContainerConfig {
            extra_apt: vec!["mdbook".into(), "g++-12".into(), "python3.11".into()],
            ..ContainerConfig::default()
        });
        validate(&m).expect("realistic apt package names must validate");
    }

    #[test]
    fn extra_args_dangerous_flag_fails_validate() {
        // F-SEC-9: dangerous container runtime flags must be rejected at
        // `pitboss validate` time — not deferred to dispatch — so
        // operators see the gate in their CI / pre-flight checks.
        use super::super::schema::ContainerConfig;
        for bad in [
            "--privileged",
            "--pid=host",
            "--cap-add=SYS_ADMIN",
            "-v",
            "--user",
            "--security-opt=seccomp=unconfined",
        ] {
            let d = with_tmp_repo(true);
            let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
            m.container = Some(ContainerConfig {
                extra_args: vec![bad.into()],
                ..ContainerConfig::default()
            });
            let err = validate(&m).expect_err(&format!("{bad} must fail validate"));
            assert!(
                err.to_string().contains("extra_args"),
                "error must reference extra_args: {err}"
            );
        }
    }

    #[test]
    fn mount_relative_container_path_fails_validate() {
        // F-SEC-10: [[container.mount]].container must be absolute, same
        // rule as [[container.copy]].container.
        use super::super::schema::{ContainerConfig, MountSpec};
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.container = Some(ContainerConfig {
            mounts: vec![MountSpec {
                host: PathBuf::from("/abs/host"),
                container: PathBuf::from("relative/path"),
                readonly: false,
            }],
            ..ContainerConfig::default()
        });
        let err = validate(&m).expect_err("relative container path must fail");
        assert!(
            err.to_string()
                .contains("container.mount]].container must be an absolute path"),
            "{err}"
        );
    }

    #[test]
    fn mount_relative_host_path_fails_validate() {
        use super::super::schema::{ContainerConfig, MountSpec};
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.container = Some(ContainerConfig {
            mounts: vec![MountSpec {
                host: PathBuf::from("./relative"),
                container: PathBuf::from("/abs/container"),
                readonly: false,
            }],
            ..ContainerConfig::default()
        });
        let err = validate(&m).expect_err("relative host path must fail");
        assert!(
            err.to_string()
                .contains("container.mount]].host must be an absolute or tilde-prefixed path"),
            "{err}"
        );
    }

    #[test]
    fn mount_dangerous_host_paths_fail_validate() {
        // F-SEC-10: catastrophic host paths are hard-rejected.
        use super::super::schema::{ContainerConfig, MountSpec};
        for bad in [
            "/var/run/docker.sock",
            "/run/docker.sock",
            "/var/run/podman/podman.sock",
            "/run/podman/podman.sock",
            "/proc",
            "/sys",
            "/",
            "/etc/shadow",
            // Trailing-slash form is canonicalised in the check; must
            // still fail.
            "/proc/",
        ] {
            let d = with_tmp_repo(true);
            let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
            m.container = Some(ContainerConfig {
                mounts: vec![MountSpec {
                    host: PathBuf::from(bad),
                    container: PathBuf::from("/mnt/host"),
                    readonly: false,
                }],
                ..ContainerConfig::default()
            });
            let err = validate(&m).expect_err(&format!("{bad} must fail validate"));
            assert!(
                err.to_string().contains("forbidden"),
                "rejection must label as forbidden: {err}"
            );
        }
    }

    #[test]
    fn mount_sensitive_but_legitimate_host_paths_pass_validate() {
        // F-SEC-10: paths that are sensitive-but-have-legitimate-uses
        // (/etc, /var/log, peer home dirs) are NOT in the denylist —
        // they pass validation. The line is "would this give the
        // worker host-root or host-namespace authority?". Sensitive ≠
        // catastrophic.
        use super::super::schema::{ContainerConfig, MountSpec};
        for ok in ["/etc", "/var/log", "/home/alice", "/tmp"] {
            let d = with_tmp_repo(true);
            let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
            m.container = Some(ContainerConfig {
                mounts: vec![MountSpec {
                    host: PathBuf::from(ok),
                    container: PathBuf::from("/mnt/host"),
                    readonly: true,
                }],
                ..ContainerConfig::default()
            });
            validate(&m).unwrap_or_else(|e| {
                panic!("{ok} should pass validation: {e}");
            });
        }
    }

    #[test]
    fn extra_args_safe_flags_pass_validate() {
        // Common legitimate flags continue to validate. This pins the
        // boundary: if a future change accidentally over-blocks (e.g.
        // rejects every --cap-add), this test catches it.
        use super::super::schema::ContainerConfig;
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.container = Some(ContainerConfig {
            extra_args: vec![
                "--network=host".into(),
                "--cap-drop=ALL".into(),
                "--cap-add=NET_ADMIN".into(),
                "--memory=4g".into(),
            ],
            ..ContainerConfig::default()
        });
        validate(&m).expect("realistic safe extra_args must validate");
    }

    #[test]
    fn copy_relative_container_path_fails_validate() {
        use super::super::schema::{ContainerConfig, CopySpec};
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.container = Some(ContainerConfig {
            copy: vec![CopySpec {
                host: PathBuf::from("/abs/script.py"),
                container: PathBuf::from("relative/path"),
            }],
            ..ContainerConfig::default()
        });
        let err = validate(&m).expect_err("relative container path must fail");
        assert!(
            err.to_string()
                .contains("container must be an absolute path"),
            "{err}"
        );
    }

    #[test]
    fn copy_relative_host_path_fails_validate() {
        use super::super::schema::{ContainerConfig, CopySpec};
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.container = Some(ContainerConfig {
            copy: vec![CopySpec {
                host: PathBuf::from("./script.py"),
                container: PathBuf::from("/opt/script.py"),
            }],
            ..ContainerConfig::default()
        });
        let err = validate(&m).expect_err("relative host path must fail");
        assert!(
            err.to_string()
                .contains("host must be an absolute or tilde-prefixed path"),
            "{err}"
        );
    }

    #[test]
    fn copy_tilde_host_and_absolute_container_pass_validate() {
        use super::super::schema::{ContainerConfig, CopySpec};
        let d = with_tmp_repo(true);
        let mut m = rm(vec![rt("t", d.path().to_path_buf(), false, None)]);
        m.container = Some(ContainerConfig {
            copy: vec![CopySpec {
                host: PathBuf::from("~/scripts/install.sh"),
                container: PathBuf::from("/opt/install.sh"),
            }],
            ..ContainerConfig::default()
        });
        validate(&m).expect("tilde host + absolute container must validate");
    }

    /// #370 (item 2): pin the post-#368 contract that
    /// `permission_routing = "path_b"` is selectable. Pre-#368 this
    /// returned `Err`; the change to a stderr soak warning is
    /// behavioral and deserves a regression test so an accidental
    /// revert to `bail!` would show up in CI rather than at runtime.
    /// Path A (the default) gets a sibling assertion to pin baseline.
    #[test]
    fn path_b_permission_routing_passes_validate() {
        use super::super::schema::PermissionRouting;
        let d = with_tmp_repo(true);
        let mut lead = rl("l", d.path().to_path_buf());
        lead.permission_routing = PermissionRouting::PathB;
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_budget_usd: None,
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        validate(&r).expect("path_b must validate (in soak); pre-#368 this bailed");
    }

    #[test]
    fn path_a_permission_routing_passes_validate() {
        use super::super::schema::PermissionRouting;
        let d = with_tmp_repo(true);
        let mut lead = rl("l", d.path().to_path_buf());
        lead.permission_routing = PermissionRouting::PathA;
        let r = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_budget_usd: None,
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        validate(&r).expect("path_a is the default and must validate");
    }

    #[test]
    fn rejects_invalid_worker_type_id_chars() {
        use crate::manifest::schema::WorkerType;
        let r = rm_with(|m| {
            m.worker_types = vec![WorkerType {
                id: "bad id!".into(),
                tools: vec!["Read".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                agent_profile: None,
            }];
        });
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("worker_type"), "{err}");
        assert!(err.contains("^[A-Za-z0-9_-]+$"), "{err}");
    }

    #[test]
    fn rejects_duplicate_worker_type_ids() {
        use crate::manifest::schema::WorkerType;
        let r = rm_with(|m| {
            m.worker_types = vec![
                WorkerType {
                    id: "extraction".into(),
                    tools: vec!["Read".into()],
                    allowed_models: vec![],
                    max_timeout_secs: None,
                    agent_profile: None,
                },
                WorkerType {
                    id: "extraction".into(),
                    tools: vec!["Write".into()],
                    allowed_models: vec![],
                    max_timeout_secs: None,
                    agent_profile: None,
                },
            ];
        });
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "{err}");
        assert!(err.contains("extraction"), "{err}");
    }

    #[test]
    fn rejects_duplicate_sublead_type_ids() {
        use crate::manifest::schema::SubleadType;
        let r = rm_with(|m| {
            m.sublead_types = vec![
                SubleadType {
                    id: "planner".into(),
                    tools: vec![],
                    allowed_models: vec![],
                    max_timeout_secs: None,
                    max_budget_usd: None,
                    agent_profile: None,
                },
                SubleadType {
                    id: "planner".into(),
                    tools: vec![],
                    allowed_models: vec![],
                    max_timeout_secs: None,
                    max_budget_usd: None,
                    agent_profile: None,
                },
            ];
        });
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "{err}");
        assert!(err.contains("planner"), "{err}");
    }

    #[test]
    fn rejects_require_actor_type_with_no_profiles() {
        let r = rm_with(|m| {
            m.require_actor_type = true;
        });
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("require_actor_type"), "{err}");
        assert!(err.contains("no `[[worker_type]]`"), "{err}");
    }

    #[test]
    fn accepts_require_actor_type_with_at_least_one_profile() {
        use crate::manifest::schema::WorkerType;
        // Bind the tempdir at test scope so the lead's directory survives
        // the validate(&r) call — rm_with drops its own tempdir on return,
        // which would trip validate_lead's directory check.
        let d = with_tmp_repo(true);
        let mut m = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: Some(rl("lead", d.path().to_path_buf())),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_budget_usd: None,
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        m.require_actor_type = true;
        m.worker_types = vec![WorkerType {
            id: "extraction".into(),
            tools: vec!["Read".into()],
            allowed_models: vec![],
            max_timeout_secs: None,
            agent_profile: None,
        }];
        validate(&m).expect("require_actor_type with profile must validate");
    }

    #[test]
    fn accepts_typed_profiles_without_require_flag() {
        use crate::manifest::schema::{SubleadType, WorkerType};
        let d = with_tmp_repo(true);
        let mut m = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: Some(rl("lead", d.path().to_path_buf())),
            max_workers: Some(4),
            budget_usd: Some(1.0),
            lead_budget_usd: None,
            lead_timeout_secs: Some(600),
            default_approval_policy: None,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: Default::default(),
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        m.worker_types = vec![WorkerType {
            id: "extraction".into(),
            tools: vec!["Read".into(), "Glob".into()],
            allowed_models: vec!["claude-haiku-4-5".into()],
            max_timeout_secs: Some(900),
            agent_profile: None,
        }];
        m.sublead_types = vec![SubleadType {
            id: "planner".into(),
            tools: vec!["Read".into()],
            allowed_models: vec!["claude-opus-4-7".into()],
            max_timeout_secs: Some(1800),
            max_budget_usd: Some(2.0),
            agent_profile: None,
        }];
        validate(&m).expect("declared profiles without require flag must validate");
    }

    fn mcp(id: &str, scope: Option<&str>) -> crate::manifest::schema::McpServerSpec {
        crate::manifest::schema::McpServerSpec {
            id: id.into(),
            command: "/bin/true".into(),
            args: vec![],
            env: Default::default(),
            scope: scope.map(str::to_string),
            tools: None,
        }
    }

    #[test]
    fn rejects_mcp_server_scope_with_unknown_prefix() {
        let r = rm_with(|m| {
            m.mcp_servers = vec![mcp("ctx", Some("role:lead"))];
        });
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("type:<id>"), "{err}");
        assert!(err.contains("ctx"), "{err}");
    }

    #[test]
    fn rejects_mcp_server_scope_with_empty_id() {
        let r = rm_with(|m| {
            m.mcp_servers = vec![mcp("ctx", Some("type:"))];
        });
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("empty id"), "{err}");
    }

    #[test]
    fn rejects_mcp_server_scope_with_unknown_id() {
        use crate::manifest::schema::WorkerType;
        let r = rm_with(|m| {
            m.worker_types = vec![WorkerType {
                id: "writer".into(),
                tools: vec!["Read".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                agent_profile: None,
            }];
            m.mcp_servers = vec![mcp("ctx", Some("type:rdr"))];
        });
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("rdr"), "{err}");
        assert!(err.contains("does not match"), "{err}");
    }

    #[test]
    fn accepts_mcp_server_scope_referencing_worker_type() {
        use crate::manifest::schema::WorkerType;
        let r = rm_with(|m| {
            m.worker_types = vec![WorkerType {
                id: "writer".into(),
                tools: vec!["Read".into(), "Write".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                agent_profile: None,
            }];
            m.mcp_servers = vec![mcp("ctx", Some("type:writer"))];
        });
        validate_skip_dir_check(&r)
            .expect("scope referencing a declared worker_type must validate");
    }

    #[test]
    fn accepts_mcp_server_scope_referencing_sublead_type() {
        use crate::manifest::schema::SubleadType;
        let r = rm_with(|m| {
            m.sublead_types = vec![SubleadType {
                id: "planner".into(),
                tools: vec!["Read".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                max_budget_usd: None,
                agent_profile: None,
            }];
            m.mcp_servers = vec![mcp("ctx", Some("type:planner"))];
        });
        validate_skip_dir_check(&r)
            .expect("scope referencing a declared sublead_type must validate");
    }

    #[test]
    fn accepts_unscoped_mcp_server_with_no_profiles() {
        let r = rm_with(|m| {
            m.mcp_servers = vec![mcp("ctx", None)];
        });
        validate_skip_dir_check(&r)
            .expect("unscoped server is the legacy default and must validate");
    }

    // ---- per-server [[mcp_server]].tools allowlist (#391 slice 4) -------

    fn mcp_with_tools(id: &str, tools: Vec<&str>) -> crate::manifest::schema::McpServerSpec {
        crate::manifest::schema::McpServerSpec {
            id: id.into(),
            command: "/bin/true".into(),
            args: vec![],
            env: Default::default(),
            scope: None,
            tools: Some(tools.into_iter().map(String::from).collect()),
        }
    }

    /// `tools = []` is self-defeating: the operator declared the server
    /// then forbade every tool. Force the explicit form (remove the
    /// block) so the manifest's intent is unambiguous.
    #[test]
    fn rejects_mcp_server_tools_empty_list() {
        let r = rm_with(|m| {
            m.mcp_servers = vec![mcp_with_tools("ctx", vec![])];
        });
        let err = validate_skip_dir_check(&r).unwrap_err().to_string();
        assert!(err.contains("self-defeating"), "{err}");
        assert!(err.contains("ctx"), "{err}");
    }

    /// Empty / whitespace tool names are almost always typos and would
    /// silently match nothing at runtime. Reject loudly.
    #[test]
    fn rejects_mcp_server_tools_blank_entry() {
        let r = rm_with(|m| {
            m.mcp_servers = vec![mcp_with_tools("ctx", vec!["read_file", "  "])];
        });
        let err = validate_skip_dir_check(&r).unwrap_err().to_string();
        assert!(err.contains("empty / whitespace"), "{err}");
    }

    /// Duplicates in the allowlist mask intent: did the operator mean
    /// to list the tool twice or is one a typo? Reject so they decide.
    #[test]
    fn rejects_mcp_server_tools_duplicate() {
        let r = rm_with(|m| {
            m.mcp_servers = vec![mcp_with_tools(
                "ctx",
                vec!["read_file", "write_file", "read_file"],
            )];
        });
        let err = validate_skip_dir_check(&r).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "{err}");
        assert!(err.contains("read_file"), "{err}");
    }

    /// A non-empty, distinct allowlist with no actor surfaces yet
    /// referencing it must validate cleanly. Pin so the shape checks
    /// don't accidentally over-reject the happy path.
    #[test]
    fn accepts_mcp_server_tools_well_formed() {
        let r = rm_with(|m| {
            m.mcp_servers = vec![mcp_with_tools("ctx", vec!["read_file", "write_file"])];
        });
        validate_skip_dir_check(&r).expect("declared, non-empty, distinct allowlist must validate");
    }

    /// Consistency check: when an actor surface declares
    /// `mcp__<server>__<tool>` and the server has an allowlist, `<tool>`
    /// must appear in it. This is the manifest-time enforcement Path 2
    /// promises (#397). Tested via worker_type since it's the most
    /// common case.
    #[test]
    fn rejects_worker_type_tool_not_in_mcp_server_allowlist() {
        use crate::manifest::schema::WorkerType;
        let r = rm_with(|m| {
            m.mcp_servers = vec![mcp_with_tools("fs", vec!["read_file"])];
            m.worker_types = vec![WorkerType {
                id: "writer".into(),
                tools: vec!["mcp__fs__write_file".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                agent_profile: None,
            }];
        });
        let err = validate_skip_dir_check(&r).unwrap_err().to_string();
        assert!(err.contains("write_file"), "{err}");
        assert!(err.contains("worker_type"), "{err}");
        assert!(err.contains("\"fs\""), "{err}");
    }

    /// Same gate, but matching: the worker_type's tool IS in the
    /// allowlist. Validate must accept.
    #[test]
    fn accepts_worker_type_tool_in_mcp_server_allowlist() {
        use crate::manifest::schema::WorkerType;
        let r = rm_with(|m| {
            m.mcp_servers = vec![mcp_with_tools("fs", vec!["read_file", "write_file"])];
            m.worker_types = vec![WorkerType {
                id: "writer".into(),
                tools: vec!["mcp__fs__write_file".into(), "Read".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                agent_profile: None,
            }];
        });
        validate_skip_dir_check(&r)
            .expect("worker_type tool listed in mcp_server allowlist must validate");
    }

    /// Server ids with underscores parse correctly:
    /// `mcp__fs_writer__read` must resolve to server `fs_writer` +
    /// tool `read`, not server `fs` + tool `writer__read`.
    /// Longest-prefix-match in the validator guards this.
    #[test]
    fn consistency_check_resolves_server_ids_with_underscores() {
        use crate::manifest::schema::WorkerType;
        let r = rm_with(|m| {
            m.mcp_servers = vec![
                mcp_with_tools("fs", vec!["never_called"]),
                mcp_with_tools("fs_writer", vec!["read_file"]),
            ];
            m.worker_types = vec![WorkerType {
                id: "writer".into(),
                tools: vec!["mcp__fs_writer__read_file".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                agent_profile: None,
            }];
        });
        validate_skip_dir_check(&r).expect(
            "longest-prefix-match must resolve `mcp__fs_writer__read_file` to server `fs_writer`, \
             not `fs`",
        );
    }

    /// Tool entries referencing servers without an allowlist must NOT
    /// trip the consistency check — the gate only constrains servers
    /// that opted into the allowlist surface.
    #[test]
    fn consistency_check_skips_servers_without_allowlist() {
        use crate::manifest::schema::WorkerType;
        let r = rm_with(|m| {
            m.mcp_servers = vec![mcp("fs", None)];
            m.worker_types = vec![WorkerType {
                id: "writer".into(),
                tools: vec!["mcp__fs__anything".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                agent_profile: None,
            }];
        });
        validate_skip_dir_check(&r)
            .expect("servers without an allowlist must not constrain actor tool entries");
    }

    /// `[lead].tools` is also subject to the consistency check.
    /// Operators using a typed lead with declared MCP servers expect
    /// the same validate-time guarantee as workers/subleads.
    #[test]
    fn rejects_lead_tool_not_in_mcp_server_allowlist() {
        let r = rm_with(|m| {
            m.mcp_servers = vec![mcp_with_tools("fs", vec!["read_file"])];
            if let Some(lead) = m.lead.as_mut() {
                lead.tools = vec!["mcp__fs__write_file".into()];
            }
        });
        let err = validate_skip_dir_check(&r).unwrap_err().to_string();
        assert!(err.contains("write_file"), "{err}");
        assert!(err.contains("[lead]"), "{err}");
    }

    /// Path B + zero profiles ⇒ migration warning fires. Pure check on
    /// the helper since the runtime caller logs via `tracing::warn!`,
    /// which is awkward to capture without installing a global
    /// subscriber. Operators see the message via `pitboss validate`'s
    /// stderr.
    #[test]
    fn path_b_without_profiles_emits_migration_warning() {
        let r = rm_with(|_| {
            // Default rl() lead has worker_types/sublead_types empty
            // and permission_routing defaults to PathA — bump it.
        });
        let mut r = r;
        r.lead.as_mut().unwrap().permission_routing =
            crate::manifest::schema::PermissionRouting::PathB;

        let msg = path_b_profile_migration_warning(&r)
            .expect("Path B + no profiles must produce a migration warning");
        assert!(
            msg.contains("`[[worker_type]]`"),
            "warning must reference the missing profile section: {msg}"
        );
        assert!(
            msg.contains("pitboss schema --format=migration"),
            "warning must point operators at the scaffold command: {msg}"
        );
    }

    /// Declaring at least one `[[worker_type]]` suppresses the
    /// migration warning even if `[[sublead_type]]` is still empty
    /// (and vice versa). A partially-migrated manifest mustn't keep
    /// firing a stale advisory — once the operator has started
    /// declaring profiles, they know the system exists.
    #[test]
    fn migration_warning_silenced_by_any_declared_profile() {
        // worker_type-only.
        let mut r = rm_with(|m| {
            m.worker_types = vec![crate::manifest::schema::WorkerType {
                id: "extraction".into(),
                tools: vec!["Read".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                agent_profile: None,
            }];
        });
        r.lead.as_mut().unwrap().permission_routing =
            crate::manifest::schema::PermissionRouting::PathB;
        assert!(
            path_b_profile_migration_warning(&r).is_none(),
            "worker_type alone must suppress the warning"
        );

        // sublead_type-only.
        let mut r = rm_with(|m| {
            m.sublead_types = vec![crate::manifest::schema::SubleadType {
                id: "planner".into(),
                tools: vec!["Read".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                max_budget_usd: None,
                agent_profile: None,
            }];
        });
        r.lead.as_mut().unwrap().permission_routing =
            crate::manifest::schema::PermissionRouting::PathB;
        assert!(
            path_b_profile_migration_warning(&r).is_none(),
            "sublead_type alone must suppress the warning"
        );
    }

    /// `untyped_actor_policy = "block"` with no profiles is
    /// self-defeating: every spawn would auto-deny via the synthetic
    /// empty profile. Validate must reject the manifest before
    /// dispatch so a confused operator doesn't run a session that
    /// produces only `denied_by_profile` events.
    #[test]
    fn validate_rejects_block_untyped_when_no_profiles() {
        let mut r = rm_with(|_| {});
        r.untyped_actor_policy = crate::manifest::schema::UntypedActorPolicy::Block;

        let err = validate_skip_dir_check(&r)
            .expect_err("block policy without any profiles must fail validate");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("untyped_actor_policy = \"block\""),
            "error must name the offending field: {msg}"
        );
        assert!(
            msg.contains("[[worker_type]]") || msg.contains("[[sublead_type]]"),
            "error must point operators at the missing profile section: {msg}"
        );
    }

    /// One profile is enough — block + any declared profile is a
    /// coherent manifest. Mirrors the migration-warning test's
    /// "either profile suffices" property; this one is stricter
    /// because it checks both arms of the OR.
    #[test]
    fn validate_accepts_block_untyped_with_at_least_one_profile() {
        // worker_type only.
        let mut r = rm_with(|m| {
            m.worker_types = vec![crate::manifest::schema::WorkerType {
                id: "extraction".into(),
                tools: vec!["Read".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                agent_profile: None,
            }];
        });
        r.untyped_actor_policy = crate::manifest::schema::UntypedActorPolicy::Block;
        validate_skip_dir_check(&r)
            .expect("worker_type alone must satisfy block-policy validation");

        // sublead_type only.
        let mut r = rm_with(|m| {
            m.sublead_types = vec![crate::manifest::schema::SubleadType {
                id: "planner".into(),
                tools: vec!["Read".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                max_budget_usd: None,
                agent_profile: None,
            }];
        });
        r.untyped_actor_policy = crate::manifest::schema::UntypedActorPolicy::Block;
        validate_skip_dir_check(&r)
            .expect("sublead_type alone must satisfy block-policy validation");
    }

    /// `bridge` (default) without profiles is fine — that's pre-#252
    /// behavior and the migration warning is the only signal.
    #[test]
    fn validate_accepts_bridge_policy_without_profiles() {
        let r = rm_with(|_| {});
        // Default `untyped_actor_policy = Bridge`; no profiles declared.
        validate_skip_dir_check(&r).expect("bridge policy without profiles must pass validate");
    }

    // ── #7: validate_agent_profiles direct coverage ─────────────────────

    fn ap(id: &str, body: &str) -> crate::manifest::schema::AgentProfile {
        crate::manifest::schema::AgentProfile {
            id: id.to_string(),
            system_prompt: body.to_string(),
            model: None,
            env: Default::default(),
            tools: None,
        }
    }

    /// Populate the resolved-manifest's agent_profiles catalogue with the
    /// built-ins, mirroring what `resolve()` does for real manifests.
    /// `rm_with` constructs `ResolvedManifest` directly and so bypasses
    /// the resolve-time catalogue build.
    fn install_builtins(m: &mut ResolvedManifest) {
        m.agent_profiles = crate::manifest::builtin_profiles::load_builtins()
            .into_iter()
            .map(|p| (p.id.clone(), p))
            .collect();
    }

    #[test]
    fn agent_profile_validate_accepts_only_builtins() {
        let r = rm_with(install_builtins);
        validate_skip_dir_check(&r).expect("built-in-only catalogue must validate");
    }

    #[test]
    fn agent_profile_validate_rejects_pitboss_namespace_squatting() {
        let r = rm_with(|m| {
            install_builtins(m);
            m.agent_profiles.insert(
                "pitboss/not-a-builtin".to_string(),
                ap("pitboss/not-a-builtin", "x"),
            );
        });
        let err = validate_skip_dir_check(&r).unwrap_err().to_string();
        assert!(
            err.contains("pitboss/") && err.contains("reserved"),
            "expected namespace-reservation error, got: {err}"
        );
    }

    #[test]
    fn agent_profile_validate_allows_manifest_shadow_of_builtin() {
        let r = rm_with(|m| {
            install_builtins(m);
            // Overwrite the built-in entry with an operator-shadowed version.
            m.agent_profiles.insert(
                "pitboss/worker-haiku".to_string(),
                ap("pitboss/worker-haiku", "OVERRIDDEN"),
            );
        });
        validate_skip_dir_check(&r)
            .expect("manifest shadowing a built-in by exact id must validate");
    }

    #[test]
    fn agent_profile_validate_rejects_invalid_charset_id() {
        let r = rm_with(|m| {
            install_builtins(m);
            m.agent_profiles
                .insert("has spaces".to_string(), ap("has spaces", "x"));
        });
        let err = validate_skip_dir_check(&r).unwrap_err().to_string();
        assert!(err.contains("^[A-Za-z0-9_/-]+$"), "got: {err}");
    }

    #[test]
    fn agent_profile_validate_rejects_unknown_worker_type_reference() {
        let r = rm_with(|m| {
            install_builtins(m);
            m.worker_types = vec![crate::manifest::schema::WorkerType {
                id: "extractor".into(),
                tools: vec!["Read".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                agent_profile: Some("does/not-exist".into()),
            }];
        });
        let err = validate_skip_dir_check(&r).unwrap_err().to_string();
        assert!(err.contains("does/not-exist"), "got: {err}");
        assert!(err.contains("worker_type"), "got: {err}");
    }

    #[test]
    fn agent_profile_validate_rejects_unknown_sublead_type_reference() {
        let r = rm_with(|m| {
            install_builtins(m);
            m.sublead_types = vec![crate::manifest::schema::SubleadType {
                id: "planner".into(),
                tools: vec!["Read".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                max_budget_usd: None,
                agent_profile: Some("nope".into()),
            }];
        });
        let err = validate_skip_dir_check(&r).unwrap_err().to_string();
        assert!(err.contains("nope"), "got: {err}");
        assert!(err.contains("sublead_type"), "got: {err}");
    }

    #[test]
    fn agent_profile_validate_accepts_known_references() {
        let r = rm_with(|m| {
            install_builtins(m);
            m.worker_types = vec![crate::manifest::schema::WorkerType {
                id: "extractor".into(),
                tools: vec!["Read".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                agent_profile: Some("pitboss/worker-haiku".into()),
            }];
            m.sublead_types = vec![crate::manifest::schema::SubleadType {
                id: "planner".into(),
                tools: vec!["Read".into()],
                allowed_models: vec![],
                max_timeout_secs: None,
                max_budget_usd: None,
                agent_profile: Some("pitboss/sublead-sonnet".into()),
            }];
        });
        validate_skip_dir_check(&r).expect("known built-in references must validate");
    }

    // ── #555: [run].claude_setting_sources validation ─────────────────────

    #[test]
    fn claude_setting_sources_accepts_valid_subset() {
        for valid in [
            "user",
            "project",
            "local",
            "project,local",
            "user,project,local",
        ] {
            let r = rm_with(|m| m.claude_setting_sources = Some(valid.into()));
            validate_skip_dir_check(&r)
                .unwrap_or_else(|e| panic!("valid value {valid:?} must accept: {e}"));
        }
    }

    #[test]
    fn claude_setting_sources_accepts_omitted() {
        // Back-compat: pre-#555 manifests have no field. Must still pass.
        let r = rm_with(|m| m.claude_setting_sources = None);
        validate_skip_dir_check(&r).expect("omitted field is back-compat default");
    }

    #[test]
    fn claude_setting_sources_rejects_empty_string() {
        // Explicit empty string is ambiguous — operator should omit the
        // field instead.
        let r = rm_with(|m| m.claude_setting_sources = Some(String::new()));
        let err = validate_skip_dir_check(&r).unwrap_err().to_string();
        assert!(err.contains("empty string"), "got: {err}");
        assert!(err.contains("claude_setting_sources"), "got: {err}");
    }

    #[test]
    fn claude_setting_sources_rejects_unknown_token() {
        let r = rm_with(|m| m.claude_setting_sources = Some("project,enterprise".into()));
        let err = validate_skip_dir_check(&r).unwrap_err().to_string();
        assert!(err.contains("enterprise"), "got: {err}");
        assert!(err.contains("--setting-sources"), "got: {err}");
    }

    #[test]
    fn claude_setting_sources_rejects_duplicate_token() {
        let r = rm_with(|m| m.claude_setting_sources = Some("project,project".into()));
        let err = validate_skip_dir_check(&r).unwrap_err().to_string();
        assert!(err.contains("more than once"), "got: {err}");
    }

    #[test]
    fn claude_setting_sources_rejects_trailing_comma() {
        // `"project,"` parses as `["project", ""]` — an empty token.
        let r = rm_with(|m| m.claude_setting_sources = Some("project,".into()));
        let err = validate_skip_dir_check(&r).unwrap_err().to_string();
        assert!(err.contains("empty token"), "got: {err}");
    }

    /// The whitespace-around-comma trap is real: an operator types
    /// `"project, local"` from muscle memory, the prior validator
    /// trimmed each token and let it pass — but the un-trimmed value
    /// flowed to `claude --setting-sources` verbatim, which doesn't
    /// accept space-padded values. Pin the rejection so a future
    /// validator refactor doesn't quietly restore the silent-pass
    /// behavior. The error must name the canonical form so an operator
    /// can fix in one read.
    #[test]
    fn claude_setting_sources_rejects_whitespace() {
        for bad in [
            "project, local", // space after comma — the classic typo
            "project ,local", // space before comma
            " project,local", // leading space
            "project,local ", // trailing space
            "user,\tlocal",   // tab counts as whitespace too
        ] {
            let r = rm_with(|m| m.claude_setting_sources = Some(bad.to_string()));
            let result = validate_skip_dir_check(&r);
            assert!(result.is_err(), "{bad:?} should reject but didn't");
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("whitespace"),
                "{bad:?} should error on whitespace; got: {err}"
            );
            assert!(
                err.contains("project,local"),
                "{bad:?} error should name the canonical form; got: {err}"
            );
        }
    }
}
