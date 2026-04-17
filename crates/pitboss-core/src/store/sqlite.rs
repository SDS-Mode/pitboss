//! `SQLite`-backed [`SessionStore`] implementation.
//!
//! Uses `rusqlite` with the `bundled` feature so no system libsqlite is
//! required — Cargo builds `SQLite` from source.
//!
//! ## Schema evolution
//!
//! The initial schema was introduced in v0.2.1. Additive changes (new
//! nullable columns) use idempotent migrations inside `SqliteStore::new`
//! that check `pragma_table_info(...)` and `ALTER TABLE` only when the
//! column is absent. This keeps old DBs forward-compatible without a
//! dedicated `schema_versions` table. Non-additive changes (dropped
//! columns, type changes, non-null constraints on existing columns)
//! would require a migration versioning scheme.
//!
//! # Concurrency
//! `SQLite`'s default journal mode (DELETE) is used; WAL mode is not required for
//! v0.2.1. The `Connection` is wrapped in `Arc<Mutex<_>>` so the store is
//! `Send + Sync`. Concurrent writes are serialised at the mutex.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::error::StoreError;
use crate::parser::TokenUsage;

use super::record::{RunMeta, RunSummary, TaskRecord, TaskStatus};
use super::traits::SessionStore;

/// `SQLite`-backed session store.
///
/// Create with [`SqliteStore::new`]; the database file is created if absent and
/// the required tables are initialised idempotently via `CREATE TABLE IF NOT
/// EXISTS`.
pub struct SqliteStore {
    inner: Arc<Mutex<rusqlite::Connection>>,
}

impl SqliteStore {
    /// Open (or create) the `SQLite` database at `path` and run the schema
    /// migrations.  Safe to call repeatedly on the same path — all DDL uses
    /// `IF NOT EXISTS`.
    ///
    /// # Errors
    /// Returns [`StoreError::Incomplete`] if the database cannot be opened or
    /// the schema cannot be initialised.
    pub fn new(path: PathBuf) -> Result<Self, StoreError> {
        let conn = rusqlite::Connection::open(path)
            .map_err(|e| StoreError::Incomplete(format!("sqlite open: {e}")))?;
        // Migration order matters: rename the legacy `shire_version` column
        // BEFORE init_schema runs its CREATE TABLE IF NOT EXISTS, otherwise
        // the pragma check below would still see the old column name for an
        // existing DB. For a fresh DB init_schema creates the table with the
        // new column name and the rename is a no-op.
        migrate_rename_shire_version_to_pitboss_version(&conn)?;
        init_schema(&conn)?;
        migrate_parent_task_id(&conn)?;
        migrate_v04_event_counters(&conn)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }
}

/// Idempotent migration: if an older DB lacks `parent_task_id`, add it.
///
/// Uses `PRAGMA table_info(task_records)` to detect whether the column is
/// already present before issuing `ALTER TABLE ... ADD COLUMN`. This avoids
/// the error `SQLite` raises when attempting to add a column that already
/// exists, and keeps this safe to call on every open.
fn migrate_parent_task_id(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    let has_parent = {
        let mut stmt = conn
            .prepare(
                "SELECT 1 FROM pragma_table_info('task_records') \
                 WHERE name = 'parent_task_id'",
            )
            .map_err(|e| StoreError::Incomplete(format!("migrate pragma prepare: {e}")))?;
        stmt.exists([])
            .map_err(|e| StoreError::Incomplete(format!("migrate pragma exists: {e}")))?
    };
    if !has_parent {
        conn.execute(
            "ALTER TABLE task_records ADD COLUMN parent_task_id TEXT NULL",
            [],
        )
        .map_err(|e| StoreError::Incomplete(format!("migrate alter: {e}")))?;
    }
    Ok(())
}

/// Idempotent migration: add v0.4 counter columns to `task_records` if missing.
fn migrate_v04_event_counters(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    for col in [
        "pause_count",
        "reprompt_count",
        "approvals_requested",
        "approvals_approved",
        "approvals_rejected",
    ] {
        let has = {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT 1 FROM pragma_table_info('task_records') WHERE name = '{col}'"
                ))
                .map_err(|e| StoreError::Incomplete(format!("migrate v04 prepare: {e}")))?;
            stmt.exists([])
                .map_err(|e| StoreError::Incomplete(format!("migrate v04 exists: {e}")))?
        };
        if !has {
            conn.execute(
                &format!("ALTER TABLE task_records ADD COLUMN {col} INTEGER NOT NULL DEFAULT 0"),
                [],
            )
            .map_err(|e| StoreError::Incomplete(format!("migrate v04 alter {col}: {e}")))?;
        }
    }
    Ok(())
}

