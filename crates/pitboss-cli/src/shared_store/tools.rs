//! MCP tool handler functions for the shared store. Each tool extracts
//! `CallerIdentity` from the `_meta` field that `mcp-bridge` injects into
//! `params.arguments`, then delegates to the `SharedStore` API.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::leases::AcquireResult;
use super::{ActorRole, CallerIdentity, Entry, ListMetadata, SharedStore, StoreError};

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

// ---------- Handlers ----------

pub async fn handle_kv_get(
    store: &Arc<SharedStore>,
    args: KvGetArgs,
) -> Result<Option<Entry>, StoreError> {
    Ok(store.get(&args.path).await)
}

pub async fn handle_kv_set(
    store: &Arc<SharedStore>,
    args: KvSetArgs,
) -> Result<KvSetResult, StoreError> {
    let caller: CallerIdentity = args.meta.into();
    let version = store
        .authorized_set(&args.path, args.value, &caller, args.override_flag)
        .await?;
    Ok(KvSetResult { version })
}

pub async fn handle_kv_cas(
    store: &Arc<SharedStore>,
    args: KvCasArgs,
) -> Result<super::CasResult, StoreError> {
    let caller: CallerIdentity = args.meta.into();
    store
        .authorized_cas(
            &args.path,
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
) -> Result<Vec<ListMetadata>, StoreError> {
    store.list(&args.glob).await
}

pub async fn handle_kv_wait(
    store: &Arc<SharedStore>,
    args: KvWaitArgs,
) -> Result<Entry, StoreError> {
    store
        .wait(
            &args.path,
            Duration::from_secs(u64::from(args.timeout_secs)),
            args.min_version,
        )
        .await
}

pub async fn handle_lease_acquire(
    store: &Arc<SharedStore>,
    args: LeaseAcquireArgs,
) -> Result<AcquireResult, StoreError> {
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
    let caller: CallerIdentity = args.meta.into();
    store.lease_release(&args.lease_id, &caller).await
}
