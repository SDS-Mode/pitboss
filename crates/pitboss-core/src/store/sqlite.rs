//! `SQLite`-backed [`SessionStore`] implementation.
//!
//! Uses `rusqlite` with the `bundled` feature so no system libsqlite is
//! required — Cargo builds `SQLite` from source.
//!
//! ## Schema evolution
//!
//! The initial schema was introduced in v0.2.1. Migrations are declared
//! in the [`MIGRATIONS`] registry as `(version, name, apply)` triples
//! and applied in version order on `SqliteStore::new`. A
//! `schema_versions(version, name, applied_at)` table records which
//! migrations have run; on each open the runner skips versions already
//! present.
//!
//! Each migration body is still **idempotent** (column-presence /
//! object-existence checks before any DDL), so legacy DBs that
//! pre-date the version table back-fill cleanly on first open with
//! the new code: every migration runs, finds the change already
//! present, and records itself in `schema_versions`. From then on the
//! version table is the source of truth.
//!
//! Adding a migration: append a new entry to [`MIGRATIONS`] with the
//! next contiguous version number — never reorder or rewrite an
//! existing entry, since DBs in the field already record those
//! versions.
//!
//! # Concurrency
//! `SQLite`'s default journal mode (DELETE) is used; WAL mode is not required for
//! v0.2.1. The `Connection` is wrapped in `Arc<Mutex<_>>` so the store is
//! `Send + Sync`. Concurrent writes are serialised at the mutex.

use std::path::{Path, PathBuf};
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
        // Enable WAL so concurrent readers (e.g. `pitboss diff` opening the
        // same DB file while the dispatcher is writing) don't hit SQLITE_BUSY
        // from the default rollback journal. Pair with a generous busy_timeout
        // so the rare contended write retries rather than bailing.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| StoreError::Incomplete(format!("pragma journal_mode: {e}")))?;
        conn.pragma_update(None, "busy_timeout", 5000)
            .map_err(|e| StoreError::Incomplete(format!("pragma busy_timeout: {e}")))?;
        run_migrations(&conn)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    /// Highest migration version currently recorded in `schema_versions`,
    /// or `None` if the table is empty (which only happens on a DB that
    /// hasn't been opened by `SqliteStore::new` yet — fresh DBs always
    /// have at least one row by the time `new` returns).
    ///
    /// Exposed for diagnostics / tests; not used on the hot path.
    #[doc(hidden)]
    pub fn current_schema_version(&self) -> Result<Option<u32>, StoreError> {
        let conn = self
            .inner
            .lock()
            .map_err(|e| StoreError::Incomplete(format!("mutex poisoned: {e}")))?;
        let v: Option<u32> = conn
            .query_row("SELECT MAX(version) FROM schema_versions", [], |row| {
                row.get::<_, Option<u32>>(0)
            })
            .map_err(|e| StoreError::Incomplete(format!("read schema_versions: {e}")))?;
        Ok(v)
    }
}

/// Schema migration entry. Versions form a contiguous, append-only
/// sequence — never reorder or rewrite an existing entry once a build
/// has shipped, since DBs in the field already record those versions.
struct Migration {
    version: u32,
    name: &'static str,
    apply: fn(&rusqlite::Connection) -> Result<(), StoreError>,
}

/// Ordered registry of every schema migration, oldest first.
///
/// On `SqliteStore::new` the runner creates `schema_versions` (if
/// absent) and then walks this slice: for each entry whose version is
/// not yet recorded, runs `apply`, then records it. Each `apply` is
/// idempotent — running it on a DB where the change is already present
/// is a no-op — so legacy DBs (pre-versioning) back-fill the
/// `schema_versions` rows on the first open with this code.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "rename_shire_version_to_pitboss_version",
        apply: migrate_rename_shire_version_to_pitboss_version,
    },
    Migration {
        version: 2,
        name: "init_schema",
        apply: init_schema,
    },
    Migration {
        version: 3,
        name: "parent_task_id",
        apply: migrate_parent_task_id,
    },
    Migration {
        version: 4,
        name: "v04_event_counters",
        apply: migrate_v04_event_counters,
    },
    Migration {
        version: 5,
        name: "task_model",
        apply: migrate_task_model,
    },
    Migration {
        version: 6,
        name: "failure_reason",
        apply: migrate_failure_reason,
    },
    Migration {
        version: 7,
        name: "final_message",
        apply: migrate_final_message,
    },
    Migration {
        version: 8,
        name: "runs_env_json",
        apply: migrate_runs_env,
    },
];