/// Idempotent migration: pre-v0.3.0 DBs had a `shire_version` column on the
/// `runs` table. During the pitboss rebrand we renamed it to
/// `pitboss_version`. This check runs before `init_schema` so:
///   - fresh DBs skip it (the `runs` table doesn't exist yet)
///   - DBs with the old column get an `ALTER TABLE ... RENAME COLUMN`
///   - DBs with the new column (or no column at all) are a no-op
///
/// Requires `SQLite` >= 3.25 for `RENAME COLUMN` — rusqlite with the
/// `bundled` feature ships a modern build so this is fine.
fn migrate_rename_shire_version_to_pitboss_version(
    conn: &rusqlite::Connection,
) -> Result<(), StoreError> {
    // If the runs table doesn't exist yet (fresh DB), nothing to do.
    let runs_exists = {
        let mut stmt = conn
            .prepare(
                "SELECT 1 FROM sqlite_master \
                 WHERE type = 'table' AND name = 'runs'",
            )
            .map_err(|e| StoreError::Incomplete(format!("migrate check runs: {e}")))?;
        stmt.exists([])
            .map_err(|e| StoreError::Incomplete(format!("migrate check runs exists: {e}")))?
    };
    if !runs_exists {
        return Ok(());
    }

    let has_shire = {
        let mut stmt = conn
            .prepare(
                "SELECT 1 FROM pragma_table_info('runs') \
                 WHERE name = 'shire_version'",
            )
            .map_err(|e| StoreError::Incomplete(format!("migrate rename pragma prepare: {e}")))?;
        stmt.exists([])
            .map_err(|e| StoreError::Incomplete(format!("migrate rename pragma exists: {e}")))?
    };
    if has_shire {
        conn.execute(
            "ALTER TABLE runs RENAME COLUMN shire_version TO pitboss_version",
            [],
        )
        .map_err(|e| StoreError::Incomplete(format!("migrate rename alter: {e}")))?;
    }
    Ok(())
}

fn init_schema(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS runs (
            run_id           TEXT PRIMARY KEY,
            manifest_path    TEXT NOT NULL,
            pitboss_version  TEXT NOT NULL,
            claude_version   TEXT,
            started_at       TEXT NOT NULL,
            ended_at         TEXT,
            tasks_total      INTEGER,
            tasks_failed     INTEGER,
            was_interrupted  INTEGER DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS task_records (
            run_id                TEXT NOT NULL,
            task_id               TEXT NOT NULL,
            status                TEXT NOT NULL,
            exit_code             INTEGER,
            started_at            TEXT,
            ended_at              TEXT,
            duration_ms           INTEGER,
            worktree_path         TEXT,
            log_path              TEXT,
            token_input           INTEGER,
            token_output          INTEGER,
            token_cache_read      INTEGER,
            token_cache_creation  INTEGER,
            claude_session_id     TEXT,
            final_message_preview TEXT,
            parent_task_id        TEXT NULL,
            pause_count           INTEGER NOT NULL DEFAULT 0,
            reprompt_count        INTEGER NOT NULL DEFAULT 0,
            approvals_requested   INTEGER NOT NULL DEFAULT 0,
            approvals_approved    INTEGER NOT NULL DEFAULT 0,
            approvals_rejected    INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (run_id, task_id)
        );
        ",
    )
    .map_err(|e| StoreError::Incomplete(format!("sqlite schema: {e}")))
}

/// Serialize a `TaskStatus` to its string representation for storage.
fn status_to_str(s: &TaskStatus) -> &'static str {
    match s {
        TaskStatus::Success => "Success",
        TaskStatus::Failed => "Failed",
        TaskStatus::TimedOut => "TimedOut",
        TaskStatus::Cancelled => "Cancelled",
        TaskStatus::SpawnFailed => "SpawnFailed",
    }
}

/// Parse a `TaskStatus` from its stored string form.
fn status_from_str(s: &str) -> Result<TaskStatus, StoreError> {
    match s {
        "Success" => Ok(TaskStatus::Success),
        "Failed" => Ok(TaskStatus::Failed),
        "TimedOut" => Ok(TaskStatus::TimedOut),
        "Cancelled" => Ok(TaskStatus::Cancelled),
        "SpawnFailed" => Ok(TaskStatus::SpawnFailed),
        other => Err(StoreError::Incomplete(format!(
            "unknown task status: {other}"
        ))),
    }
}

