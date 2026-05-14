//! Pitboss-owned directional communication plane.
//!
//! KV remains the small metadata/coordination surface. This module owns
//! mailbox messages and larger artifacts so actors transfer payloads by
//! Pitboss-managed ids rather than by stuffing serialized blobs into KV.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::dispatch::state::DispatchState;
use crate::manifest::schema::{CommunicationConfig, CommunicationMode};
use crate::shared_store::tools::MetaField;
use crate::shared_store::ActorRole;

#[derive(Debug, thiserror::Error)]
pub enum CommunicationError {
    #[error("communication tools are disabled by [communication].mode")]
    Disabled,
    #[error("caller identity required (missing _meta)")]
    MissingIdentity,
    #[error("unknown actor: {0}")]
    UnknownActor(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("message body exceeds max_message_bytes ({actual} > {limit})")]
    MessageTooLarge { actual: usize, limit: usize },
    #[error("artifact payload exceeds max_artifact_bytes ({actual} > {limit})")]
    ArtifactTooLarge { actual: usize, limit: usize },
    #[error("artifact count limit exceeded for {actor_id} ({limit})")]
    ArtifactCountExceeded { actor_id: String, limit: usize },
    #[error("invalid base64 content: {0}")]
    InvalidBase64(String),
    #[error("artifact storage error: {0}")]
    Storage(String),
    #[error("invalid scope: {0}")]
    InvalidScope(String),
}

#[derive(Debug, Clone)]
struct Caller {
    id: String,
    role: ActorRole,
}