/// Apply every entry in [`MIGRATIONS`] whose version is not yet
/// recorded in `schema_versions`. Records the version + name + UTC
/// applied-at timestamp on success. Migration order matters: the
/// `shire_version` rename has to run **before** `init_schema` so the
/// pragma check sees the old column name on a pre-rebrand DB; the
/// registry ordering bakes that in.
fn run_migrations(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_versions (
            version    INTEGER PRIMARY KEY,
            name       TEXT NOT NULL,
            applied_at TEXT NOT NULL
        );",
    )
    .map_err(|e| StoreError::Incomplete(format!("create schema_versions: {e}")))?;

    for m in MIGRATIONS {
        let already: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_versions WHERE version = ?1",
                rusqlite::params![m.version],
                |row| row.get(0),
            )
            .map_err(|e| {
                StoreError::Incomplete(format!("schema_versions check v{}: {e}", m.version))
            })?;
        if already > 0 {
            continue;
        }
        (m.apply)(conn)?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO schema_versions (version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![m.version, m.name, now],
        )
        .map_err(|e| {
            StoreError::Incomplete(format!("schema_versions insert v{}: {e}", m.version))
        })?;
    }
    Ok(())
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

/// Idempotent migration: add the v0.4.2 `model` column to `task_records`
/// if it's missing. Populated at spawn time so the Detail-view model
/// field doesn't need to rescan the log on every snapshot tick.
fn migrate_task_model(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    let has_model = {
        let mut stmt = conn
            .prepare(
                "SELECT 1 FROM pragma_table_info('task_records') \
                 WHERE name = 'model'",
            )
            .map_err(|e| StoreError::Incomplete(format!("migrate model pragma prepare: {e}")))?;
        stmt.exists([])
            .map_err(|e| StoreError::Incomplete(format!("migrate model pragma exists: {e}")))?
    };
    if !has_model {
        conn.execute("ALTER TABLE task_records ADD COLUMN model TEXT NULL", [])
            .map_err(|e| StoreError::Incomplete(format!("migrate model alter: {e}")))?;
    }
    Ok(())
}

/// Idempotent migration: add the v0.7.1 `failure_reason` column to
/// `task_records` if it's missing. Stores the `FailureReason` enum as JSON
/// (TEXT NULL) so adding/removing variants doesn't require another schema
/// migration. Populated by the post-exit failure-detection parser.
fn migrate_failure_reason(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    let has_col = {
        let mut stmt = conn
            .prepare(
                "SELECT 1 FROM pragma_table_info('task_records') \
                 WHERE name = 'failure_reason'",
            )
            .map_err(|e| StoreError::Incomplete(format!("migrate failure_reason prepare: {e}")))?;
        stmt.exists([])
            .map_err(|e| StoreError::Incomplete(format!("migrate failure_reason exists: {e}")))?
    };
    if !has_col {
        conn.execute(
            "ALTER TABLE task_records ADD COLUMN failure_reason TEXT NULL",
            [],
        )
        .map_err(|e| StoreError::Incomplete(format!("migrate failure_reason alter: {e}")))?;
    }
    Ok(())
}

/// Idempotent migration: add the v0.10 `final_message` column (untruncated
/// assistant final message — sibling to the existing 200-char preview). Lets
/// consumers read the complete text from `summary.json` / the `SQLite` store
/// without re-parsing per-task `stdout.log` for the terminal `result` event.
fn migrate_final_message(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    let has_col = {
        let mut stmt = conn
            .prepare(
                "SELECT 1 FROM pragma_table_info('task_records') \
                 WHERE name = 'final_message'",
            )
            .map_err(|e| StoreError::Incomplete(format!("migrate final_message prepare: {e}")))?;
        stmt.exists([])
            .map_err(|e| StoreError::Incomplete(format!("migrate final_message exists: {e}")))?
    };
    if !has_col {
        conn.execute(
            "ALTER TABLE task_records ADD COLUMN final_message TEXT NULL",
            [],
        )
        .map_err(|e| StoreError::Incomplete(format!("migrate final_message alter: {e}")))?;
    }
    Ok(())
}