/// Parse an RFC 3339 timestamp from a stored string.
fn parse_ts(s: &str) -> Result<DateTime<Utc>, StoreError> {
    s.parse::<DateTime<Utc>>()
        .map_err(|e| StoreError::Incomplete(format!("bad timestamp '{s}': {e}")))
}

// ---------------------------------------------------------------------------
// load_run helpers — extracted so the impl block stays under the line limit.
// ---------------------------------------------------------------------------

/// Strongly-typed row projection for the `runs` table. Confined to this module
/// so schema evolution lives in one place: add a column, extend this struct,
/// update `from_row`, done. Reads use column-name lookup (`row.get("name")`)
/// so column reordering in DDL is harmless.
struct RunRow {
    manifest_path: String,
    pitboss_version: String,
    claude_version: Option<String>,
    started_at: String,
    ended_at: Option<String>,
    tasks_total: Option<i64>,
    tasks_failed: Option<i64>,
    was_interrupted: i64,
}

impl RunRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            manifest_path: row.get("manifest_path")?,
            pitboss_version: row.get("pitboss_version")?,
            claude_version: row.get("claude_version")?,
            started_at: row.get("started_at")?,
            ended_at: row.get("ended_at")?,
            tasks_total: row.get("tasks_total")?,
            tasks_failed: row.get("tasks_failed")?,
            was_interrupted: row.get("was_interrupted")?,
        })
    }
}

/// Strongly-typed row projection for `task_records`. Same rationale as
/// `RunRow`: fields align with columns by name, so a SQL SELECT reorder is
/// harmless and a column rename turns into a compile-free runtime error at
/// exactly one call site — which is much easier to track than a silent
/// mismap across 15 positional getters.
struct TaskRow {
    task_id: String,
    status: String,
    exit_code: Option<i32>,
    started_at: String,
    ended_at: String,
    duration_ms: i64,
    worktree_path: Option<String>,
    log_path: String,
    token_input: Option<i64>,
    token_output: Option<i64>,
    token_cache_read: Option<i64>,
    token_cache_creation: Option<i64>,
    claude_session_id: Option<String>,
    final_message_preview: Option<String>,
    parent_task_id: Option<String>,
    pause_count: i64,
    reprompt_count: i64,
    approvals_requested: i64,
    approvals_approved: i64,
    approvals_rejected: i64,
}

impl TaskRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            task_id: row.get("task_id")?,
            status: row.get("status")?,
            exit_code: row.get("exit_code")?,
            started_at: row.get("started_at")?,
            ended_at: row.get("ended_at")?,
            duration_ms: row.get("duration_ms")?,
            worktree_path: row.get("worktree_path")?,
            log_path: row.get("log_path")?,
            token_input: row.get("token_input")?,
            token_output: row.get("token_output")?,
            token_cache_read: row.get("token_cache_read")?,
            token_cache_creation: row.get("token_cache_creation")?,
            claude_session_id: row.get("claude_session_id")?,
            final_message_preview: row.get("final_message_preview")?,
            parent_task_id: row.get("parent_task_id")?,
            pause_count: row.get("pause_count").unwrap_or(0),
            reprompt_count: row.get("reprompt_count").unwrap_or(0),
            approvals_requested: row.get("approvals_requested").unwrap_or(0),
            approvals_approved: row.get("approvals_approved").unwrap_or(0),
            approvals_rejected: row.get("approvals_rejected").unwrap_or(0),
        })
    }

    fn into_task_record(self) -> Result<TaskRecord, StoreError> {
        Ok(TaskRecord {
            task_id: self.task_id,
            status: status_from_str(&self.status)?,
            exit_code: self.exit_code,
            started_at: parse_ts(&self.started_at)?,
            ended_at: parse_ts(&self.ended_at)?,
            duration_ms: self.duration_ms,
            worktree_path: self.worktree_path.map(PathBuf::from),
            log_path: PathBuf::from(self.log_path),
            token_usage: TokenUsage {
                input: self.token_input.unwrap_or(0).try_into().unwrap_or(0),
                output: self.token_output.unwrap_or(0).try_into().unwrap_or(0),
                cache_read: self.token_cache_read.unwrap_or(0).try_into().unwrap_or(0),
                cache_creation: self
                    .token_cache_creation
                    .unwrap_or(0)
                    .try_into()
                    .unwrap_or(0),
            },
            claude_session_id: self.claude_session_id,
            final_message_preview: self.final_message_preview,
            parent_task_id: self.parent_task_id,
            pause_count: self.pause_count.try_into().unwrap_or(0),
            reprompt_count: self.reprompt_count.try_into().unwrap_or(0),
            approvals_requested: self.approvals_requested.try_into().unwrap_or(0),
            approvals_approved: self.approvals_approved.try_into().unwrap_or(0),
            approvals_rejected: self.approvals_rejected.try_into().unwrap_or(0),
        })
    }
}

