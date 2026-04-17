//! `SQLite`-backed [`SessionStore`] implementation.
//!
//! Uses `rusqlite` with the `bundled` feature so no system libsqlite is
//! required — Cargo builds `SQLite` from source.
//!
//! # Schema versioning note
//! This implementation assumes a greenfield database (v0.2.1). A `schema_versions`
//! table would be needed for safe schema evolution in future versions.
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
        init_schema(&conn)?;
        migrate_parent_task_id(&conn)?;
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

fn init_schema(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS runs (
            run_id           TEXT PRIMARY KEY,
            manifest_path    TEXT NOT NULL,
            shire_version    TEXT NOT NULL,
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

/// Raw fields fetched from the `runs` table.
#[allow(clippy::type_complexity)]
fn fetch_run_row(
    guard: &rusqlite::Connection,
    run_id_str: &str,
) -> Result<
    Option<(
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        Option<i64>,
        Option<i64>,
        i64,
    )>,
    StoreError,
> {
    let mut stmt = guard
        .prepare(
            "SELECT manifest_path, shire_version, claude_version, \
                 started_at, ended_at, tasks_total, tasks_failed, was_interrupted \
             FROM runs WHERE run_id = ?1",
        )
        .map_err(|e| StoreError::Incomplete(format!("load_run prepare: {e}")))?;
    let mut rows = stmt
        .query_map(rusqlite::params![run_id_str], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })
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
                  claude_session_id, final_message_preview, parent_task_id \
             FROM task_records WHERE run_id = ?1 ORDER BY rowid",
        )
        .map_err(|e| StoreError::Incomplete(format!("task query prepare: {e}")))?;

    let rows = stmt
        .query_map(rusqlite::params![run_id_str], |row| {
            Ok((
                row.get::<_, String>(0)?,          // task_id
                row.get::<_, String>(1)?,          // status
                row.get::<_, Option<i32>>(2)?,     // exit_code
                row.get::<_, String>(3)?,          // started_at
                row.get::<_, String>(4)?,          // ended_at
                row.get::<_, i64>(5)?,             // duration_ms
                row.get::<_, Option<String>>(6)?,  // worktree_path
                row.get::<_, String>(7)?,          // log_path
                row.get::<_, Option<i64>>(8)?,     // token_input
                row.get::<_, Option<i64>>(9)?,     // token_output
                row.get::<_, Option<i64>>(10)?,    // token_cache_read
                row.get::<_, Option<i64>>(11)?,    // token_cache_creation
                row.get::<_, Option<String>>(12)?, // claude_session_id
                row.get::<_, Option<String>>(13)?, // final_message_preview
                row.get::<_, Option<String>>(14)?, // parent_task_id
            ))
        })
        .map_err(|e| StoreError::Incomplete(format!("task query: {e}")))?;

    let mut out = Vec::new();
    for row in rows {
        let (
            task_id,
            status_str,
            exit_code,
            ts_started,
            ts_ended,
            duration_ms,
            worktree_path_str,
            log_path_str,
            token_input,
            token_output,
            token_cache_read,
            token_cache_creation,
            claude_session_id,
            final_message_preview,
            parent_task_id,
        ) = row.map_err(|e| StoreError::Incomplete(format!("task row read: {e}")))?;

        out.push(TaskRecord {
            task_id,
            status: status_from_str(&status_str)?,
            exit_code,
            started_at: parse_ts(&ts_started)?,
            ended_at: parse_ts(&ts_ended)?,
            duration_ms,
            worktree_path: worktree_path_str.map(PathBuf::from),
            log_path: PathBuf::from(log_path_str),
            token_usage: TokenUsage {
                input: token_input.unwrap_or(0).try_into().unwrap_or(0),
                output: token_output.unwrap_or(0).try_into().unwrap_or(0),
                cache_read: token_cache_read.unwrap_or(0).try_into().unwrap_or(0),
                cache_creation: token_cache_creation.unwrap_or(0).try_into().unwrap_or(0),
            },
            claude_session_id,
            final_message_preview,
            parent_task_id,
        });
    }
    Ok(out)
}

/// Blocking implementation of `load_run`, called inside `spawn_blocking`.
fn load_run_blocking(guard: &rusqlite::Connection, run_id: Uuid) -> Result<RunSummary, StoreError> {
    let run_id_str = run_id.to_string();

    let (
        manifest_path_str,
        shire_version,
        claude_version,
        started_at_str,
        ended_at_opt,
        tasks_total_stored,
        tasks_failed_stored,
        was_interrupted_stored,
    ) = fetch_run_row(guard, &run_id_str)?.ok_or(StoreError::NotFound(run_id))?;

    let manifest_path = PathBuf::from(manifest_path_str);
    let started_at = parse_ts(&started_at_str)?;
    let tasks = fetch_task_records(guard, &run_id_str)?;

    // If ended_at is NULL the run was never finalised — treat as interrupted.
    let (ended_at, was_interrupted) = if let Some(ref ts) = ended_at_opt {
        (parse_ts(ts)?, was_interrupted_stored != 0)
    } else {
        let fallback = tasks.last().map_or_else(Utc::now, |t| t.ended_at);
        (fallback, true)
    };

    // Prefer stored aggregates when the run was finalised; recompute from
    // tasks when it was not (orphan / interrupted run).
    let tasks_failed = if ended_at_opt.is_some() {
        usize::try_from(tasks_failed_stored.unwrap_or(0)).unwrap_or(0)
    } else {
        tasks
            .iter()
            .filter(|t| !matches!(t.status, TaskStatus::Success))
            .count()
    };
    let tasks_total = if ended_at_opt.is_some() {
        usize::try_from(tasks_total_stored.unwrap_or(0)).unwrap_or(0)
    } else {
        tasks.len()
    };

    Ok(RunSummary {
        run_id,
        manifest_path,
        shire_version,
        claude_version,
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
        let shire_version = meta.shire_version.clone();
        let claude_version = meta.claude_version.clone();
        let started_at = meta.started_at.to_rfc3339();

        tokio::task::spawn_blocking(move || {
            let guard = conn.lock().unwrap();
            guard
                .execute(
                    "INSERT OR REPLACE INTO runs \
                     (run_id, manifest_path, shire_version, claude_version, started_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        run_id,
                        manifest_path,
                        shire_version,
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
                      claude_session_id, final_message_preview, parent_task_id) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
            manifest_path: dir.join("shire.toml"),
            shire_version: "0.1.0".into(),
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
            manifest_path: dir.path().join("shire.toml"),
            shire_version: "0.1.0".into(),
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
            shire_version: "0.3.0".into(),
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
    }
}