/// Idempotent migration: add v0.4 counter columns to `task_records` if missing.
///
/// Per-column work is delegated to [`add_column_if_missing`] which:
///   * uses bound parameters for the existence check (no string
///     interpolation of the column name into the SQL — the parameterised
///     `pragma_table_info` selector is a load-bearing #149 L10 fix to
///     remove the only DDL footgun in this file), and
///   * still has to format the `ADD COLUMN` itself because `SQLite` does
///     not parameterise DDL — so we vet the column name with
///     [`assert_safe_ident`] before inlining.
fn migrate_v04_event_counters(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    for col in [
        "pause_count",
        "reprompt_count",
        "approvals_requested",
        "approvals_approved",
        "approvals_rejected",
    ] {
        add_column_if_missing(conn, "task_records", col, "INTEGER NOT NULL DEFAULT 0")?;
    }
    Ok(())
}

/// Add `column` of `type_clause` to `table` if missing. Single source of
/// truth for the additive-migration pattern used by `migrate_parent_task_id`,
/// `migrate_task_model`, etc. — extracted here so `migrate_v04_event_counters`
/// can iterate over a list of columns without each callsite re-implementing
/// the pragma check + ALTER pattern. (#149 L10)
///
/// Existence check uses a *bound* parameter for the column name; only the
/// `ADD COLUMN` statement needs the column inlined, and we run the column
/// through [`assert_safe_ident`] first (no whitespace, no quotes, no
/// semicolons) so a future caller who passes user input cannot smuggle DDL.
fn add_column_if_missing(
    conn: &rusqlite::Connection,
    table: &str,
    column: &str,
    type_clause: &str,
) -> Result<(), StoreError> {
    assert_safe_ident(table)?;
    assert_safe_ident(column)?;
    let has = {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
            ))
            .map_err(|e| {
                StoreError::Incomplete(format!("migrate {table}.{column} prepare: {e}"))
            })?;
        stmt.exists([column])
            .map_err(|e| StoreError::Incomplete(format!("migrate {table}.{column} exists: {e}")))?
    };
    if !has {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {type_clause}"),
            [],
        )
        .map_err(|e| StoreError::Incomplete(format!("migrate {table}.{column} alter: {e}")))?;
    }
    Ok(())
}

/// Reject anything that isn't a SQL-safe bare identifier — letters,
/// digits, and underscore only, must not start with a digit. Lets us
/// inline `column` and `table` names into `format!`-built DDL without
/// risking a future contributor passing a string with `; DROP TABLE …`.
/// All current callers pass static literals (`"task_records"`,
/// `"pause_count"`, …), so this is purely a defensive guard against
/// later refactors. (#149 L10)
fn assert_safe_ident(s: &str) -> Result<(), StoreError> {
    if s.is_empty() {
        return Err(StoreError::Incomplete("empty SQL identifier".into()));
    }
    let mut chars = s.chars();
    let first = chars.next().expect("non-empty checked above");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(StoreError::Incomplete(format!(
            "unsafe SQL identifier (leading char): {s:?}"
        )));
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return Err(StoreError::Incomplete(format!(
                "unsafe SQL identifier: {s:?}"
            )));
        }
    }
    Ok(())
}