/// Fetch the `runs` row for `run_id_str` as a typed projection.
fn fetch_run_row(
    guard: &rusqlite::Connection,
    run_id_str: &str,
) -> Result<Option<RunRow>, StoreError> {
    let mut stmt = guard
        .prepare(
            "SELECT manifest_path, pitboss_version, claude_version, \
                 started_at, ended_at, tasks_total, tasks_failed, was_interrupted \
             FROM runs WHERE run_id = ?1",
        )
        .map_err(|e| StoreError::Incomplete(format!("load_run prepare: {e}")))?;
    let mut rows = stmt
        .query_map(rusqlite::params![run_id_str], RunRow::from_row)
        .map_err(|e| StoreError::Incomplete(format!("load_run query: {e}")))?;
    rows.next()
        .transpose()
        .map_err(|e| StoreError::Incomplete(format!("load_run row: {e}")))
}

/// Fetch all task records for `run_id_str` in append order (by rowid).
fn fetch_task_records(
    guard: &rusqlite::Connection,
    run_id_str: &str,
) -> Result<Vec<TaskRecord>, StoreError> {
    let mut stmt = guard
        .prepare(
            "SELECT task_id, status, exit_code, \
                  started_at, ended_at, duration_ms, \
                  worktree_path, log_path, \
                  token_input, token_output, token_cache_read, token_cache_creation, \
                  claude_session_id, final_message_preview, parent_task_id, \
                  pause_count, reprompt_count, approvals_requested, \
                  approvals_approved, approvals_rejected \
             FROM task_records WHERE run_id = ?1 ORDER BY rowid",
        )
        .map_err(|e| StoreError::Incomplete(format!("task query prepare: {e}")))?;

    let rows = stmt
        .query_map(rusqlite::params![run_id_str], TaskRow::from_row)
        .map_err(|e| StoreError::Incomplete(format!("task query: {e}")))?;

    let mut out = Vec::new();
    for row in rows {
        let task_row = row.map_err(|e| StoreError::Incomplete(format!("task row read: {e}")))?;
        out.push(task_row.into_task_record()?);
    }
    Ok(out)
}

/// Blocking implementation of `load_run`, called inside `spawn_blocking`.
fn load_run_blocking(guard: &rusqlite::Connection, run_id: Uuid) -> Result<RunSummary, StoreError> {
    let run_id_str = run_id.to_string();

    let run = fetch_run_row(guard, &run_id_str)?.ok_or(StoreError::NotFound(run_id))?;

    let manifest_path = PathBuf::from(run.manifest_path);
    let started_at = parse_ts(&run.started_at)?;
    let tasks = fetch_task_records(guard, &run_id_str)?;

    // If ended_at is NULL the run was never finalised — treat as interrupted.
    let (ended_at, was_interrupted) = if let Some(ref ts) = run.ended_at {
        (parse_ts(ts)?, run.was_interrupted != 0)
    } else {
        let fallback = tasks.last().map_or_else(Utc::now, |t| t.ended_at);
        (fallback, true)
    };

    // Prefer stored aggregates when the run was finalised; recompute from
    // tasks when it was not (orphan / interrupted run).
    let tasks_failed = if run.ended_at.is_some() {
        usize::try_from(run.tasks_failed.unwrap_or(0)).unwrap_or(0)
    } else {
        tasks
            .iter()
            .filter(|t| !matches!(t.status, TaskStatus::Success))
            .count()
    };
    let tasks_total = if run.ended_at.is_some() {
        usize::try_from(run.tasks_total.unwrap_or(0)).unwrap_or(0)
    } else {
        tasks.len()
    };

    Ok(RunSummary {
        run_id,
        manifest_path,
        pitboss_version: run.pitboss_version,
        claude_version: run.claude_version,
        started_at,
        ended_at,
        total_duration_ms: (ended_at - started_at).num_milliseconds(),
        tasks_total,
        tasks_failed,
        was_interrupted,
        tasks,
    })
}