impl From<MetaField> for Caller {
    fn from(meta: MetaField) -> Self {
        Self {
            id: meta.actor_id,
            role: meta.actor_role,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub message_id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub body: String,
    pub refs: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub acked_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageSummary {
    pub message_id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub refs: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub acked_by: Vec<String>,
}

#[derive(Debug, Clone)]
struct StoredMessage {
    message_id: String,
    from: String,
    to: String,
    subject: String,
    body: String,
    refs: Vec<String>,
    created_at: DateTime<Utc>,
    acked_by: HashSet<String>,
}

impl StoredMessage {
    fn summary(&self) -> MessageSummary {
        let mut acked_by: Vec<String> = self.acked_by.iter().cloned().collect();
        acked_by.sort();
        MessageSummary {
            message_id: self.message_id.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            subject: self.subject.clone(),
            refs: self.refs.clone(),
            created_at: self.created_at,
            acked_by,
        }
    }

    fn full(&self) -> Message {
        let mut acked_by: Vec<String> = self.acked_by.iter().cloned().collect();
        acked_by.sort();
        Message {
            message_id: self.message_id.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            subject: self.subject.clone(),
            body: self.body.clone(),
            refs: self.refs.clone(),
            created_at: self.created_at,
            acked_by,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactMetadata {
    pub artifact_id: String,
    pub uri: String,
    pub owner: String,
    pub name: String,
    pub mime_type: Option<String>,
    pub size_bytes: u64,
    pub sha256: String,
    pub created_at: DateTime<Utc>,
    pub grants: Vec<String>,
}

#[derive(Debug, Clone)]
struct StoredArtifact {
    metadata: ArtifactMetadata,
    path: PathBuf,
    grants: HashSet<String>,
}

impl StoredArtifact {
    fn metadata(&self) -> ArtifactMetadata {
        let mut metadata = self.metadata.clone();
        metadata.grants = self.grants.iter().cloned().collect();
        metadata.grants.sort();
        metadata
    }
}

pub struct CommunicationStore {
    config: CommunicationConfig,
    artifact_dir: PathBuf,
    /// Append-only journal at `<run_subdir>/communication/messages.jsonl`.
    /// Every mutating handler writes a typed [`JournalRecord`] after its
    /// in-memory write succeeds; [`CommunicationStore::new`] replays the
    /// file on construction so `pitboss resume` rehydrates the mailbox
    /// + artifact metadata that existed pre-crash. See #282.
    journal_path: PathBuf,
    messages: RwLock<HashMap<String, StoredMessage>>,
    artifacts: RwLock<HashMap<String, StoredArtifact>>,
    activity: RwLock<HashMap<String, CommunicationActivityCounters>>,
}

/// One line in the `messages.jsonl` journal (#282). The journal exists so
/// `pitboss resume` rebuilds in-memory mailbox + artifact metadata after a
/// crash; artifact bytes are on disk under `communication/artifacts/<id>`
/// independently.
///
/// Each mutating handler emits one record after its in-memory write
/// succeeds. The variants mirror the four observable state transitions
/// the handlers perform; `note_message_op` / `note_artifact_op` activity
/// counters are derived on replay from the records themselves
/// (`MessageSend`/`MessageAck` → `message_ops`,
/// `ArtifactPut`/`ArtifactGrant` → `artifact_ops`).
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
enum JournalRecord {
    MessageSend {
        message: Message,
    },
    MessageAck {
        message_id: String,
        by: String,
    },
    ArtifactPut {
        metadata: ArtifactMetadata,
    },
    /// `by` records the grantor so replay can correctly attribute the
    /// `artifact_ops` counter increment that `note_artifact_op` made
    /// before the in-memory write. Beyond the original #282 spec, which
    /// only listed `artifact_id` + `to` — without `by` the grantor's
    /// counter would silently drop on replay.
    ArtifactGrant {
        artifact_id: String,
        to: String,
        by: String,
    },
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommunicationActivityCounters {
    pub message_ops: u64,
    pub artifact_ops: u64,
}

impl CommunicationStore {
    pub fn new(config: CommunicationConfig, run_subdir: PathBuf) -> Self {
        let comm_dir = run_subdir.join("communication");
        let artifact_dir = comm_dir.join("artifacts");
        let journal_path = comm_dir.join("messages.jsonl");

        // #282: replay the journal if present so a resumed dispatcher
        // inherits the pre-crash mailbox + artifact metadata. `new` is
        // sync (called from `DispatchState::new`) so the replay uses
        // std::fs; this is one-shot at run start, not a hot path.
        let mut messages: HashMap<String, StoredMessage> = HashMap::new();
        let mut artifacts: HashMap<String, StoredArtifact> = HashMap::new();
        let mut activity: HashMap<String, CommunicationActivityCounters> = HashMap::new();
        replay_journal(
            &journal_path,
            &artifact_dir,
            &mut messages,
            &mut artifacts,
            &mut activity,
        );

        Self {
            config,
            artifact_dir,
            journal_path,
            messages: RwLock::new(messages),
            artifacts: RwLock::new(artifacts),
            activity: RwLock::new(activity),
        }
    }

    pub fn config(&self) -> &CommunicationConfig {
        &self.config
    }

    /// Path to the append-only journal (`<run_subdir>/communication/messages.jsonl`).
    /// Exposed crate-private so the handler helpers can call
    /// [`journal_append`] after a successful in-memory write.
    pub(crate) fn journal_path(&self) -> &Path {
        &self.journal_path
    }

    pub async fn note_message_op(&self, actor_id: &str) {
        if actor_id.is_empty() {
            return;
        }
        let mut activity = self.activity.write().await;
        activity
            .entry(actor_id.to_string())
            .or_default()
            .message_ops += 1;
    }

    pub async fn note_artifact_op(&self, actor_id: &str) {
        if actor_id.is_empty() {
            return;
        }
        let mut activity = self.activity.write().await;
        activity
            .entry(actor_id.to_string())
            .or_default()
            .artifact_ops += 1;
    }

    pub async fn activity_snapshot(&self) -> HashMap<String, CommunicationActivityCounters> {
        self.activity.read().await.clone()
    }
}

/// Read the `messages.jsonl` journal (if present) and seed in-memory
/// state from it. Mirrors `pitboss_core::store::summary::overlay_summary_jsonl`:
/// tolerant of blank lines and torn trailing writes (`Err → warn + skip`).
///
/// Each successfully-parsed record reconstructs both the entity (message
/// or artifact) AND the activity counter increment the live handler
/// would have made via `note_message_op` / `note_artifact_op`.
fn replay_journal(
    journal_path: &Path,
    artifact_dir: &Path,
    messages: &mut HashMap<String, StoredMessage>,
    artifacts: &mut HashMap<String, StoredArtifact>,
    activity: &mut HashMap<String, CommunicationActivityCounters>,
) {
    let Ok(text) = std::fs::read_to_string(journal_path) else {
        return;
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let record: JournalRecord = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    line = trimmed,
                    path = %journal_path.display(),
                    "messages.jsonl: skipping unparseable line",
                );
                continue;
            }
        };
        apply_journal_record(record, artifact_dir, messages, artifacts, activity);
    }
}

/// Apply a single replayed record to the rebuilt in-memory state.
/// Split out from `replay_journal` so tests can drive the apply path
/// directly without touching the filesystem if they need to.
fn apply_journal_record(
    record: JournalRecord,
    artifact_dir: &Path,
    messages: &mut HashMap<String, StoredMessage>,
    artifacts: &mut HashMap<String, StoredArtifact>,
    activity: &mut HashMap<String, CommunicationActivityCounters>,
) {
    match record {
        JournalRecord::MessageSend { message } => {
            let stored = StoredMessage {
                message_id: message.message_id.clone(),
                from: message.from.clone(),
                to: message.to,
                subject: message.subject,
                body: message.body,
                refs: message.refs,
                created_at: message.created_at,
                acked_by: message.acked_by.into_iter().collect(),
            };
            if !stored.from.is_empty() {
                activity.entry(stored.from.clone()).or_default().message_ops += 1;
            }
            messages.insert(message.message_id, stored);
        }
        JournalRecord::MessageAck { message_id, by } => {
            if let Some(msg) = messages.get_mut(&message_id) {
                msg.acked_by.insert(by.clone());
            }
            if !by.is_empty() {
                activity.entry(by).or_default().message_ops += 1;
            }
        }
        JournalRecord::ArtifactPut { metadata } => {
            if !metadata.owner.is_empty() {
                activity
                    .entry(metadata.owner.clone())
                    .or_default()
                    .artifact_ops += 1;
            }
            let path = artifact_dir.join(&metadata.artifact_id);
            let grants: HashSet<String> = metadata.grants.iter().cloned().collect();
            artifacts.insert(
                metadata.artifact_id.clone(),
                StoredArtifact {
                    metadata,
                    path,
                    grants,
                },
            );
        }
        JournalRecord::ArtifactGrant {
            artifact_id,
            to,
            by,
        } => {
            if let Some(art) = artifacts.get_mut(&artifact_id) {
                art.grants.insert(to);
            }
            if !by.is_empty() {
                activity.entry(by).or_default().artifact_ops += 1;
            }
        }
    }
}

/// Append one [`JournalRecord`] to the messages.jsonl journal. Best-effort:
/// errors are logged and swallowed so a transient I/O failure can't break
/// the live mailbox or wedge a handler. The in-memory state is then
/// briefly ahead of the journal until the next successful append — a
/// fine recovery trade because the operator sees what happened in
/// memory, and the next write that succeeds resyncs the journal.
async fn journal_append(path: &Path, record: &JournalRecord) {
    // Lazily ensure the parent directory exists. For message-only runs
    // nothing else creates `<run_subdir>/communication/`; for
    // artifact-using runs `handle_artifact_put` already creates the
    // sibling `artifacts/` directory, but we don't take a dependency
    // on call order.
    if let Some(parent) = path.parent() {
        if let Err(e) = tokio::fs::create_dir_all(parent).await {
            tracing::warn!(
                error = %e,
                path = %parent.display(),
                "messages.jsonl: create_dir_all failed; journal append skipped",
            );
            return;
        }
    }
    let mut line = match serde_json::to_vec(record) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "messages.jsonl: serialize failed; journal append skipped");
            return;
        }
    };
    line.push(b'\n');
    let mut file = match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "messages.jsonl: open failed; journal append skipped",
            );
            return;
        }
    };
    if let Err(e) = file.write_all(&line).await {
        tracing::warn!(
            error = %e,
            path = %path.display(),
            "messages.jsonl: write failed; partial line possible (next replay tolerates)",
        );
        return;
    }
    // `sync_all` matches `pitboss_core::store::json_file::append_record`'s
    // durability guarantee — after this returns, the row survives a
    // power loss / kernel panic.
    if let Err(e) = file.sync_all().await {
        tracing::warn!(
            error = %e,
            path = %path.display(),
            "messages.jsonl: sync_all failed",
        );
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MessageSendArgs {
    pub to: String,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub refs: Vec<String>,
    #[serde(rename = "_meta")]
    #[schemars(skip)]
    pub meta: MetaField,
}

#[derive(Debug, Serialize)]
pub struct MessageSendResult {
    pub message_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MessageListArgs {
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(rename = "_meta")]
    #[schemars(skip)]
    pub meta: MetaField,
}

#[derive(Debug, Serialize)]
pub struct MessageListResult {
    pub messages: Vec<MessageSummary>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MessageReadArgs {
    pub message_id: String,
    #[serde(rename = "_meta")]
    #[schemars(skip)]
    pub meta: MetaField,
}

#[derive(Debug, Serialize)]
pub struct MessageReadResult {
    pub message: Message,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MessageAckArgs {
    pub message_id: String,
    #[serde(rename = "_meta")]
    #[schemars(skip)]
    pub meta: MetaField,
}

#[derive(Debug, Serialize)]
pub struct OkResult {
    pub ok: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ArtifactPutArgs {
    pub name: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    pub content_base64: String,
    #[serde(rename = "_meta")]
    #[schemars(skip)]
    pub meta: MetaField,
}

#[derive(Debug, Serialize)]
pub struct ArtifactPutResult {
    pub artifact_id: String,
    pub uri: String,
    pub size_bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ArtifactListArgs {
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(rename = "_meta")]
    #[schemars(skip)]
    pub meta: MetaField,
}

#[derive(Debug, Serialize)]
pub struct ArtifactListResult {
    pub artifacts: Vec<ArtifactMetadata>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ArtifactReadArgs {
    pub artifact_id: String,
    #[serde(rename = "_meta")]
    #[schemars(skip)]
    pub meta: MetaField,
}

#[derive(Debug, Serialize)]
pub struct ArtifactReadResult {
    pub artifact: ArtifactMetadata,
    pub content_base64: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ArtifactGrantArgs {
    pub artifact_id: String,
    pub to: String,
    #[serde(rename = "_meta")]
    #[schemars(skip)]
    pub meta: MetaField,
}

pub async fn handle_message_send(
    state: &Arc<DispatchState>,
    args: MessageSendArgs,
) -> Result<MessageSendResult, CommunicationError> {
    ensure_enabled(state)?;
    let caller: Caller = args.meta.into();
    state.communication.note_message_op(&caller.id).await;
    let limit = state.root.manifest.communication.max_message_bytes as usize;
    if args.body.len() > limit {
        return Err(CommunicationError::MessageTooLarge {
            actual: args.body.len(),
            limit,
        });
    }
    let to = resolve_recipient(state, &caller, &args.to).await?;
    ensure_can_send(state, &caller, &to).await?;

    let message_id = Uuid::now_v7().to_string();
    let message = StoredMessage {
        message_id: message_id.clone(),
        from: caller.id,
        to,
        subject: args.subject,
        body: args.body,
        refs: args.refs,
        created_at: Utc::now(),
        acked_by: HashSet::new(),
    };
    let journal_record = JournalRecord::MessageSend {
        message: message.full(),
    };
    state
        .communication
        .messages
        .write()
        .await
        .insert(message_id.clone(), message);
    // #282: journal AFTER the in-memory write succeeds. Append errors
    // are logged + swallowed inside `journal_append` so a transient I/O
    // failure can't fail the live op.
    journal_append(state.communication.journal_path(), &journal_record).await;
    Ok(MessageSendResult { message_id })
}

pub async fn handle_message_list(
    state: &Arc<DispatchState>,
    args: MessageListArgs,
) -> Result<MessageListResult, CommunicationError> {
    ensure_enabled(state)?;
    let caller: Caller = args.meta.into();
    state.communication.note_message_op(&caller.id).await;
    let scope = args.scope.as_deref().unwrap_or("inbox");
    let messages = state.communication.messages.read().await;
    let mut out = Vec::new();
    for message in messages.values() {
        let visible = match scope {
            "inbox" => message.to == caller.id,
            "sent" => message.from == caller.id,
            "visible" => can_read_message(state, &caller, message).await?,
            "all" => {
                if caller.role != ActorRole::Lead {
                    return Err(CommunicationError::Forbidden(
                        "message_list scope=all is root-lead only".into(),
                    ));
                }
                true
            }
            other => return Err(CommunicationError::InvalidScope(other.into())),
        };
        if visible {
            out.push(message.summary());
        }
    }
    out.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then(a.message_id.cmp(&b.message_id))
    });
    Ok(MessageListResult { messages: out })
}

pub async fn handle_message_read(
    state: &Arc<DispatchState>,
    args: MessageReadArgs,
) -> Result<MessageReadResult, CommunicationError> {
    ensure_enabled(state)?;
    let caller: Caller = args.meta.into();
    state.communication.note_message_op(&caller.id).await;
    let messages = state.communication.messages.read().await;
    let message = messages
        .get(&args.message_id)
        .ok_or_else(|| CommunicationError::NotFound(args.message_id.clone()))?;
    if !can_read_message(state, &caller, message).await? {
        return Err(CommunicationError::Forbidden(format!(
            "{} cannot read message {}",
            caller.id, args.message_id
        )));
    }
    Ok(MessageReadResult {
        message: message.full(),
    })
}

pub async fn handle_message_ack(
    state: &Arc<DispatchState>,
    args: MessageAckArgs,
) -> Result<OkResult, CommunicationError> {
    ensure_enabled(state)?;
    let caller: Caller = args.meta.into();
    state.communication.note_message_op(&caller.id).await;
    let journal_record = {
        let mut messages = state.communication.messages.write().await;
        let message = messages
            .get_mut(&args.message_id)
            .ok_or_else(|| CommunicationError::NotFound(args.message_id.clone()))?;
        if !can_read_message(state, &caller, message).await? {
            return Err(CommunicationError::Forbidden(format!(
                "{} cannot ack message {}",
                caller.id, args.message_id
            )));
        }
        message.acked_by.insert(caller.id.clone());
        JournalRecord::MessageAck {
            message_id: args.message_id,
            by: caller.id,
        }
    };
    // #282: journal AFTER the in-memory ack lands. Lock dropped above
    // so the append doesn't hold the messages-write lock across I/O.
    journal_append(state.communication.journal_path(), &journal_record).await;
    Ok(OkResult { ok: true })
}

pub async fn handle_artifact_put(
    state: &Arc<DispatchState>,
    args: ArtifactPutArgs,
) -> Result<ArtifactPutResult, CommunicationError> {
    ensure_enabled(state)?;
    let caller: Caller = args.meta.into();
    state.communication.note_artifact_op(&caller.id).await;
    let content = base64::engine::general_purpose::STANDARD
        .decode(args.content_base64.as_bytes())
        .map_err(|e| CommunicationError::InvalidBase64(e.to_string()))?;
    let limit = state.root.manifest.communication.max_artifact_bytes as usize;
    if content.len() > limit {
        return Err(CommunicationError::ArtifactTooLarge {
            actual: content.len(),
            limit,
        });
    }
    let per_actor_limit = state.root.manifest.communication.max_artifacts_per_actor as usize;

    // Filesystem I/O happens *outside* the artifacts write lock so concurrent
    // artifact_put calls don't serialize on the lock for the duration of the
    // disk write. The lock is acquired briefly at the end to re-check the
    // per-actor count and insert the metadata. If the count check fails after
    // the bytes are on disk we unlink the orphan; this trades a rare wasted
    // write on overflow for a non-blocking common path.
    tokio::fs::create_dir_all(&state.communication.artifact_dir)
        .await
        .map_err(|e| CommunicationError::Storage(e.to_string()))?;
    let artifact_id = Uuid::now_v7().to_string();
    let path = state.communication.artifact_dir.join(&artifact_id);
    tokio::fs::write(&path, &content)
        .await
        .map_err(|e| CommunicationError::Storage(e.to_string()))?;

    let digest = Sha256::digest(&content);
    let sha256 = hex::encode(digest);
    let uri = format!("pitboss://run/{}/artifact/{artifact_id}", state.root.run_id);
    let metadata = ArtifactMetadata {
        artifact_id: artifact_id.clone(),
        uri: uri.clone(),
        owner: caller.id.clone(),
        name: args.name,
        mime_type: args.mime_type,
        size_bytes: content.len() as u64,
        sha256: sha256.clone(),
        created_at: Utc::now(),
        grants: Vec::new(),
    };

    let journal_metadata = metadata.clone();
    {
        let mut artifacts = state.communication.artifacts.write().await;
        let current_count = artifacts
            .values()
            .filter(|artifact| artifact.metadata.owner == caller.id)
            .count();
        if current_count >= per_actor_limit {
            drop(artifacts);
            let _ = tokio::fs::remove_file(&path).await;
            return Err(CommunicationError::ArtifactCountExceeded {
                actor_id: caller.id,
                limit: per_actor_limit,
            });
        }
        artifacts.insert(
            artifact_id.clone(),
            StoredArtifact {
                metadata,
                path,
                grants: HashSet::new(),
            },
        );
    }

    // #282: journal AFTER the in-memory insert. The bytes are already
    // on disk by this point; the journal records the metadata so a
    // resume rebuilds the in-memory entry without re-reading every
    // artifact directory entry.
    journal_append(
        state.communication.journal_path(),
        &JournalRecord::ArtifactPut {
            metadata: journal_metadata,
        },
    )
    .await;

    Ok(ArtifactPutResult {
        artifact_id,
        uri,
        size_bytes: content.len() as u64,
        sha256,
    })
}

pub async fn handle_artifact_list(
    state: &Arc<DispatchState>,
    args: ArtifactListArgs,
) -> Result<ArtifactListResult, CommunicationError> {
    ensure_enabled(state)?;
    let caller: Caller = args.meta.into();
    state.communication.note_artifact_op(&caller.id).await;
    let scope = args.scope.as_deref().unwrap_or("visible");
    let artifacts = state.communication.artifacts.read().await;
    let mut out = Vec::new();
    for artifact in artifacts.values() {
        let visible = match scope {
            "owned" => artifact.metadata.owner == caller.id,
            "granted" => artifact.grants.contains(&caller.id),
            "visible" => can_read_artifact(state, &caller, artifact).await?,
            "all" => {
                if caller.role != ActorRole::Lead {
                    return Err(CommunicationError::Forbidden(
                        "artifact_list scope=all is root-lead only".into(),
                    ));
                }
                true
            }
            other => return Err(CommunicationError::InvalidScope(other.into())),
        };
        if visible {
            out.push(artifact.metadata());
        }
    }
    out.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then(a.artifact_id.cmp(&b.artifact_id))
    });
    Ok(ArtifactListResult { artifacts: out })
}

pub async fn handle_artifact_read(
    state: &Arc<DispatchState>,
    args: ArtifactReadArgs,
) -> Result<ArtifactReadResult, CommunicationError> {
    ensure_enabled(state)?;
    let caller: Caller = args.meta.into();
    state.communication.note_artifact_op(&caller.id).await;
    let (metadata, path) = {
        let artifacts = state.communication.artifacts.read().await;
        let artifact = artifacts
            .get(&args.artifact_id)
            .ok_or_else(|| CommunicationError::NotFound(args.artifact_id.clone()))?;
        if !can_read_artifact(state, &caller, artifact).await? {
            return Err(CommunicationError::Forbidden(format!(
                "{} cannot read artifact {}",
                caller.id, args.artifact_id
            )));
        }
        (artifact.metadata(), artifact.path.clone())
    };
    let content = tokio::fs::read(&path)
        .await
        .map_err(|e| CommunicationError::Storage(e.to_string()))?;
    Ok(ArtifactReadResult {
        artifact: metadata,
        content_base64: base64::engine::general_purpose::STANDARD.encode(content),
    })
}

pub async fn handle_artifact_grant(
    state: &Arc<DispatchState>,
    args: ArtifactGrantArgs,
) -> Result<OkResult, CommunicationError> {
    ensure_enabled(state)?;
    let caller: Caller = args.meta.into();
    state.communication.note_artifact_op(&caller.id).await;
    let to = resolve_recipient(state, &caller, &args.to).await?;
    ensure_can_send(state, &caller, &to).await?;
    let journal_record = {
        let mut artifacts = state.communication.artifacts.write().await;
        let artifact = artifacts
            .get_mut(&args.artifact_id)
            .ok_or_else(|| CommunicationError::NotFound(args.artifact_id.clone()))?;
        if !can_read_artifact(state, &caller, artifact).await? {
            return Err(CommunicationError::Forbidden(format!(
                "{} cannot grant artifact {}",
                caller.id, args.artifact_id
            )));
        }
        artifact.grants.insert(to.clone());
        JournalRecord::ArtifactGrant {
            artifact_id: args.artifact_id,
            to,
            by: caller.id,
        }
    };
    // #282: journal AFTER the in-memory grant lands. Lock dropped above
    // so the append doesn't hold the artifacts-write lock across I/O.
    journal_append(state.communication.journal_path(), &journal_record).await;
    Ok(OkResult { ok: true })
}

fn ensure_enabled(state: &Arc<DispatchState>) -> Result<(), CommunicationError> {
    if state.root.manifest.communication.mode == CommunicationMode::Disabled {
        Err(CommunicationError::Disabled)
    } else {
        Ok(())
    }
}

async fn resolve_recipient(
    state: &Arc<DispatchState>,
    caller: &Caller,
    raw: &str,
) -> Result<String, CommunicationError> {
    let resolved = match raw {
        "self" => caller.id.clone(),
        "root" => state.root.lead_id.clone(),
        "parent" => parent_for_actor(state, caller)
            .await?
            .ok_or_else(|| CommunicationError::UnknownActor("parent".into()))?,
        other => other.to_string(),
    };
    if !actor_exists(state, &resolved).await {
        return Err(CommunicationError::UnknownActor(resolved));
    }
    Ok(resolved)
}

async fn ensure_can_send(
    state: &Arc<DispatchState>,
    caller: &Caller,
    to: &str,
) -> Result<(), CommunicationError> {
    match caller.role {
        ActorRole::Lead => Ok(()),
        ActorRole::Sublead => {
            if to == state.root.lead_id
                || to == caller.id
                || worker_parent(state, to).await? == Some(caller.id.clone())
            {
                Ok(())
            } else {
                Err(CommunicationError::Forbidden(format!(
                    "sublead {} can only address root or its own workers",
                    caller.id
                )))
            }
        }
        ActorRole::Worker => {
            let parent = parent_for_actor(state, caller).await?;
            if parent.as_deref() == Some(to) || to == caller.id {
                Ok(())
            } else {
                Err(CommunicationError::Forbidden(format!(
                    "worker {} can only address its parent",
                    caller.id
                )))
            }
        }
    }
}

async fn can_read_message(
    state: &Arc<DispatchState>,
    caller: &Caller,
    message: &StoredMessage,
) -> Result<bool, CommunicationError> {
    Ok(message.from == caller.id
        || message.to == caller.id
        || can_observe_actor(state, caller, &message.from).await?
        || can_observe_actor(state, caller, &message.to).await?)
}

async fn can_read_artifact(
    state: &Arc<DispatchState>,
    caller: &Caller,
    artifact: &StoredArtifact,
) -> Result<bool, CommunicationError> {
    Ok(artifact.metadata.owner == caller.id
        || artifact.grants.contains(&caller.id)
        || can_observe_actor(state, caller, &artifact.metadata.owner).await?)
}

async fn can_observe_actor(
    state: &Arc<DispatchState>,
    caller: &Caller,
    target: &str,
) -> Result<bool, CommunicationError> {
    if caller.id == target {
        return Ok(true);
    }
    match caller.role {
        ActorRole::Lead => Ok(true),
        ActorRole::Sublead => Ok(worker_parent(state, target).await? == Some(caller.id.clone())),
        ActorRole::Worker => Ok(false),
    }
}

async fn parent_for_actor(
    state: &Arc<DispatchState>,
    actor: &Caller,
) -> Result<Option<String>, CommunicationError> {
    match actor.role {
        ActorRole::Lead => Ok(None),
        ActorRole::Sublead => Ok(Some(state.root.lead_id.clone())),
        ActorRole::Worker => worker_parent(state, &actor.id).await,
    }
}

async fn worker_parent(
    state: &Arc<DispatchState>,
    actor_id: &str,
) -> Result<Option<String>, CommunicationError> {
    if let Some(owner) = state.worker_layer_index.read().await.get(actor_id).cloned() {
        return Ok(Some(owner.unwrap_or_else(|| state.root.lead_id.clone())));
    }
    if state.root.workers.read().await.contains_key(actor_id) {
        return Ok(Some(state.root.lead_id.clone()));
    }
    let subleads = state.subleads.read().await;
    for (sublead_id, layer) in subleads.iter() {
        if layer.workers.read().await.contains_key(actor_id) {
            return Ok(Some(sublead_id.clone()));
        }
    }
    let terminated = state.terminated_sublead_layers.read().await;
    for layer in terminated.iter() {
        if layer.workers.read().await.contains_key(actor_id) {
            return Ok(Some(layer.lead_id.clone()));
        }
    }
    Ok(None)
}

async fn actor_exists(state: &Arc<DispatchState>, actor_id: &str) -> bool {
    if actor_id == state.root.lead_id {
        return true;
    }
    if state.root.workers.read().await.contains_key(actor_id) {
        return true;
    }
    let subleads = state.subleads.read().await;
    if subleads.contains_key(actor_id) {
        return true;
    }
    for layer in subleads.values() {
        if layer.workers.read().await.contains_key(actor_id) {
            return true;
        }
    }
    let terminated = state.terminated_sublead_layers.read().await;
    for layer in terminated.iter() {
        if layer.lead_id == actor_id || layer.workers.read().await.contains_key(actor_id) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::layer::LayerState;
    use crate::dispatch::state::{ApprovalPolicy, WorkerState};
    use crate::manifest::resolve::{ResolvedLead, ResolvedManifest};
    use crate::manifest::schema::{Effort, WorktreeCleanup};
    use pitboss_core::process::fake::{FakeScript, FakeSpawner};
    use pitboss_core::process::ProcessSpawner;
    use pitboss_core::session::CancelToken;
    use pitboss_core::store::{JsonFileStore, SessionStore};
    use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};

    fn meta(actor_id: &str, actor_role: ActorRole) -> MetaField {
        MetaField {
            actor_id: actor_id.to_string(),
            actor_role,
        }
    }

    #[test]
    fn communication_tool_schemas_hide_bridge_meta() {
        fn assert_schema_hides_meta<T: schemars::JsonSchema>() {
            let schema = schemars::schema_for!(T);
            let json = serde_json::to_string(&schema).unwrap();
            assert!(
                !json.contains("_meta"),
                "tool input schema must not expose bridge-owned _meta: {json}"
            );
        }

        assert_schema_hides_meta::<MessageSendArgs>();
        assert_schema_hides_meta::<MessageListArgs>();
        assert_schema_hides_meta::<MessageReadArgs>();
        assert_schema_hides_meta::<MessageAckArgs>();
        assert_schema_hides_meta::<ArtifactPutArgs>();
        assert_schema_hides_meta::<ArtifactListArgs>();
        assert_schema_hides_meta::<ArtifactReadArgs>();
        assert_schema_hides_meta::<ArtifactGrantArgs>();
    }

    async fn test_state(config: CommunicationConfig) -> Arc<DispatchState> {
        let dir = tempfile::TempDir::new().unwrap();
        let lead = ResolvedLead {
            id: "lead".into(),
            directory: PathBuf::from("/tmp"),
            prompt: "lead prompt".into(),
            branch: None,
            model: "claude-haiku-4-5".into(),
            effort: Effort::High,
            tools: vec![],
            timeout_secs: 3600,
            use_worktree: false,
            env: Default::default(),
            resume_session_id: None,
            permission_routing: Default::default(),
            allow_subleads: true,
            max_subleads: Some(2),
            max_sublead_budget_usd: Some(1.0),
            max_total_workers: None,
            sublead_defaults: None,
        };
        let manifest = ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: Some(4),
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(lead),
            max_workers: Some(4),
            budget_usd: Some(5.0),
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
            communication: config,
            lifecycle: None,
            worker_types: vec![],
            sublead_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: Default::default(),
            agent_profiles: ::std::collections::HashMap::new(),
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let run_id = Uuid::now_v7();
        let spawner: Arc<dyn ProcessSpawner> =
            Arc::new(FakeSpawner::new(FakeScript::new().hold_until_signal()));
        let run_subdir = dir.path().join(run_id.to_string());
        std::mem::forget(dir);
        Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("claude"),
            Arc::new(WorktreeManager::new()),
            CleanupPolicy::Never,
            run_subdir,
            ApprovalPolicy::Block,
            None,
            Arc::new(crate::shared_store::SharedStore::new()),
        ))
    }

    async fn add_root_worker(state: &Arc<DispatchState>, id: &str) {
        state
            .root
            .workers
            .write()
            .await
            .insert(id.to_string(), WorkerState::Pending);
        state
            .worker_layer_index
            .write()
            .await
            .insert(id.to_string(), None);
    }

    async fn add_sublead_with_worker(state: &Arc<DispatchState>, sublead: &str, worker: &str) {
        let layer = Arc::new(LayerState::new(
            state.root.run_id,
            state.root.manifest.clone(),
            state.root.store.clone(),
            CancelToken::new(),
            sublead.to_string(),
            state.root.spawner.clone(),
            PathBuf::from("claude"),
            Arc::new(WorktreeManager::new()),
            CleanupPolicy::Never,
            state.root.run_subdir.join(sublead),
            ApprovalPolicy::Block,
            None,
            Arc::new(crate::shared_store::SharedStore::new()),
            Some(1.0),
        ));
        layer
            .workers
            .write()
            .await
            .insert(worker.to_string(), WorkerState::Pending);
        state.register_sublead(sublead.to_string(), layer).await;
        state
            .worker_layer_index
            .write()
            .await
            .insert(worker.to_string(), Some(sublead.to_string()));
    }

    #[tokio::test]
    async fn worker_can_message_parent_but_not_sibling() {
        let state = test_state(CommunicationConfig {
            mode: CommunicationMode::ParentChild,
            ..Default::default()
        })
        .await;
        add_root_worker(&state, "w1").await;
        add_root_worker(&state, "w2").await;

        let sent = handle_message_send(
            &state,
            MessageSendArgs {
                to: "parent".into(),
                subject: "done".into(),
                body: "ready".into(),
                refs: vec![],
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .unwrap();
        assert!(!sent.message_id.is_empty());

        let inbox = handle_message_list(
            &state,
            MessageListArgs {
                scope: Some("inbox".into()),
                meta: meta("lead", ActorRole::Lead),
            },
        )
        .await
        .unwrap();
        assert_eq!(inbox.messages.len(), 1);
        assert_eq!(inbox.messages[0].from, "w1");

        let denied = handle_message_send(
            &state,
            MessageSendArgs {
                to: "w2".into(),
                subject: "side".into(),
                body: "nope".into(),
                refs: vec![],
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(denied, CommunicationError::Forbidden(_)));
    }

    #[tokio::test]
    async fn artifact_grant_enables_sibling_read_without_kv_payloads() {
        let state = test_state(CommunicationConfig {
            mode: CommunicationMode::ParentChild,
            ..Default::default()
        })
        .await;
        add_root_worker(&state, "w1").await;
        add_root_worker(&state, "w2").await;

        let put = handle_artifact_put(
            &state,
            ArtifactPutArgs {
                name: "report.bin".into(),
                mime_type: Some("application/octet-stream".into()),
                content_base64: base64::engine::general_purpose::STANDARD.encode([0, 1, 2, 3]),
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .unwrap();

        let denied = handle_artifact_read(
            &state,
            ArtifactReadArgs {
                artifact_id: put.artifact_id.clone(),
                meta: meta("w2", ActorRole::Worker),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(denied, CommunicationError::Forbidden(_)));

        handle_artifact_grant(
            &state,
            ArtifactGrantArgs {
                artifact_id: put.artifact_id.clone(),
                to: "w2".into(),
                meta: meta("lead", ActorRole::Lead),
            },
        )
        .await
        .unwrap();

        let read = handle_artifact_read(
            &state,
            ArtifactReadArgs {
                artifact_id: put.artifact_id,
                meta: meta("w2", ActorRole::Worker),
            },
        )
        .await
        .unwrap();
        assert_eq!(read.artifact.name, "report.bin");
        assert_eq!(read.content_base64, "AAECAw==");
    }

    #[tokio::test]
    async fn activity_counters_track_message_and_artifact_attempts() {
        let state = test_state(CommunicationConfig {
            mode: CommunicationMode::ParentChild,
            ..Default::default()
        })
        .await;
        add_root_worker(&state, "w1").await;
        add_root_worker(&state, "w2").await;

        handle_message_send(
            &state,
            MessageSendArgs {
                to: "parent".into(),
                subject: "done".into(),
                body: "ready".into(),
                refs: vec![],
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .unwrap();

        let put = handle_artifact_put(
            &state,
            ArtifactPutArgs {
                name: "report.bin".into(),
                mime_type: Some("application/octet-stream".into()),
                content_base64: base64::engine::general_purpose::STANDARD.encode([0, 1, 2, 3]),
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .unwrap();

        let _ = handle_artifact_read(
            &state,
            ArtifactReadArgs {
                artifact_id: put.artifact_id,
                meta: meta("w2", ActorRole::Worker),
            },
        )
        .await
        .unwrap_err();

        let snapshot = state.communication.activity_snapshot().await;
        assert_eq!(snapshot["w1"].message_ops, 1);
        assert_eq!(snapshot["w1"].artifact_ops, 1);
        assert_eq!(snapshot["w2"].message_ops, 0);
        assert_eq!(snapshot["w2"].artifact_ops, 1);
    }

    #[tokio::test]
    async fn sublead_can_message_own_worker_and_root_only() {
        let state = test_state(CommunicationConfig {
            mode: CommunicationMode::ParentChild,
            ..Default::default()
        })
        .await;
        add_root_worker(&state, "root-worker").await;
        add_sublead_with_worker(&state, "s1", "sw1").await;

        handle_message_send(
            &state,
            MessageSendArgs {
                to: "parent".into(),
                subject: "up".into(),
                body: "status".into(),
                refs: vec![],
                meta: meta("sw1", ActorRole::Worker),
            },
        )
        .await
        .unwrap();

        let sublead_inbox = handle_message_list(
            &state,
            MessageListArgs {
                scope: Some("inbox".into()),
                meta: meta("s1", ActorRole::Sublead),
            },
        )
        .await
        .unwrap();
        assert_eq!(sublead_inbox.messages.len(), 1);
        assert_eq!(sublead_inbox.messages[0].from, "sw1");

        handle_message_send(
            &state,
            MessageSendArgs {
                to: "root".into(),
                subject: "up".into(),
                body: "sublead status".into(),
                refs: vec![],
                meta: meta("s1", ActorRole::Sublead),
            },
        )
        .await
        .unwrap();

        let denied = handle_message_send(
            &state,
            MessageSendArgs {
                to: "root-worker".into(),
                subject: "cross".into(),
                body: "nope".into(),
                refs: vec![],
                meta: meta("s1", ActorRole::Sublead),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(denied, CommunicationError::Forbidden(_)));
    }

    #[tokio::test]
    async fn disabled_mode_rejects_communication_tools() {
        let config = CommunicationConfig {
            mode: CommunicationMode::Disabled,
            ..Default::default()
        };
        let state = test_state(config).await;
        add_root_worker(&state, "w1").await;

        let err = handle_message_send(
            &state,
            MessageSendArgs {
                to: "parent".into(),
                subject: "blocked".into(),
                body: "blocked".into(),
                refs: vec![],
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, CommunicationError::Disabled));
    }

    /// Boundary: a message body exactly at `max_message_bytes` is accepted;
    /// one byte more is rejected. Off-by-one regression guard for the
    /// length check in [`handle_message_send`].
    #[tokio::test]
    async fn message_send_size_boundary() {
        let config = CommunicationConfig {
            mode: CommunicationMode::ParentChild,
            max_message_bytes: 16,
            ..Default::default()
        };
        let state = test_state(config).await;
        add_root_worker(&state, "w1").await;

        // Exactly the limit: accepted.
        handle_message_send(
            &state,
            MessageSendArgs {
                to: "parent".into(),
                subject: "ok".into(),
                body: "x".repeat(16),
                refs: vec![],
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .expect("exact-limit body must be accepted");

        // One byte over: rejected.
        let err = handle_message_send(
            &state,
            MessageSendArgs {
                to: "parent".into(),
                subject: "too-big".into(),
                body: "x".repeat(17),
                refs: vec![],
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err,
                CommunicationError::MessageTooLarge {
                    actual: 17,
                    limit: 16
                }
            ),
            "expected MessageTooLarge(actual=17, limit=16), got {err:?}",
        );
    }

    /// Boundary: an artifact payload exactly at `max_artifact_bytes` is
    /// accepted; one byte more is rejected. The check runs after base64
    /// decoding, so the comparison is against the decoded byte count.
    #[tokio::test]
    async fn artifact_put_size_boundary() {
        use base64::Engine;
        let config = CommunicationConfig {
            mode: CommunicationMode::ParentChild,
            max_artifact_bytes: 32,
            ..Default::default()
        };
        let state = test_state(config).await;
        add_root_worker(&state, "w1").await;

        let exact = base64::engine::general_purpose::STANDARD.encode(vec![0u8; 32]);
        handle_artifact_put(
            &state,
            ArtifactPutArgs {
                name: "exact.bin".into(),
                mime_type: None,
                content_base64: exact,
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .expect("exact-limit decoded payload must be accepted");

        let over = base64::engine::general_purpose::STANDARD.encode(vec![0u8; 33]);
        let err = handle_artifact_put(
            &state,
            ArtifactPutArgs {
                name: "too-big.bin".into(),
                mime_type: None,
                content_base64: over,
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err,
                CommunicationError::ArtifactTooLarge {
                    actual: 33,
                    limit: 32
                }
            ),
            "expected ArtifactTooLarge(actual=33, limit=32), got {err:?}",
        );
    }

    /// `artifact_put` writes the decoded payload to disk under
    /// `<run_dir>/communication/artifacts/<artifact_id>`. Container-dispatch
    /// auto-mounts `run_dir`, so this layout makes artifacts visible inside
    /// the container without extra plumbing. `pitboss prune --remove`
    /// reclaims them via recursive `remove_dir_all`.
    #[tokio::test]
    async fn artifact_put_writes_under_run_subdir() {
        let state = test_state(CommunicationConfig {
            mode: CommunicationMode::ParentChild,
            ..Default::default()
        })
        .await;
        add_root_worker(&state, "w1").await;

        let payload = vec![1u8, 2, 3, 4, 5];
        let put = handle_artifact_put(
            &state,
            ArtifactPutArgs {
                name: "blob.bin".into(),
                mime_type: None,
                content_base64: base64::engine::general_purpose::STANDARD.encode(&payload),
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .unwrap();

        let expected_path = state
            .root
            .run_subdir
            .join("communication")
            .join("artifacts")
            .join(&put.artifact_id);
        assert!(
            expected_path.exists(),
            "artifact bytes should land at {expected_path:?}",
        );
        let on_disk = std::fs::read(&expected_path).expect("read artifact");
        assert_eq!(
            on_disk, payload,
            "on-disk bytes must round-trip the payload"
        );
    }

    /// Regression for the lock-narrowing refactor in `handle_artifact_put`:
    /// filesystem I/O happens before the per-actor count is re-checked, so an
    /// over-limit put writes bytes to disk and must roll the file back when
    /// `ArtifactCountExceeded` is returned. Otherwise repeated rejected puts
    /// would accumulate orphan bytes under `<run_dir>/communication/artifacts/`.
    #[tokio::test]
    async fn artifact_put_unlinks_orphan_on_count_overflow() {
        use base64::Engine;
        let config = CommunicationConfig {
            mode: CommunicationMode::ParentChild,
            max_artifacts_per_actor: 1,
            ..Default::default()
        };
        let state = test_state(config).await;
        add_root_worker(&state, "w1").await;

        let payload = base64::engine::general_purpose::STANDARD.encode(b"hello");
        handle_artifact_put(
            &state,
            ArtifactPutArgs {
                name: "first.bin".into(),
                mime_type: None,
                content_base64: payload.clone(),
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .expect("first put under the limit must succeed");

        let artifacts_dir = state
            .root
            .run_subdir
            .join("communication")
            .join("artifacts");
        let after_first = std::fs::read_dir(&artifacts_dir)
            .expect("artifact dir should exist after first put")
            .count();
        assert_eq!(after_first, 1, "first put should leave exactly one file");

        let err = handle_artifact_put(
            &state,
            ArtifactPutArgs {
                name: "second.bin".into(),
                mime_type: None,
                content_base64: payload,
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, CommunicationError::ArtifactCountExceeded { .. }),
            "expected ArtifactCountExceeded, got {err:?}",
        );

        let after_second = std::fs::read_dir(&artifacts_dir)
            .expect("artifact dir should still exist after rejected put")
            .count();
        assert_eq!(
            after_second, 1,
            "rejected put must roll back the orphan file (saw {after_second} files)",
        );
    }

    // ----------------------------------------------------------------
    // #282: messages.jsonl journal — persist + replay on resume.
    // ----------------------------------------------------------------

    /// Send three messages from a worker to its parent, drop the
    /// `CommunicationStore`, construct a fresh one at the same
    /// `run_subdir`, and assert that all three reappear in the
    /// rebuilt mailbox (with parent ↔ child wiring intact).
    #[tokio::test]
    async fn journal_round_trip_rehydrates_messages() {
        let state = test_state(CommunicationConfig {
            mode: CommunicationMode::ParentChild,
            ..Default::default()
        })
        .await;
        add_root_worker(&state, "w1").await;

        for i in 0..3 {
            handle_message_send(
                &state,
                MessageSendArgs {
                    to: "parent".into(),
                    subject: format!("subject-{i}"),
                    body: format!("body-{i}"),
                    refs: vec![],
                    meta: meta("w1", ActorRole::Worker),
                },
            )
            .await
            .unwrap();
        }

        // The journal exists on disk with one line per send.
        let journal_path = state.communication.journal_path().to_path_buf();
        let on_disk = std::fs::read_to_string(&journal_path).unwrap();
        assert_eq!(
            on_disk.lines().count(),
            3,
            "journal should have one line per message_send",
        );

        // Replay into a fresh store and confirm the messages reappear.
        let fresh = CommunicationStore::new(
            state.root.manifest.communication.clone(),
            state.root.run_subdir.clone(),
        );
        let rebuilt = fresh.messages.read().await;
        assert_eq!(rebuilt.len(), 3);
        let mut subjects: Vec<String> = rebuilt.values().map(|m| m.subject.clone()).collect();
        subjects.sort();
        assert_eq!(subjects, vec!["subject-0", "subject-1", "subject-2"]);
        for msg in rebuilt.values() {
            assert_eq!(msg.from, "w1");
            assert_eq!(msg.to, "lead");
        }
    }

    /// Put one artifact, grant it to a sibling, drop, recreate; the
    /// metadata + grant survive into the rebuilt store so a resumed
    /// dispatcher can serve `artifact_read` for the grantee without
    /// re-issuing the original grant call.
    #[tokio::test]
    async fn journal_round_trip_rehydrates_artifacts_and_grants() {
        let state = test_state(CommunicationConfig {
            mode: CommunicationMode::ParentChild,
            ..Default::default()
        })
        .await;
        add_root_worker(&state, "w1").await;
        add_root_worker(&state, "w2").await;

        let put = handle_artifact_put(
            &state,
            ArtifactPutArgs {
                name: "report.bin".into(),
                mime_type: Some("application/octet-stream".into()),
                content_base64: base64::engine::general_purpose::STANDARD.encode([1, 2, 3]),
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .unwrap();

        handle_artifact_grant(
            &state,
            ArtifactGrantArgs {
                artifact_id: put.artifact_id.clone(),
                to: "w2".into(),
                meta: meta("lead", ActorRole::Lead),
            },
        )
        .await
        .unwrap();

        let fresh = CommunicationStore::new(
            state.root.manifest.communication.clone(),
            state.root.run_subdir.clone(),
        );
        let rebuilt = fresh.artifacts.read().await;
        let artifact = rebuilt
            .get(&put.artifact_id)
            .expect("artifact must rehydrate");
        assert_eq!(artifact.metadata.owner, "w1");
        assert!(
            artifact.grants.contains("w2"),
            "grant to w2 must survive replay: grants={:?}",
            artifact.grants
        );
        // The artifact's on-disk path was reconstructed under the run
        // subdir's artifact_dir — not stored on the wire, but derived
        // from `artifact_id`.
        let expected_path = state
            .root
            .run_subdir
            .join("communication")
            .join("artifacts")
            .join(&put.artifact_id);
        assert_eq!(artifact.path, expected_path);
    }

    /// A torn trailing write — partial line, no newline — must not
    /// fail the replay. The store loads everything before the torn
    /// line and skips the bad row with a warn log. Mirrors
    /// `pitboss_core::store::summary::overlay_summary_jsonl`'s
    /// tolerant-tail behavior.
    #[tokio::test]
    async fn journal_tolerates_torn_trailing_line() {
        let dir = tempfile::TempDir::new().unwrap();
        let run_subdir = dir.path().to_path_buf();
        let comm_dir = run_subdir.join("communication");
        std::fs::create_dir_all(&comm_dir).unwrap();
        let journal = comm_dir.join("messages.jsonl");

        // Two valid records, then a torn partial line with no \n.
        let valid_rec = JournalRecord::MessageSend {
            message: Message {
                message_id: "m1".into(),
                from: "w1".into(),
                to: "lead".into(),
                subject: "s".into(),
                body: "b".into(),
                refs: vec![],
                created_at: Utc::now(),
                acked_by: vec![],
            },
        };
        let second_rec = JournalRecord::MessageAck {
            message_id: "m1".into(),
            by: "lead".into(),
        };
        let mut content = serde_json::to_string(&valid_rec).unwrap();
        content.push('\n');
        content.push_str(&serde_json::to_string(&second_rec).unwrap());
        content.push('\n');
        // Torn: missing the closing brace.
        content.push_str(r#"{"type":"message_send","message":{"message_id":"m2""#);
        std::fs::write(&journal, content).unwrap();

        let store = CommunicationStore::new(CommunicationConfig::default(), run_subdir);
        let rebuilt = store.messages.read().await;
        // Only m1 survives: it was sent + acked; m2's record was torn.
        assert_eq!(rebuilt.len(), 1);
        let m1 = rebuilt.get("m1").expect("m1 must be present");
        assert!(m1.acked_by.contains("lead"), "ack must be replayed");
    }

    /// `note_message_op` / `note_artifact_op` bump the activity
    /// counters BEFORE the in-memory write. The replay re-bumps from
    /// the journal so the post-resume snapshot matches what the
    /// pre-crash live snapshot would have shown.
    #[tokio::test]
    async fn journal_replay_rebuilds_activity_counters() {
        let state = test_state(CommunicationConfig {
            mode: CommunicationMode::ParentChild,
            ..Default::default()
        })
        .await;
        add_root_worker(&state, "w1").await;

        handle_message_send(
            &state,
            MessageSendArgs {
                to: "parent".into(),
                subject: "s".into(),
                body: "b".into(),
                refs: vec![],
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .unwrap();
        handle_artifact_put(
            &state,
            ArtifactPutArgs {
                name: "f.bin".into(),
                mime_type: None,
                content_base64: base64::engine::general_purpose::STANDARD.encode([0u8]),
                meta: meta("w1", ActorRole::Worker),
            },
        )
        .await
        .unwrap();

        let fresh = CommunicationStore::new(
            state.root.manifest.communication.clone(),
            state.root.run_subdir.clone(),
        );
        let activity = fresh.activity_snapshot().await;
        let w1 = activity.get("w1").expect("w1 counters must rehydrate");
        assert_eq!(w1.message_ops, 1, "one message_send → one message_ops");
        assert_eq!(w1.artifact_ops, 1, "one artifact_put → one artifact_ops");
    }
}