/// Idempotent migration: add the `env_json` column to `runs` if missing.
/// Stores `RunMeta.env` as a JSON-encoded TEXT blob so the `SQLite` backend
/// has parity with the `JsonFileStore` backend (whose `meta.json` already
/// round-trips the field). Pre-fix, sqlite silently dropped the env on
/// `init_run` and re-materialised an empty `HashMap` on `load_run`,
/// hiding any operator-injected env vars when consumers later compared
/// runs across backends. (#149 M2)
fn migrate_runs_env(conn: &rusqlite::Connection) -> Result<(), StoreError> {
    let runs_exists = {
        let mut stmt = conn
            .prepare(
                "SELECT 1 FROM sqlite_master \
                 WHERE type = 'table' AND name = 'runs'",
            )
            .map_err(|e| StoreError::Incomplete(format!("migrate env check: {e}")))?;
        stmt.exists([])
            .map_err(|e| StoreError::Incomplete(format!("migrate env check exists: {e}")))?
    };
    if !runs_exists {
        return Ok(());
    }
    let has_col = {
        let mut stmt = conn
            .prepare(
                "SELECT 1 FROM pragma_table_info('runs') \
                 WHERE name = 'env_json'",
            )
            .map_err(|e| StoreError::Incomplete(format!("migrate env_json prepare: {e}")))?;
        stmt.exists([])
            .map_err(|e| StoreError::Incomplete(format!("migrate env_json exists: {e}")))?
    };
    if !has_col {
        conn.execute("ALTER TABLE runs ADD COLUMN env_json TEXT NULL", [])
            .map_err(|e| StoreError::Incomplete(format!("migrate env_json alter: {e}")))?;
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
            was_interrupted  INTEGER DEFAULT 0,
            env_json         TEXT NULL
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
            model                 TEXT NULL,
            failure_reason        TEXT NULL,
            final_message         TEXT NULL,
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
        TaskStatus::ApprovalRejected => "ApprovalRejected",
        TaskStatus::ApprovalTimedOut => "ApprovalTimedOut",
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
        "ApprovalRejected" => Ok(TaskStatus::ApprovalRejected),
        "ApprovalTimedOut" => Ok(TaskStatus::ApprovalTimedOut),
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
    model: Option<String>,
    failure_reason: Option<String>,
    final_message: Option<String>,
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
            model: row.get("model").unwrap_or(None),
            failure_reason: row.get("failure_reason").unwrap_or(None),
            final_message: row.get("final_message").unwrap_or(None),
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
            final_message: self.final_message,
            parent_task_id: self.parent_task_id,
            pause_count: self.pause_count.try_into().unwrap_or(0),
            reprompt_count: self.reprompt_count.try_into().unwrap_or(0),
            approvals_requested: self.approvals_requested.try_into().unwrap_or(0),
            approvals_approved: self.approvals_approved.try_into().unwrap_or(0),
            approvals_rejected: self.approvals_rejected.try_into().unwrap_or(0),
            model: self.model,
            failure_reason: self
                .failure_reason
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok()),
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
                  approvals_approved, approvals_rejected, model, failure_reason, \
                  final_message \
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
        // TODO: SQLite backend doesn't persist manifest_name yet — schema
        // has no column, finalize_run doesn't bind it, and this load path
        // hardcodes None. JsonFileStore (the production path) handles it
        // via serde. Add an idempotent ALTER TABLE migration matching the
        // existing migrate_failure_reason / migrate_parent_task_id pattern
        // when SqliteStore moves out of test-only use.
        manifest_name: None,
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
    fn open(path: &Path) -> Result<Box<dyn SessionStore>, StoreError> {
        Ok(Box::new(SqliteStore::new(path.to_path_buf())?))
    }

    /// Insert a run row.  Uses `INSERT OR REPLACE` so calling `init_run` twice
    /// on the same `run_id` is safe (idempotent).
    async fn init_run(&self, meta: &RunMeta) -> Result<(), StoreError> {
        let conn = Arc::clone(&self.inner);
        let run_id = meta.run_id.to_string();
        let manifest_path = meta.manifest_path.to_string_lossy().into_owned();
        let pitboss_version = meta.pitboss_version.clone();
        let claude_version = meta.claude_version.clone();
        let started_at = meta.started_at.to_rfc3339();
        // Persist env as JSON for parity with `JsonFileStore` (#149 M2).
        // Empty maps are stored as `NULL` rather than `"{}"` to keep the
        // common case (no operator env injection) cheap on disk.
        let env_json = if meta.env.is_empty() {
            None
        } else {
            Some(
                serde_json::to_string(&meta.env)
                    .map_err(|e| StoreError::Incomplete(format!("init_run env: {e}")))?,
            )
        };

        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .execute(
                    "INSERT OR REPLACE INTO runs \
                     (run_id, manifest_path, pitboss_version, claude_version, started_at, env_json) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        run_id,
                        manifest_path,
                        pitboss_version,
                        claude_version,
                        started_at,
                        env_json,
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
            let guard = conn.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            guard
                .execute(
                    "INSERT OR REPLACE INTO task_records \
                     (run_id, task_id, status, exit_code, \
                      started_at, ended_at, duration_ms, \
                      worktree_path, log_path, \
                      token_input, token_output, token_cache_read, token_cache_creation, \
                      claude_session_id, final_message_preview, parent_task_id, \
                      pause_count, reprompt_count, approvals_requested, \
                      approvals_approved, approvals_rejected, model, failure_reason, \
                      final_message) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
                             ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
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
                        record.model.as_deref(),
                        record
                            .failure_reason
                            .as_ref()
                            .and_then(|fr| serde_json::to_string(fr).ok()),
                        record.final_message,
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
            let guard = conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            let guard = conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            load_run_blocking(&guard, run_id)
        })
        .await
        .map_err(|e| StoreError::Incomplete(format!("join: {e}")))?
    }

    /// Enumerate runs as `RunMeta` records. Single `SELECT` against
    /// `runs` ordered by `started_at DESC` — never touches
    /// `task_records`, so the cost is bounded by run-count regardless
    /// of how big the per-run task lists are. (#149 L8)
    async fn iter_runs(&self) -> Result<Vec<RunMeta>, StoreError> {
        let conn = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            iter_runs_blocking(&guard)
        })
        .await
        .map_err(|e| StoreError::Incomplete(format!("join: {e}")))?
    }
}