#[async_trait]
impl SessionStore for SqliteStore {
    /// Insert a run row.  Uses `INSERT OR REPLACE` so calling `init_run` twice
    /// on the same `run_id` is safe (idempotent).
    async fn init_run(&self, meta: &RunMeta) -> Result<(), StoreError> {
        let conn = Arc::clone(&self.inner);
        let run_id = meta.run_id.to_string();
        let manifest_path = meta.manifest_path.to_string_lossy().into_owned();
        let pitboss_version = meta.pitboss_version.clone();
        let claude_version = meta.claude_version.clone();
        let started_at = meta.started_at.to_rfc3339();

        tokio::task::spawn_blocking(move || {
            let guard = conn.lock().unwrap();
            guard
                .execute(
                    "INSERT OR REPLACE INTO runs \
                     (run_id, manifest_path, pitboss_version, claude_version, started_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        run_id,
                        manifest_path,
                        pitboss_version,
                        claude_version,
                        started_at
                    ],
                )
                .map_err(|e| StoreError::Incomplete(format!("init_run insert: {e}")))?;
            Ok::<(), StoreError>(())
        })
        .await
        .map_err(|e| StoreError::Incomplete(format!("join: {e}")))?
    }

    /// Upsert a task record.  `INSERT OR REPLACE` means a retry with the same
    /// `task_id` safely overwrites the previous entry.
    async fn append_record(&self, run_id: Uuid, record: &TaskRecord) -> Result<(), StoreError> {
        let conn = Arc::clone(&self.inner);
        let record = record.clone();
        let run_id_str = run_id.to_string();

        tokio::task::spawn_blocking(move || {
            let guard = conn.lock().unwrap();
            guard
                .execute(
                    "INSERT OR REPLACE INTO task_records \
                     (run_id, task_id, status, exit_code, \
                      started_at, ended_at, duration_ms, \
                      worktree_path, log_path, \
                      token_input, token_output, token_cache_read, token_cache_creation, \
                      claude_session_id, final_message_preview, parent_task_id, \
                      pause_count, reprompt_count, approvals_requested, \
                      approvals_approved, approvals_rejected) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
                             ?17, ?18, ?19, ?20, ?21)",
                    rusqlite::params![
                        run_id_str,
                        record.task_id,
                        status_to_str(&record.status),
                        record.exit_code,
                        record.started_at.to_rfc3339(),
                        record.ended_at.to_rfc3339(),
                        record.duration_ms,
                        record
                            .worktree_path
                            .as_deref()
                            .map(|p| p.to_string_lossy().into_owned()),
                        record.log_path.to_string_lossy().as_ref(),
                        i64::try_from(record.token_usage.input).unwrap_or(i64::MAX),
                        i64::try_from(record.token_usage.output).unwrap_or(i64::MAX),
                        i64::try_from(record.token_usage.cache_read).unwrap_or(i64::MAX),
                        i64::try_from(record.token_usage.cache_creation).unwrap_or(i64::MAX),
                        record.claude_session_id,
                        record.final_message_preview,
                        record.parent_task_id.as_deref(),
                        i64::from(record.pause_count),
                        i64::from(record.reprompt_count),
                        i64::from(record.approvals_requested),
                        i64::from(record.approvals_approved),
                        i64::from(record.approvals_rejected),
                    ],
                )
                .map_err(|e| StoreError::Incomplete(format!("append_record insert: {e}")))?;
            Ok::<(), StoreError>(())
        })
        .await
        .map_err(|e| StoreError::Incomplete(format!("join: {e}")))?
    }

    /// Update the run row with final aggregates.  Does NOT re-insert task
    /// records — those were written incrementally by [`append_record`].
    ///
    /// Returns [`StoreError::NotFound`] if no `init_run` was called first.
    async fn finalize_run(&self, summary: &RunSummary) -> Result<(), StoreError> {
        let conn = Arc::clone(&self.inner);
        let run_id = summary.run_id;
        let run_id_str = run_id.to_string();
        let ended_at = summary.ended_at.to_rfc3339();
        let tasks_total = i64::try_from(summary.tasks_total).unwrap_or(i64::MAX);
        let tasks_failed = i64::try_from(summary.tasks_failed).unwrap_or(i64::MAX);
        let was_interrupted = i64::from(summary.was_interrupted);

        tokio::task::spawn_blocking(move || {
            let guard = conn.lock().unwrap();
            let rows = guard
                .execute(
                    "UPDATE runs \
                     SET ended_at=?1, tasks_total=?2, tasks_failed=?3, was_interrupted=?4 \
                     WHERE run_id=?5",
                    rusqlite::params![
                        ended_at,
                        tasks_total,
                        tasks_failed,
                        was_interrupted,
                        run_id_str
                    ],
                )
                .map_err(|e| StoreError::Incomplete(format!("finalize_run update: {e}")))?;
            if rows == 0 {
                return Err(StoreError::NotFound(run_id));
            }
            Ok::<(), StoreError>(())
        })
        .await
        .map_err(|e| StoreError::Incomplete(format!("join: {e}")))?
    }

    /// Load a run by ID.  If the run's `ended_at` is `NULL` the run is treated
    /// as interrupted: `was_interrupted` is forced `true` and `ended_at` is
    /// taken from the last task's `ended_at`, falling back to `Utc::now()`.
    async fn load_run(&self, run_id: Uuid) -> Result<RunSummary, StoreError> {
        let conn = Arc::clone(&self.inner);

        tokio::task::spawn_blocking(move || {
            let guard = conn.lock().unwrap();
            load_run_blocking(&guard, run_id)
        })
        .await
        .map_err(|e| StoreError::Incomplete(format!("join: {e}")))?
    }
}

