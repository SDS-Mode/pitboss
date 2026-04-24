//! MCP tool handler functions for the shared store. Each tool extracts
//! `CallerIdentity` from the `_meta` field that `mcp-bridge` injects into
//! `params.arguments`, then delegates to the `SharedStore` API.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::leases::AcquireResult;
use super::{ActorRole, CallerIdentity, Entry, SharedStore, StoreError};

// ---------- Meta field injected by the bridge ----------

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct MetaField {
    pub actor_id: String,
    pub actor_role: ActorRole,
}

impl From<MetaField> for CallerIdentity {
    fn from(m: MetaField) -> Self {
        CallerIdentity {
            id: m.actor_id,
            role: m.actor_role,
        }
    }
}

// ---------- Args ----------
// Read-only tools accept an Option<MetaField> (reads are unrestricted;
// identity doesn't affect the outcome). Write tools require it (missing
// meta -> deserialization error -> MCP parse error -> effectively
// "unauthenticated").

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KvGetArgs {
    pub path: String,
    #[serde(rename = "_meta", default)]
    #[schemars(skip)]
    pub meta: Option<MetaField>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KvSetArgs {
    pub path: String,
    pub value: Vec<u8>,
    #[serde(default)]
    pub override_flag: bool,
    #[serde(rename = "_meta")]
    pub meta: MetaField,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KvCasArgs {
    pub path: String,
    pub expected_version: u64,
    pub new_value: Vec<u8>,
    #[serde(default)]
    pub override_flag: bool,
    #[serde(rename = "_meta")]
    pub meta: MetaField,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KvListArgs {
    pub glob: String,
    #[serde(rename = "_meta", default)]
    #[schemars(skip)]
    pub meta: Option<MetaField>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KvWaitArgs {
    pub path: String,
    pub timeout_secs: u32,
    #[serde(default)]
    pub min_version: Option<u64>,
    #[serde(rename = "_meta", default)]
    #[schemars(skip)]
    pub meta: Option<MetaField>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LeaseAcquireArgs {
    pub name: String,
    pub ttl_secs: u32,
    #[serde(default)]
    pub wait_secs: Option<u32>,
    #[serde(rename = "_meta")]
    pub meta: MetaField,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LeaseReleaseArgs {
    pub lease_id: String,
    #[serde(rename = "_meta")]
    pub meta: MetaField,
}

// ---------- Returns ----------

#[derive(Debug, Serialize)]
pub struct KvSetResult {
    pub version: u64,
}

// ---------- Path helpers ----------

/// Rewrite `/peer/self/...` to `/peer/<caller_id>/...` so workers can use
/// the generic `self` keyword without knowing their own actor_id.
///
/// Workers don't have a natural way to discover their actor_id (the
/// dispatcher assigns UUIDs for dynamically-spawned workers). Before this
/// helper, a task-prompt instruction like "write your findings to
/// `/peer/p0-4/findings.md`" would fail authz because the worker's actual
/// actor_id is a UUID, not `p0-4`. Workers would then narrate about the
/// mismatch and waste turns experimenting with paths. `self` is an
/// always-correct alias.
///
/// Applies to both `/peer/self/...` (single-segment) and `/peer/self`
/// (exact). Any other path is returned unchanged.
fn resolve_peer_self(path: &str, caller_id: &str) -> String {
    if path == "/peer/self" {
        return format!("/peer/{caller_id}");
    }
    if let Some(rest) = path.strip_prefix("/peer/self/") {
        return format!("/peer/{caller_id}/{rest}");
    }
    path.to_string()
}

/// Reject `self`-aliased paths when identity is missing. Used by read-only
/// tools where `meta` is optional — if a caller omits `_meta` AND uses
/// `/peer/self/...`, we can't resolve and return a clear error instead of
/// silently reading the literal path `/peer/self/...`.
fn require_identity_for_self(path: &str, meta: Option<&MetaField>) -> Result<String, StoreError> {
    if path.starts_with("/peer/self") {
        let Some(m) = meta else {
            return Err(StoreError::InvalidArg(
                "/peer/self/... requires caller identity (missing _meta). \
                 Prefer /peer/<actor_id>/... when calling without identity."
                    .into(),
            ));
        };
        return Ok(resolve_peer_self(path, &m.actor_id));
    }
    Ok(path.to_string())
}

// ---------- Handlers ----------

// Counter-bump policy: every tool call that carries an actor identity
// bumps the matching counter EXACTLY ONCE at entry, BEFORE authz or
// execution. That way "tried and got denied" still shows up in the TUI
// — often the most useful signal when debugging a worker that's spinning
// on bad paths. For tools where identity is optional (Option<MetaField>
// — reads), we only bump when it's actually present.

pub async fn handle_kv_get(
    store: &Arc<SharedStore>,
    args: KvGetArgs,
) -> Result<Option<Entry>, StoreError> {
    if let Some(m) = &args.meta {
        store.note_kv_op(&m.actor_id).await;
    }
    let path = require_identity_for_self(&args.path, args.meta.as_ref())?;
    Ok(store.get(&path).await)
}

pub async fn handle_kv_set(
    store: &Arc<SharedStore>,
    args: KvSetArgs,
) -> Result<KvSetResult, StoreError> {
    store.note_kv_op(&args.meta.actor_id).await;
    let caller: CallerIdentity = args.meta.into();
    let path = resolve_peer_self(&args.path, &caller.id);
    let version = store
        .authorized_set(&path, args.value, &caller, args.override_flag)
        .await?;
    Ok(KvSetResult { version })
}

pub async fn handle_kv_cas(
    store: &Arc<SharedStore>,
    args: KvCasArgs,
) -> Result<super::CasResult, StoreError> {
    store.note_kv_op(&args.meta.actor_id).await;
    let caller: CallerIdentity = args.meta.into();
    let path = resolve_peer_self(&args.path, &caller.id);
    store
        .authorized_cas(
            &path,
            args.expected_version,
            args.new_value,
            &caller,
            args.override_flag,
        )
        .await
}

pub async fn handle_kv_list(
    store: &Arc<SharedStore>,
    args: KvListArgs,
) -> Result<crate::shared_store::ListResult, StoreError> {
    if let Some(m) = &args.meta {
        store.note_kv_op(&m.actor_id).await;
    }
    let glob = require_identity_for_self(&args.glob, args.meta.as_ref())?;
    store.list_with_truncation(&glob).await
}

pub async fn handle_kv_wait(
    store: &Arc<SharedStore>,
    args: KvWaitArgs,
) -> Result<Entry, StoreError> {
    if let Some(m) = &args.meta {
        store.note_kv_op(&m.actor_id).await;
    }
    let path = require_identity_for_self(&args.path, args.meta.as_ref())?;
    store
        .wait(
            &path,
            Duration::from_secs(u64::from(args.timeout_secs)),
            args.min_version,
        )
        .await
}

pub async fn handle_lease_acquire(
    store: &Arc<SharedStore>,
    args: LeaseAcquireArgs,
) -> Result<AcquireResult, StoreError> {
    store.note_lease_op(&args.meta.actor_id).await;
    let caller: CallerIdentity = args.meta.into();
    store
        .lease_acquire(
            &args.name,
            Duration::from_secs(u64::from(args.ttl_secs)),
            args.wait_secs.map(|s| Duration::from_secs(u64::from(s))),
            &caller,
        )
        .await
}

pub async fn handle_lease_release(
    store: &Arc<SharedStore>,
    args: LeaseReleaseArgs,
) -> Result<(), StoreError> {
    store.note_lease_op(&args.meta.actor_id).await;
    let caller: CallerIdentity = args.meta.into();
    store.lease_release(&args.lease_id, &caller).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_peer_self_rewrites_exact_match() {
        assert_eq!(
            resolve_peer_self("/peer/self", "worker-abc"),
            "/peer/worker-abc"
        );
    }

    #[test]
    fn resolve_peer_self_rewrites_subpath() {
        assert_eq!(
            resolve_peer_self("/peer/self/findings.md", "worker-abc"),
            "/peer/worker-abc/findings.md"
        );
    }

    #[test]
    fn resolve_peer_self_rewrites_nested_subpath() {
        assert_eq!(
            resolve_peer_self("/peer/self/out/a/b.json", "worker-abc"),
            "/peer/worker-abc/out/a/b.json"
        );
    }

    #[test]
    fn resolve_peer_self_leaves_other_paths_untouched() {
        assert_eq!(resolve_peer_self("/peer/other/x", "me"), "/peer/other/x");
        assert_eq!(resolve_peer_self("/shared/foo", "me"), "/shared/foo");
        assert_eq!(resolve_peer_self("/ref/x", "me"), "/ref/x");
    }

    #[test]
    fn resolve_peer_self_does_not_rewrite_peer_selfx() {
        // Path `/peer/selfish/...` is a different peer id that happens to
        // start with "self" — must NOT be rewritten.
        assert_eq!(
            resolve_peer_self("/peer/selfish/out", "me"),
            "/peer/selfish/out"
        );
    }

    #[test]
    fn require_identity_rejects_self_without_meta() {
        let err = require_identity_for_self("/peer/self/x", None).unwrap_err();
        assert!(matches!(err, StoreError::InvalidArg(_)));
    }

    #[test]
    fn require_identity_allows_non_self_without_meta() {
        // Reads to any other path are fine without meta.
        let out = require_identity_for_self("/shared/x", None).unwrap();
        assert_eq!(out, "/shared/x");
    }

    #[test]
    fn require_identity_rewrites_self_with_meta() {
        let meta = MetaField {
            actor_id: "worker-abc".into(),
            actor_role: ActorRole::Worker,
        };
        let out = require_identity_for_self("/peer/self/x", Some(&meta)).unwrap();
        assert_eq!(out, "/peer/worker-abc/x");
    }
}