/// Synchronous worker for `SqliteStore::iter_runs` — runs under
/// `spawn_blocking` since rusqlite is sync. Skips rows that fail to
/// parse (timestamp, `Uuid`, `env_json`) rather than aborting the
/// whole iteration: an operational console listing 200 runs shouldn't
/// 500 because one row has a malformed `env_json` blob.
fn iter_runs_blocking(guard: &rusqlite::Connection) -> Result<Vec<RunMeta>, StoreError> {
    let mut stmt = guard
        .prepare(
            "SELECT run_id, manifest_path, pitboss_version, claude_version, \
                    started_at, env_json \
             FROM runs ORDER BY started_at DESC",
        )
        .map_err(|e| StoreError::from_rusqlite(&e))?;
    let mapped = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })
        .map_err(|e| StoreError::from_rusqlite(&e))?;
    let mut metas: Vec<RunMeta> = Vec::new();
    for raw in mapped {
        let Ok((run_id_s, manifest_path, pitboss_version, claude_version, started_at, env_json)) =
            raw
        else {
            continue;
        };
        let Ok(run_id) = Uuid::parse_str(&run_id_s) else {
            continue;
        };
        let Ok(started_at) = parse_ts(&started_at) else {
            continue;
        };
        let env = match env_json.as_deref() {
            Some(s) => serde_json::from_str(s).unwrap_or_default(),
            None => std::collections::HashMap::new(),
        };
        metas.push(RunMeta {
            run_id,
            manifest_path: PathBuf::from(manifest_path),
            pitboss_version,
            claude_version,
            started_at,
            env,
        });
    }
    Ok(metas)
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
            final_message: None,
            parent_task_id: None,
            pause_count: 0,
            reprompt_count: 0,
            approvals_requested: 0,
            approvals_approved: 0,
            approvals_rejected: 0,
            model: None,
            failure_reason: None,
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
            manifest_name: None,
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
            manifest_name: None,
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
            manifest_name: None,
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

    #[tokio::test]
    async fn sqlite_migrates_old_db_missing_final_message_column() {
        // Pre-v0.10 schema has `final_message_preview` but not `final_message`.
        // Migration must add the column idempotently and round-trip a long
        // assistant message that would have been truncated under the preview cap.
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("pre_v010.db");
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
                 pause_count INTEGER NOT NULL DEFAULT 0, \
                 reprompt_count INTEGER NOT NULL DEFAULT 0, \
                 approvals_requested INTEGER NOT NULL DEFAULT 0, \
                 approvals_approved INTEGER NOT NULL DEFAULT 0, \
                 approvals_rejected INTEGER NOT NULL DEFAULT 0, \
                 model TEXT NULL, failure_reason TEXT NULL, \
                 PRIMARY KEY (run_id, task_id));",
            )
            .unwrap();
        }
        let store = SqliteStore::new(db_path.clone()).unwrap();
        let run_id = Uuid::now_v7();
        store.init_run(&meta(run_id, dir.path())).await.unwrap();
        let full = "x".repeat(1500);
        let mut rec = rec("t", TaskStatus::Success);
        rec.final_message_preview = Some(format!("{}…", &full[..200]));
        rec.final_message = Some(full.clone());
        store.append_record(run_id, &rec).await.unwrap();
        let back = store.load_run(run_id).await.unwrap();
        assert_eq!(back.tasks[0].final_message.as_deref(), Some(full.as_str()));
        // Re-open is a no-op (column now exists).
        let _s2 = SqliteStore::new(db_path).unwrap();
    }

    /// #149 M2 regression: `RunMeta.env` must round-trip through the
    /// `SQLite` backend (parity with `JsonFileStore`). We can't directly
    /// observe via `load_run` because `RunSummary` doesn't expose env —
    /// instead, look at the raw column.
    #[tokio::test]
    async fn sqlite_persists_run_meta_env() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("env.db");
        let store = SqliteStore::new(db_path.clone()).unwrap();
        let run_id = Uuid::now_v7();
        let mut env = HashMap::new();
        env.insert("PITBOSS_NOTIFY_SLACK_WEBHOOK".to_string(), "x".to_string());
        env.insert("PITBOSS_RUN_BUDGET_USD".to_string(), "10.50".to_string());
        let m = RunMeta {
            run_id,
            manifest_path: dir.path().join("p.toml"),
            pitboss_version: "0.9.1".into(),
            claude_version: None,
            started_at: Utc::now(),
            env,
        };
        store.init_run(&m).await.unwrap();

        // Open a fresh connection and read the raw env_json column to
        // assert the persistence is real (and survives a re-open).
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let env_json: Option<String> = conn
            .query_row(
                "SELECT env_json FROM runs WHERE run_id = ?1",
                rusqlite::params![run_id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        let env_json = env_json.expect("env_json must be persisted, not NULL");
        let parsed: HashMap<String, String> = serde_json::from_str(&env_json).unwrap();
        assert_eq!(
            parsed
                .get("PITBOSS_NOTIFY_SLACK_WEBHOOK")
                .map(String::as_str),
            Some("x")
        );
        assert_eq!(
            parsed.get("PITBOSS_RUN_BUDGET_USD").map(String::as_str),
            Some("10.50")
        );
    }

    /// #149 M2 regression: empty env stays NULL on disk, not `"{}"` —
    /// avoids a 2-byte-per-row tax on the common case.
    #[tokio::test]
    async fn sqlite_persists_empty_env_as_null() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("empty-env.db");
        let store = SqliteStore::new(db_path.clone()).unwrap();
        let run_id = Uuid::now_v7();
        store.init_run(&meta(run_id, dir.path())).await.unwrap();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let env_json: Option<String> = conn
            .query_row(
                "SELECT env_json FROM runs WHERE run_id = ?1",
                rusqlite::params![run_id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(env_json.is_none(), "empty env must persist as NULL");
    }

    /// #149 L10 regression: `assert_safe_ident` rejects anything that
    /// isn't a bare SQL identifier — this is the gate that lets
    /// `add_column_if_missing` interpolate column names into DDL safely.
    #[test]
    fn assert_safe_ident_accepts_bare_identifiers() {
        assert!(assert_safe_ident("task_records").is_ok());
        assert!(assert_safe_ident("env_json").is_ok());
        assert!(assert_safe_ident("_internal").is_ok());
        assert!(assert_safe_ident("col42").is_ok());
    }

    #[test]
    fn assert_safe_ident_rejects_dangerous_chars() {
        for bad in &[
            "",
            " spaces ",
            "drop;table",
            "with-dash",
            "with.dot",
            "with\"quote",
            "1leading_digit",
            "back`tick",
            "with'apos",
        ] {
            assert!(
                assert_safe_ident(bad).is_err(),
                "must reject {bad:?} as unsafe SQL ident"
            );
        }
    }

    /// #149 L11: a fresh DB opened by `SqliteStore::new` records every
    /// migration in `schema_versions` and the highest version equals
    /// the last entry in the registry.
    #[test]
    fn fresh_db_records_all_migration_versions() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);

        let conn = store.inner.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT version, name FROM schema_versions ORDER BY version")
            .unwrap();
        let rows: Vec<(u32, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        drop(stmt);
        drop(conn);

        let expected: Vec<(u32, String)> = MIGRATIONS
            .iter()
            .map(|m| (m.version, m.name.to_string()))
            .collect();
        assert_eq!(rows, expected, "every registered migration is recorded");

        let max = store.current_schema_version().unwrap();
        assert_eq!(max, MIGRATIONS.last().map(|m| m.version));
    }

    /// #149 L11: re-opening the same DB does not reapply or duplicate
    /// any migration — `applied_at` for each row is unchanged on
    /// second open.
    #[test]
    fn reopen_is_idempotent_and_preserves_applied_at() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");

        let first = SqliteStore::new(path.clone()).unwrap();
        let first_rows: Vec<(u32, String)> = {
            let conn = first.inner.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT version, applied_at FROM schema_versions ORDER BY version")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        drop(first);

        let second = SqliteStore::new(path).unwrap();
        let second_rows: Vec<(u32, String)> = {
            let conn = second.inner.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT version, applied_at FROM schema_versions ORDER BY version")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };

        assert_eq!(
            first_rows, second_rows,
            "applied_at timestamps must not change on re-open"
        );
        assert_eq!(first_rows.len(), MIGRATIONS.len());
    }

    /// #149 L11: a "legacy" DB that pre-dates the version table — i.e.
    /// has the post-migration schema but no `schema_versions` row —
    /// gets back-filled on the next `SqliteStore::new`. Simulated by
    /// running the migration apply functions directly on a raw
    /// connection (skipping the runner that records them) and then
    /// dropping/re-opening through `SqliteStore`.
    #[test]
    fn legacy_db_backfills_schema_versions() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");

        // Build a DB the "old way": apply migrations directly without
        // the schema_versions table.
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            for m in MIGRATIONS {
                (m.apply)(&conn).unwrap();
            }
            // Ensure schema_versions is genuinely absent so the
            // back-fill path is what we exercise next.
            let exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master \
                     WHERE type='table' AND name='schema_versions'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(exists, 0, "legacy fixture must not have schema_versions");
        }

        // Re-open through the production path — runner should
        // create schema_versions and record every migration as having
        // been applied.
        let store = SqliteStore::new(path).unwrap();
        let conn = store.inner.lock().unwrap();
        let count: usize = conn
            .query_row("SELECT COUNT(*) FROM schema_versions", [], |r| {
                r.get::<_, i64>(0).map(|n| usize::try_from(n).unwrap_or(0))
            })
            .unwrap();
        assert_eq!(
            count,
            MIGRATIONS.len(),
            "all migrations back-filled on first open"
        );
    }
    /// #149 L8: an empty `runs` table yields an empty inventory.
    #[tokio::test]
    async fn iter_runs_empty_db_returns_empty() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let metas = store.iter_runs().await.unwrap();
        assert!(metas.is_empty());
    }

    /// #149 L8: `SqliteStore` returns runs newest-first by `started_at`.
    /// Three runs initialised with deliberately-skewed timestamps so
    /// the ordering check is independent of insertion order.
    #[tokio::test]
    async fn iter_runs_orders_newest_first() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);

        let mid_id = Uuid::now_v7();
        let oldest_id = Uuid::now_v7();
        let newest_id = Uuid::now_v7();
        let now = Utc::now();

        let mut m_oldest = meta(oldest_id, dir.path());
        m_oldest.started_at = now - chrono::Duration::hours(2);
        let mut m_mid = meta(mid_id, dir.path());
        m_mid.started_at = now - chrono::Duration::minutes(30);
        let mut m_newest = meta(newest_id, dir.path());
        m_newest.started_at = now;

        // Insertion order intentionally does not match started_at.
        store.init_run(&m_mid).await.unwrap();
        store.init_run(&m_oldest).await.unwrap();
        store.init_run(&m_newest).await.unwrap();

        let metas = store.iter_runs().await.unwrap();
        let ids: Vec<Uuid> = metas.iter().map(|m| m.run_id).collect();
        assert_eq!(
            ids,
            vec![newest_id, mid_id, oldest_id],
            "iter_runs sorts newest-first by started_at"
        );
    }

    /// `iter_runs` does not touch the `task_records` table — verified
    /// by inserting a run with no tasks and checking the metadata
    /// round-trip while task counts stay at zero. The audit win
    /// (avoid materialising task lists during enumeration) lives or
    /// dies on this not pulling in tasks.
    #[tokio::test]
    async fn iter_runs_returns_metadata_only() {
        let dir = TempDir::new().unwrap();
        let store = make_store(&dir);
        let run_id = Uuid::now_v7();
        let mut m = meta(run_id, dir.path());
        m.env.insert("KEY".into(), "VALUE".into());
        store.init_run(&m).await.unwrap();
        // Append a task — iter_runs should still return the meta
        // without ever reading task_records.
        store
            .append_record(run_id, &rec("a", TaskStatus::Success))
            .await
            .unwrap();

        let metas = store.iter_runs().await.unwrap();
        assert_eq!(metas.len(), 1);
        let got = &metas[0];
        assert_eq!(got.run_id, run_id);
        assert_eq!(got.pitboss_version, "0.1.0");
        assert_eq!(got.env.get("KEY").map(String::as_str), Some("VALUE"));
    }
}