#[cfg(test)]
mod sqlite_tests {
    use super::*;
    use crate::store::record::TaskStatus;
    use chrono::Utc;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn make_store(dir: &TempDir) -> SqliteStore {
        SqliteStore::new(dir.path().join("test.db")).expect("SqliteStore::new")
    }

    fn meta(run_id: Uuid, dir: &Path) -> RunMeta {
        RunMeta {
            run_id,
            manifest_path: dir.join("pitboss.toml"),
            pitboss_version: "0.1.0".into(),
            claude_version: Some("1.0.0".into()),
            started_at: Utc::now(),
            env: HashMap::new(),
        }
    }

    fn rec(task_id: &str, status: TaskStatus) -> TaskRecord {
        let now = Utc::now();
        TaskRecord {
            task_id: task_id.into(),
            status,
            exit_code: Some(0),
            started_at: now,
            ended_at: now,
            duration_ms: 0,
            worktree_path: None,
            log_path: PathBuf::from("/dev/null"),
            token_usage: TokenUsage::default(),
            claude_session_id: None,
            final_message_preview: None,
            parent_task_id: None,
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
        }
    }

    /// Full round-trip: init → append two records → finalize → load.
    #[tokio::test]
    async fn sqlite_round_trip() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let run_id = Uuid::now_v7();

        store.init_run(&meta(run_id, dir.path())).await.unwrap();
        store
            .append_record(run_id, &rec("a", TaskStatus::Success))
            .await
            .unwrap();
        store
            .append_record(run_id, &rec("b", TaskStatus::Failed))
            .await
            .unwrap();

        let summary = RunSummary {
            run_id,
            manifest_path: dir.path().join("pitboss.toml"),
            pitboss_version: "0.1.0".into(),
            claude_version: None,
            started_at: Utc::now(),
            ended_at: Utc::now(),
            total_duration_ms: 0,
            tasks_total: 2,
            tasks_failed: 1,
            was_interrupted: false,
            tasks: vec![rec("a", TaskStatus::Success), rec("b", TaskStatus::Failed)],
        };
        store.finalize_run(&summary).await.unwrap();

        let back = store.load_run(run_id).await.unwrap();
        assert_eq!(back.tasks.len(), 2);
        assert_eq!(back.tasks_failed, 1);
        assert!(!back.was_interrupted);
        assert_eq!(back.tasks[0].task_id, "a");
        assert_eq!(back.tasks[1].task_id, "b");
    }

    /// Loading a `run_id` that was never initialised returns `StoreError::NotFound`.
    #[tokio::test]
    async fn sqlite_load_missing_run() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let missing = Uuid::now_v7();
        let err = store.load_run(missing).await.unwrap_err();
        assert!(matches!(err, StoreError::NotFound(_)));
    }

    /// A run that was never finalised should come back with `was_interrupted == true`.
    #[tokio::test]
    async fn sqlite_orphan_run_is_interrupted() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let run_id = Uuid::now_v7();

        store.init_run(&meta(run_id, dir.path())).await.unwrap();
        store
            .append_record(run_id, &rec("only", TaskStatus::Success))
            .await
            .unwrap();

        let loaded = store.load_run(run_id).await.unwrap();
        assert!(loaded.was_interrupted);
        assert_eq!(loaded.tasks.len(), 1);
    }

    /// Appending the same `task_id` twice should produce exactly one record
    /// because of the `PRIMARY KEY (run_id, task_id)` constraint with
    /// `INSERT OR REPLACE`.
    #[tokio::test]
    async fn sqlite_append_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let run_id = Uuid::now_v7();

        store.init_run(&meta(run_id, dir.path())).await.unwrap();
        store
            .append_record(run_id, &rec("dup", TaskStatus::Success))
            .await
            .unwrap();
        store
            .append_record(run_id, &rec("dup", TaskStatus::Failed))
            .await
            .unwrap();

        // Don't finalize — orphan load shows us the raw task_records.
        let loaded = store.load_run(run_id).await.unwrap();
        assert_eq!(
            loaded.tasks.len(),
            1,
            "duplicate task_id must collapse to one row"
        );
        // Second append (Failed) wins since it was an OR REPLACE.
        assert!(matches!(loaded.tasks[0].status, TaskStatus::Failed));
    }

    #[tokio::test]
    async fn sqlite_stores_parent_task_id() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("runs.db");
        let store = SqliteStore::new(db_path.clone()).unwrap();

        let run_id = Uuid::now_v7();
        store.init_run(&meta(run_id, dir.path())).await.unwrap();

        let mut rec = rec("worker-1", TaskStatus::Success);
        rec.parent_task_id = Some("lead-abc".to_string());
        store.append_record(run_id, &rec).await.unwrap();

        let summary = RunSummary {
            run_id,
            manifest_path: PathBuf::new(),
            pitboss_version: "0.3.0".into(),
            claude_version: None,
            started_at: Utc::now(),
            ended_at: Utc::now(),
            total_duration_ms: 0,
            tasks_total: 1,
            tasks_failed: 0,
            was_interrupted: false,
            tasks: vec![rec.clone()],
        };
        store.finalize_run(&summary).await.unwrap();

        let back = store.load_run(run_id).await.unwrap();
        assert_eq!(back.tasks[0].parent_task_id.as_deref(), Some("lead-abc"));
    }

    #[tokio::test]
    async fn sqlite_migrates_old_db_missing_parent_column() {
        // Covers both the initial migration (ALTER TABLE adds the column) and
        // the re-open idempotency guard (second open skips ALTER TABLE because
        // the column now exists).
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("old.db");

        // Create a DB manually without the parent_task_id column.
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE runs (run_id TEXT PRIMARY KEY, manifest_path TEXT NOT NULL, \
                 shire_version TEXT NOT NULL, claude_version TEXT, started_at TEXT NOT NULL, \
                 ended_at TEXT, tasks_total INTEGER, tasks_failed INTEGER, was_interrupted INTEGER DEFAULT 0); \
                 CREATE TABLE task_records (run_id TEXT NOT NULL, task_id TEXT NOT NULL, \
                 status TEXT NOT NULL, exit_code INTEGER, started_at TEXT, ended_at TEXT, \
                 duration_ms INTEGER, worktree_path TEXT, log_path TEXT, token_input INTEGER, \
                 token_output INTEGER, token_cache_read INTEGER, token_cache_creation INTEGER, \
                 claude_session_id TEXT, final_message_preview TEXT, \
                 PRIMARY KEY (run_id, task_id));",
            )
            .unwrap();
        }

        // Opening with the new store must add the missing column idempotently.
        let store = SqliteStore::new(db_path.clone()).unwrap();
        let run_id = Uuid::now_v7();
        store.init_run(&meta(run_id, dir.path())).await.unwrap();
        let mut rec = rec("t1", TaskStatus::Success);
        rec.parent_task_id = Some("parent-x".into());
        store.append_record(run_id, &rec).await.unwrap();
        // No panic, no error — column migration worked.

        // Re-open: must not attempt ALTER TABLE again (column now exists).
        let _store2 = SqliteStore::new(db_path.clone()).unwrap();
    }

    /// Covers the v0.3 pitboss rebrand migration: pre-existing DBs had a
    /// `shire_version` column on `runs`. Opening such a DB with the current
    /// `SqliteStore` must rename the column to `pitboss_version` idempotently.
    #[tokio::test]
    async fn sqlite_migrates_old_db_with_shire_version_column() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("legacy.db");

        // Build a pre-rebrand schema by hand (column named `shire_version`).
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE runs (run_id TEXT PRIMARY KEY, manifest_path TEXT NOT NULL, \
                 shire_version TEXT NOT NULL, claude_version TEXT, started_at TEXT NOT NULL, \
                 ended_at TEXT, tasks_total INTEGER, tasks_failed INTEGER, was_interrupted INTEGER DEFAULT 0); \
                 CREATE TABLE task_records (run_id TEXT NOT NULL, task_id TEXT NOT NULL, \
                 status TEXT NOT NULL, exit_code INTEGER, started_at TEXT, ended_at TEXT, \
                 duration_ms INTEGER, worktree_path TEXT, log_path TEXT, token_input INTEGER, \
                 token_output INTEGER, token_cache_read INTEGER, token_cache_creation INTEGER, \
                 claude_session_id TEXT, final_message_preview TEXT, parent_task_id TEXT NULL, \
                 PRIMARY KEY (run_id, task_id));",
            )
            .unwrap();
        }

        // Opening with the new store must rename the column.
        let store = SqliteStore::new(db_path.clone()).unwrap();

        // Writing and reading a record must round-trip through the renamed column.
        let run_id = Uuid::now_v7();
        store.init_run(&meta(run_id, dir.path())).await.unwrap();
        let rec = rec("t1", TaskStatus::Success);
        store.append_record(run_id, &rec).await.unwrap();

        let summary = RunSummary {
            run_id,
            manifest_path: dir.path().join("pitboss.toml"),
            pitboss_version: "0.1.0".into(),
            claude_version: None,
            started_at: Utc::now(),
            ended_at: Utc::now(),
            total_duration_ms: 0,
            tasks_total: 1,
            tasks_failed: 0,
            was_interrupted: false,
            tasks: vec![rec.clone()],
        };
        store.finalize_run(&summary).await.unwrap();
        let back = store.load_run(run_id).await.unwrap();
        // The version stored came from `meta()` via `init_run`; finalize_run
        // doesn't touch the column. What matters is that the read path
        // succeeds under the renamed column.
        assert_eq!(back.pitboss_version, "0.1.0");

        // Directly inspect pragma_table_info: the column must be named
        // `pitboss_version` now, and NOT `shire_version`.
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let has_pitboss = conn
            .prepare("SELECT 1 FROM pragma_table_info('runs') WHERE name = 'pitboss_version'")
            .unwrap()
            .exists([])
            .unwrap();
        let has_shire = conn
            .prepare("SELECT 1 FROM pragma_table_info('runs') WHERE name = 'shire_version'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(
            has_pitboss,
            "pitboss_version column must exist after migration"
        );
        assert!(
            !has_shire,
            "shire_version column must be gone after migration"
        );

        // Re-opening is idempotent — must not attempt another rename.
        let _store2 = SqliteStore::new(db_path).unwrap();
    }

    #[tokio::test]
    async fn sqlite_migrates_old_db_missing_counter_columns() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("v03.db");
        // Create a v0.3.3-shape DB by hand (no counter columns).
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE runs (run_id TEXT PRIMARY KEY, manifest_path TEXT NOT NULL, \
                 pitboss_version TEXT NOT NULL, claude_version TEXT, started_at TEXT NOT NULL, \
                 ended_at TEXT, tasks_total INTEGER, tasks_failed INTEGER, was_interrupted INTEGER DEFAULT 0); \
                 CREATE TABLE task_records (run_id TEXT NOT NULL, task_id TEXT NOT NULL, \
                 status TEXT NOT NULL, exit_code INTEGER, started_at TEXT, ended_at TEXT, \
                 duration_ms INTEGER, worktree_path TEXT, log_path TEXT, token_input INTEGER, \
                 token_output INTEGER, token_cache_read INTEGER, token_cache_creation INTEGER, \
                 claude_session_id TEXT, final_message_preview TEXT, parent_task_id TEXT NULL, \
                 PRIMARY KEY (run_id, task_id));",
            )
            .unwrap();
        }
        // Open with the new store → migration should add all 5 counter columns.
        let store = SqliteStore::new(db_path.clone()).unwrap();
        let run_id = Uuid::now_v7();
        store.init_run(&meta(run_id, dir.path())).await.unwrap();
        let mut rec = rec("t", TaskStatus::Success);
        rec.pause_count = 2;
        rec.approvals_approved = 1;
        store.append_record(run_id, &rec).await.unwrap();
        let back = store.load_run(run_id).await.unwrap();
        assert_eq!(back.tasks[0].pause_count, 2);
        assert_eq!(back.tasks[0].approvals_approved, 1);

        // Re-open: must not ALTER again.
        let _s2 = SqliteStore::new(db_path).unwrap();
    }
}
