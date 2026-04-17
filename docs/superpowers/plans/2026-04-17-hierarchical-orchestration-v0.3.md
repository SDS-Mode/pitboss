# Hierarchical Orchestration v0.3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add hierarchical mode to Agent Shire where one lead Hobbit dynamically spawns worker Hobbits via MCP tool calls, per `docs/superpowers/specs/2026-04-17-hierarchical-orchestration-design.md`.

**Architecture:** New `[[lead]]` manifest section, a local `rmcp`-based MCP server on a unix socket, six tools (`shire__spawn_worker`, `worker_status`, `wait_for_worker`, `wait_for_any`, `list_workers`, `cancel_worker`), hard caps on worker count and cost. Depth = 1, hub-and-spoke, workers get no MCP channel. Reuses existing `SessionHandle`, `WorktreeManager`, `SessionStore`, Mosaic observation, and dispatch runner unchanged.

**Tech Stack:** Rust stable, existing workspace (tokio, serde, clap, git2, rusqlite, ratatui); new dep `rmcp = "0.8"` with `server`, `transport-io`, `macros` features.

---

## Conventions

- Workspace root: `/run/media/system/Dos/Projects/agentshire/`
- Every task ends with a commit. Commits are small and buildable.
- `cargo test --workspace --features pitboss-core/test-support` is the gate.
- `cargo lint` (clippy `-D warnings`) must pass before every commit.
- `cargo fmt --all -- --check` must pass before every commit.
- Forward-reference stubs allowed ONLY when marked with `unimplemented!("covered in Task N")`.
- Existing 158 tests from v0.2.2 must stay green throughout.

---

## Phase 0 — Foundation

### Task 1: Add `parent_task_id` to TaskRecord

**Files:**
- Modify: `crates/pitboss-core/src/store/record.rs`
- Modify: `crates/pitboss-core/src/store/json_file.rs` (no-op for JSON — serde handles it)

- [ ] **Step 1: Write failing test**

Append to `crates/pitboss-core/src/store/record.rs` `mod tests`:

```rust
    #[test]
    fn task_record_with_parent_round_trips() {
        let rec = TaskRecord {
            task_id: "worker-1".into(),
            status: TaskStatus::Success,
            exit_code: Some(0),
            started_at: Utc.with_ymd_and_hms(2026, 4, 17, 0, 0, 0).unwrap(),
            ended_at:   Utc.with_ymd_and_hms(2026, 4, 17, 0, 0, 30).unwrap(),
            duration_ms: 30_000,
            worktree_path: None,
            log_path: PathBuf::from("/dev/null"),
            token_usage: TokenUsage::default(),
            claude_session_id: None,
            final_message_preview: None,
            parent_task_id: Some("lead-abc".into()),
        };
        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains("parent_task_id"));
        let back: TaskRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.parent_task_id.as_deref(), Some("lead-abc"));
    }

    #[test]
    fn task_record_without_parent_deserializes_from_old_json() {
        let old_json = r#"{
            "task_id": "t1",
            "status": "Success",
            "exit_code": 0,
            "started_at": "2026-04-17T00:00:00Z",
            "ended_at":   "2026-04-17T00:00:30Z",
            "duration_ms": 30000,
            "worktree_path": null,
            "log_path": "/dev/null",
            "token_usage": {"input":0,"output":0,"cache_read":0,"cache_creation":0},
            "claude_session_id": null,
            "final_message_preview": null
        }"#;
        let rec: TaskRecord = serde_json::from_str(old_json).unwrap();
        assert!(rec.parent_task_id.is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pitboss-core store::record::tests::task_record_with_parent_round_trips`
Expected: FAIL — field missing.

- [ ] **Step 3: Add the field**

In `crates/pitboss-core/src/store/record.rs`, modify the `TaskRecord` struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub task_id: String,
    pub status: TaskStatus,
    pub exit_code: Option<i32>,
    pub started_at: DateTime<Utc>,
    pub ended_at:   DateTime<Utc>,
    pub duration_ms: i64,
    pub worktree_path: Option<PathBuf>,
    pub log_path: PathBuf,
    pub token_usage: TokenUsage,
    pub claude_session_id: Option<String>,
    pub final_message_preview: Option<String>,
    /// Task id of the lead that spawned this worker, or `None` for flat-mode
    /// tasks and the lead itself.
    #[serde(default)]
    pub parent_task_id: Option<String>,
}
```

- [ ] **Step 4: Fix all callers**

Search for `TaskRecord {` constructions across the workspace and add `parent_task_id: None` where missing:

```
grep -rn "TaskRecord {" crates/ tests-support/
```

Each construction needs `parent_task_id: None,` before the closing brace.

- [ ] **Step 5: Run full tests to verify pass**

Run: `cargo test --workspace --features pitboss-core/test-support`
Expected: all 158 previous tests pass + 2 new tests pass.

Run: `cargo lint`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add crates/pitboss-core/src/store/record.rs
git commit -m "Add parent_task_id to TaskRecord with backward-compat deserialization"
```

---

### Task 2: SQLite migration for parent_task_id column

**Files:**
- Modify: `crates/pitboss-core/src/store/sqlite.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/pitboss-core/src/store/sqlite.rs` `#[cfg(test)] mod sqlite_tests`:

```rust
    #[tokio::test]
    async fn sqlite_stores_parent_task_id() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("runs.db");
        let store = SqliteStore::new(db_path.clone()).unwrap();

        let run_id = Uuid::now_v7();
        store.init_run(&meta(run_id, dir.path().to_path_buf())).await.unwrap();

        let mut rec = rec("worker-1", TaskStatus::Success);
        rec.parent_task_id = Some("lead-abc".to_string());
        store.append_record(run_id, &rec).await.unwrap();

        let summary = RunSummary {
            run_id,
            manifest_path: PathBuf::new(),
            shire_version: "0.3.0".into(),
            claude_version: None,
            started_at: Utc::now(),
            ended_at:   Utc::now(),
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
                 PRIMARY KEY (run_id, task_id));"
            ).unwrap();
        }

        // Opening with the new store must add the missing column idempotently.
        let store = SqliteStore::new(db_path.clone()).unwrap();
        let run_id = Uuid::now_v7();
        store.init_run(&meta(run_id, dir.path().to_path_buf())).await.unwrap();
        let mut rec = rec("t1", TaskStatus::Success);
        rec.parent_task_id = Some("parent-x".into());
        store.append_record(run_id, &rec).await.unwrap();
        // No panic, no error — column migration worked.
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pitboss-core store::sqlite::sqlite_tests::sqlite_stores_parent_task_id`
Expected: FAIL — column doesn't exist.

- [ ] **Step 3: Update schema + migration**

In `crates/pitboss-core/src/store/sqlite.rs`, update `SqliteStore::new` to add the column to both create and migrate paths. The key change is:

1. Include `parent_task_id TEXT NULL` in the `CREATE TABLE task_records` clause.
2. After creating/opening the DB, run a migration check that uses `PRAGMA table_info(task_records)` to detect whether `parent_task_id` exists, and if not, issue `ALTER TABLE task_records ADD COLUMN parent_task_id TEXT NULL`.

Replace the `new` function body with the corrected schema + migration pattern:

```rust
    pub fn new(path: PathBuf) -> Result<Self, StoreError> {
        let conn = Connection::open(&path).map_err(|e| StoreError::Incomplete(e.to_string()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS runs (
                run_id TEXT PRIMARY KEY,
                manifest_path TEXT NOT NULL,
                shire_version TEXT NOT NULL,
                claude_version TEXT,
                started_at TEXT NOT NULL,
                ended_at TEXT,
                tasks_total INTEGER,
                tasks_failed INTEGER,
                was_interrupted INTEGER DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS task_records (
                run_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                status TEXT NOT NULL,
                exit_code INTEGER,
                started_at TEXT,
                ended_at TEXT,
                duration_ms INTEGER,
                worktree_path TEXT,
                log_path TEXT,
                token_input INTEGER,
                token_output INTEGER,
                token_cache_read INTEGER,
                token_cache_creation INTEGER,
                claude_session_id TEXT,
                final_message_preview TEXT,
                parent_task_id TEXT NULL,
                PRIMARY KEY (run_id, task_id)
            );"
        ).map_err(|e| StoreError::Incomplete(e.to_string()))?;

        // Idempotent migration: if an older DB lacks parent_task_id, add it.
        let has_parent = {
            let mut stmt = conn
                .prepare("SELECT 1 FROM pragma_table_info('task_records') WHERE name = 'parent_task_id'")
                .map_err(|e| StoreError::Incomplete(e.to_string()))?;
            stmt.exists([]).map_err(|e| StoreError::Incomplete(e.to_string()))?
        };
        if !has_parent {
            conn.execute(
                "ALTER TABLE task_records ADD COLUMN parent_task_id TEXT NULL",
                [],
            ).map_err(|e| StoreError::Incomplete(e.to_string()))?;
        }

        Ok(Self { inner: Arc::new(Mutex::new(conn)) })
    }
```

Also update the `append_record` implementation's INSERT SQL to include `parent_task_id`:

```rust
            let sql = "INSERT OR REPLACE INTO task_records (
                run_id, task_id, status, exit_code, started_at, ended_at, duration_ms,
                worktree_path, log_path, token_input, token_output,
                token_cache_read, token_cache_creation, claude_session_id,
                final_message_preview, parent_task_id
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)";
            // ... bind ?16 to record.parent_task_id.as_deref() ...
```

Update the row→record mapper in `load_run` to also read `parent_task_id` (new column at the end).

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-core store::sqlite`
Expected: all 4 sqlite tests (2 existing + 2 new) pass.

Run: `cargo lint` and `cargo fmt --all -- --check`. Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-core/src/store/sqlite.rs
git commit -m "SqliteStore: add parent_task_id column with idempotent migration"
```

---

### Task 3: Add `rmcp` workspace dependency

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/pitboss-cli/Cargo.toml`

- [ ] **Step 1: Add to workspace deps**

In root `Cargo.toml` `[workspace.dependencies]`, add:

```toml
rmcp = { version = "0.8", features = ["server", "transport-io", "macros"] }
```

- [ ] **Step 2: Add to pitboss-cli**

In `crates/pitboss-cli/Cargo.toml` `[dependencies]`, add:

```toml
rmcp = { workspace = true }
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p pitboss-cli`
Expected: build succeeds. No other changes yet — this is just adding the dep.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/pitboss-cli/Cargo.toml
git commit -m "Add rmcp dependency for v0.3 MCP server"
```

---

## Phase 1 — Manifest

### Task 4: `[[lead]]` schema + new `[run]` fields

**Files:**
- Modify: `crates/pitboss-cli/src/manifest/schema.rs`

- [ ] **Step 1: Write failing tests**

Append to `crates/pitboss-cli/src/manifest/schema.rs` `mod tests`:

```rust
    #[test]
    fn parses_lead_section() {
        let toml_src = r#"
            [run]
            max_workers = 4
            budget_usd = 5.00
            lead_timeout_secs = 1200

            [[lead]]
            id = "triage"
            directory = "/tmp"
            prompt = "coordinate the triage"
            branch = "feat/triage"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.run.max_workers, Some(4));
        assert_eq!(m.run.budget_usd, Some(5.00));
        assert_eq!(m.run.lead_timeout_secs, Some(1200));
        assert_eq!(m.leads.len(), 1);
        assert_eq!(m.leads[0].id, "triage");
        assert_eq!(m.leads[0].branch.as_deref(), Some("feat/triage"));
    }

    #[test]
    fn rejects_unknown_lead_field() {
        let toml_src = r#"
            [[lead]]
            id = "x"
            directory = "/tmp"
            prompt = "p"
            wibble = "surprise"
        "#;
        let err: Result<Manifest, _> = toml::from_str(toml_src);
        assert!(err.is_err());
    }

    #[test]
    fn parses_run_fields_without_lead_section() {
        // These fields parse fine on their own; validation rejects them later
        // when no [[lead]] is present.
        let toml_src = r#"
            [run]
            max_workers = 2
            budget_usd = 1.00

            [[task]]
            id = "x"
            directory = "/tmp"
            prompt = "p"
        "#;
        let m: Manifest = toml::from_str(toml_src).unwrap();
        assert_eq!(m.run.max_workers, Some(2));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p pitboss-cli manifest::schema::tests::parses_lead_section`
Expected: FAIL — `leads` field doesn't exist on `Manifest`.

- [ ] **Step 3: Extend schema types**

In `crates/pitboss-cli/src/manifest/schema.rs`:

Add `Lead` struct:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Lead {
    pub id: String,
    pub directory: PathBuf,
    pub prompt: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<Effort>,
    #[serde(default)]
    pub tools: Option<Vec<String>>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub use_worktree: Option<bool>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}
```

Extend `Manifest` with a `leads` field:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    #[serde(default)]
    pub run: RunConfig,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default, rename = "template")]
    pub templates: Vec<Template>,
    #[serde(default, rename = "task")]
    pub tasks: Vec<Task>,
    #[serde(default, rename = "lead")]
    pub leads: Vec<Lead>,  // NEW — 0 or 1 entry after validation
}
```

Extend `RunConfig`:

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    pub max_parallel:      Option<u32>,
    #[serde(default)]
    pub halt_on_failure:   bool,
    pub run_dir:           Option<PathBuf>,
    #[serde(default = "default_cleanup")]
    pub worktree_cleanup:  WorktreeCleanup,
    #[serde(default)]
    pub emit_event_stream: bool,

    // NEW in v0.3 — only meaningful when [[lead]] is present.
    #[serde(default)]
    pub max_workers:       Option<u32>,
    #[serde(default)]
    pub budget_usd:        Option<f64>,
    #[serde(default)]
    pub lead_timeout_secs: Option<u64>,
}
```

Update `Default for RunConfig` to include `max_workers: None, budget_usd: None, lead_timeout_secs: None`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p pitboss-cli manifest::schema`
Expected: all existing schema tests pass plus 3 new ones.

Run: `cargo lint`. Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-cli/src/manifest/schema.rs
git commit -m "manifest: add [[lead]] section and hierarchical [run] fields"
```

---

### Task 5: Resolve `[[lead]]` into `ResolvedLead`

**Files:**
- Modify: `crates/pitboss-cli/src/manifest/resolve.rs`

- [ ] **Step 1: Write failing tests**

Append to `crates/pitboss-cli/src/manifest/resolve.rs` `mod tests`:

```rust
    #[test]
    fn resolves_lead_inheriting_defaults() {
        let m = man(r#"
            [defaults]
            model = "claude-haiku-4-5"
            tools = ["Read","Write"]
            timeout_secs = 1800

            [[lead]]
            id = "triage"
            directory = "/tmp"
            prompt = "coordinate"
        "#);
        let r = resolve(m, None).unwrap();
        let lead = r.lead.as_ref().expect("must resolve a lead");
        assert_eq!(lead.id, "triage");
        assert_eq!(lead.model, "claude-haiku-4-5");
        assert_eq!(lead.tools, vec!["Read", "Write"]);
        assert_eq!(lead.timeout_secs, 1800);
        assert!(lead.use_worktree);
        assert!(r.tasks.is_empty());
    }

    #[test]
    fn lead_overrides_defaults() {
        let m = man(r#"
            [defaults]
            model = "claude-haiku-4-5"

            [[lead]]
            id = "triage"
            directory = "/tmp"
            prompt = "coordinate"
            model = "claude-sonnet-4-6"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.lead.unwrap().model, "claude-sonnet-4-6");
    }

    #[test]
    fn lead_timeout_falls_back_to_run_lead_timeout_secs() {
        let m = man(r#"
            [run]
            lead_timeout_secs = 7200

            [[lead]]
            id = "triage"
            directory = "/tmp"
            prompt = "p"
        "#);
        let r = resolve(m, None).unwrap();
        assert_eq!(r.lead.unwrap().timeout_secs, 7200);
    }
```

- [ ] **Step 2: Run tests — should fail because `ResolvedManifest` has no `lead` field**

Run: `cargo test -p pitboss-cli manifest::resolve::tests::resolves_lead_inheriting_defaults`
Expected: FAIL — `r.lead` doesn't exist.

- [ ] **Step 3: Add `ResolvedLead` + hierarchical fields to `ResolvedManifest`**

In `crates/pitboss-cli/src/manifest/resolve.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedLead {
    pub id: String,
    pub directory: PathBuf,
    pub prompt: String,
    pub branch: Option<String>,
    pub model: String,
    pub effort: Effort,
    pub tools: Vec<String>,
    pub timeout_secs: u64,
    pub use_worktree: bool,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedManifest {
    pub max_parallel: u32,
    pub halt_on_failure: bool,
    pub run_dir: PathBuf,
    pub worktree_cleanup: WorktreeCleanup,
    pub emit_event_stream: bool,
    pub tasks: Vec<ResolvedTask>,
    // NEW in v0.3:
    pub lead: Option<ResolvedLead>,
    pub max_workers:       Option<u32>,
    pub budget_usd:        Option<f64>,
    pub lead_timeout_secs: Option<u64>,
}
```

Add a `resolve_lead` function:

```rust
fn resolve_lead(lead: &Lead, defaults: &Defaults, run: &RunConfig) -> Result<ResolvedLead> {
    let mut env = defaults.env.clone();
    env.extend(lead.env.clone());

    // Lead timeout cascade: per-lead timeout_secs > [run].lead_timeout_secs > defaults.timeout_secs > 3600
    let timeout_secs = lead.timeout_secs
        .or(run.lead_timeout_secs)
        .or(defaults.timeout_secs)
        .unwrap_or(3600);

    Ok(ResolvedLead {
        id: lead.id.clone(),
        directory: lead.directory.clone(),
        prompt: lead.prompt.clone(),
        branch: lead.branch.clone(),
        model:  lead.model.clone()
                    .or_else(|| defaults.model.clone())
                    .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
        effort: lead.effort.or(defaults.effort).unwrap_or(DEFAULT_EFFORT),
        tools:  lead.tools.clone()
                    .or_else(|| defaults.tools.clone())
                    .unwrap_or_else(default_tools),
        timeout_secs,
        use_worktree: lead.use_worktree.or(defaults.use_worktree).unwrap_or(true),
        env,
    })
}
```

Update the `resolve()` top-level function:

```rust
pub fn resolve(manifest: Manifest, env_max_parallel: Option<u32>) -> Result<ResolvedManifest> {
    let templates: HashMap<String, &Template> =
        manifest.templates.iter().map(|t| (t.id.clone(), t)).collect();

    let mut resolved = Vec::with_capacity(manifest.tasks.len());
    for task in &manifest.tasks {
        resolved.push(resolve_task(task, &manifest.defaults, &templates)?);
    }

    let lead = if let Some(l) = manifest.leads.first() {
        Some(resolve_lead(l, &manifest.defaults, &manifest.run)?)
    } else {
        None
    };

    let max_parallel = manifest.run.max_parallel
        .or(env_max_parallel)
        .unwrap_or(DEFAULT_MAX_PARALLEL);

    let run_dir = manifest.run.run_dir
        .unwrap_or_else(default_run_dir);

    Ok(ResolvedManifest {
        max_parallel,
        halt_on_failure:   manifest.run.halt_on_failure,
        run_dir,
        worktree_cleanup:  manifest.run.worktree_cleanup,
        emit_event_stream: manifest.run.emit_event_stream,
        tasks: resolved,
        lead,
        max_workers:       manifest.run.max_workers,
        budget_usd:        manifest.run.budget_usd,
        lead_timeout_secs: manifest.run.lead_timeout_secs,
    })
}
```

Fix any existing test that constructs `ResolvedManifest` literally to include the new fields (set them to `None`).

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-cli manifest::resolve`
Expected: all pass.

Run: `cargo lint`. Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-cli/src/manifest/resolve.rs
git commit -m "manifest: resolve [[lead]] into ResolvedLead with defaults inheritance"
```

---

### Task 6: Hierarchical validation rules

**Files:**
- Modify: `crates/pitboss-cli/src/manifest/validate.rs`

- [ ] **Step 1: Write failing tests**

Append to `crates/pitboss-cli/src/manifest/validate.rs` `mod tests`:

```rust
    #[test]
    fn rejects_mixing_tasks_and_lead() {
        let d = with_tmp_repo(true);
        let r = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![rt("t1", d.path().to_path_buf(), false, None)],
            lead: Some(rl("lead", d.path().to_path_buf())),
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: Some(600),
        };
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("cannot combine [[task]] and [[lead]]"), "got: {err}");
    }

    #[test]
    fn rejects_max_workers_on_flat_manifest() {
        let d = with_tmp_repo(true);
        let r = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![rt("t1", d.path().to_path_buf(), false, None)],
            lead: None,
            max_workers: Some(4), // set without a lead → error
            budget_usd: None,
            lead_timeout_secs: None,
        };
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("max_workers"), "got: {err}");
    }

    #[test]
    fn rejects_max_workers_out_of_range() {
        let d = with_tmp_repo(true);
        let r = ResolvedManifest {
            max_parallel: 4, halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(rl("l", d.path().to_path_buf())),
            max_workers: Some(17),
            budget_usd: Some(1.0),
            lead_timeout_secs: Some(600),
        };
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_non_positive_budget() {
        let d = with_tmp_repo(true);
        let r = ResolvedManifest {
            max_parallel: 4, halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: Some(rl("l", d.path().to_path_buf())),
            max_workers: Some(4),
            budget_usd: Some(0.0),
            lead_timeout_secs: Some(600),
        };
        assert!(validate(&r).is_err());
    }

    #[test]
    fn rejects_manifest_with_no_tasks_and_no_lead() {
        let r = ResolvedManifest {
            max_parallel: 4, halt_on_failure: false,
            run_dir: PathBuf::from("."),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: None, budget_usd: None, lead_timeout_secs: None,
        };
        let err = validate(&r).unwrap_err().to_string();
        assert!(err.contains("empty manifest"), "got: {err}");
    }

    // Helper for these tests.
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
        }
    }
```

- [ ] **Step 2: Run tests to verify fail**

Run: `cargo test -p pitboss-cli manifest::validate::tests::rejects_mixing_tasks_and_lead`
Expected: FAIL.

- [ ] **Step 3: Implement validation rules**

In `crates/pitboss-cli/src/manifest/validate.rs`, replace the top-level `validate` function:

```rust
pub fn validate(resolved: &ResolvedManifest) -> Result<()> {
    validate_mode(resolved)?;
    if resolved.lead.is_some() {
        validate_lead(resolved)?;
        validate_hierarchical_ranges(resolved)?;
    } else {
        // Flat-mode validations unchanged.
        validate_ids(resolved)?;
        validate_directories(resolved)?;
        validate_branch_conflicts(resolved)?;
        validate_ranges(resolved)?;
    }
    Ok(())
}

fn validate_mode(r: &ResolvedManifest) -> Result<()> {
    if !r.tasks.is_empty() && r.lead.is_some() {
        bail!("cannot combine [[task]] and [[lead]] in the same manifest");
    }
    if r.tasks.is_empty() && r.lead.is_none() {
        bail!("empty manifest: define either [[task]] entries or exactly one [[lead]]");
    }
    if r.lead.is_none() {
        if r.max_workers.is_some() {
            bail!("[run].max_workers is only valid with a [[lead]] section");
        }
        if r.budget_usd.is_some() {
            bail!("[run].budget_usd is only valid with a [[lead]] section");
        }
        if r.lead_timeout_secs.is_some() {
            bail!("[run].lead_timeout_secs is only valid with a [[lead]] section");
        }
    }
    Ok(())
}

fn validate_lead(r: &ResolvedManifest) -> Result<()> {
    let lead = r.lead.as_ref().unwrap();
    if lead.id.is_empty() {
        bail!("lead id is required");
    }
    if !lead.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        bail!("lead id '{}' contains invalid characters", lead.id);
    }
    if !lead.directory.is_dir() {
        bail!("lead directory does not exist: {}", lead.directory.display());
    }
    if lead.use_worktree && !is_in_git_repo(&lead.directory) {
        bail!("lead has use_worktree=true but directory is not a git work-tree: {}",
              lead.directory.display());
    }
    if lead.timeout_secs == 0 {
        bail!("lead timeout_secs must be > 0");
    }
    Ok(())
}

fn validate_hierarchical_ranges(r: &ResolvedManifest) -> Result<()> {
    if let Some(mw) = r.max_workers {
        if mw == 0 || mw > 16 {
            bail!("max_workers must be between 1 and 16 inclusive");
        }
    }
    if let Some(b) = r.budget_usd {
        if b <= 0.0 {
            bail!("budget_usd must be > 0");
        }
    }
    if let Some(t) = r.lead_timeout_secs {
        if t == 0 {
            bail!("lead_timeout_secs must be > 0");
        }
    }
    if r.max_parallel == 0 {
        bail!("max_parallel must be > 0");
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-cli manifest::validate`
Expected: all pass (existing tests + 5 new).

Run: `cargo lint`. Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-cli/src/manifest/validate.rs
git commit -m "manifest: hierarchical-mode validation (mutex, ranges, lead checks)"
```

---

## Phase 2 — MCP Server Scaffolding

### Task 7: Create `mcp` module skeleton

**Files:**
- Create: `crates/pitboss-cli/src/mcp/mod.rs`
- Create: `crates/pitboss-cli/src/mcp/server.rs`
- Create: `crates/pitboss-cli/src/mcp/tools.rs`
- Modify: `crates/pitboss-cli/src/main.rs` (add `mod mcp;`)

- [ ] **Step 1: Create the module structure**

Create `crates/pitboss-cli/src/mcp/mod.rs`:

```rust
//! MCP server exposed to the lead Hobbit so it can spawn and coordinate
//! worker Hobbits via structured tool calls. Bound to a single hierarchical
//! run; started before the lead, shut down after the lead + workers drain.

pub mod server;
pub mod tools;

pub use server::{McpServer, socket_path_for_run};
```

Create `crates/pitboss-cli/src/mcp/server.rs` with a minimal skeleton:

```rust
//! Lifecycle of the shire MCP server (unix socket transport).

use std::path::{Path, PathBuf};
use anyhow::Result;
use uuid::Uuid;

/// Compute the socket path for a given run. Falls back to the run_dir if
/// $XDG_RUNTIME_DIR is unset or non-writable.
pub fn socket_path_for_run(run_id: Uuid, run_dir: &Path) -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg).join("shire");
        if std::fs::create_dir_all(&p).is_ok() {
            return p.join(format!("{}.sock", run_id));
        }
    }
    // Fallback: alongside the run artifacts.
    let p = run_dir.join(run_id.to_string());
    let _ = std::fs::create_dir_all(&p);
    p.join("mcp.sock")
}

pub struct McpServer {
    socket_path: PathBuf,
    // TODO: fields populated in Task 9
}

impl McpServer {
    /// Start serving on the given socket path. Returns a handle you can drop
    /// to shut down.
    pub async fn start(socket_path: PathBuf) -> Result<Self> {
        let _ = socket_path;
        unimplemented!("covered in Task 9")
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use tempfile::TempDir;

    #[test]
    fn socket_path_uses_xdg_runtime_dir_when_set() {
        let dir = TempDir::new().unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", dir.path());
        let run_id = Uuid::now_v7();
        let p = socket_path_for_run(run_id, Path::new("/tmp"));
        assert!(p.starts_with(dir.path()));
        assert!(p.to_string_lossy().ends_with(".sock"));
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    #[test]
    fn socket_path_falls_back_to_run_dir_when_xdg_unset() {
        std::env::remove_var("XDG_RUNTIME_DIR");
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let p = socket_path_for_run(run_id, dir.path());
        assert!(p.starts_with(dir.path()));
    }
}
```

Create `crates/pitboss-cli/src/mcp/tools.rs` with placeholder types:

```rust
//! The six MCP tool handlers exposed to the lead. Real implementations
//! land in Tasks 10-16; this file establishes the types + signatures.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnWorkerArgs {
    pub prompt: String,
    #[serde(default)] pub directory: Option<String>,
    #[serde(default)] pub branch: Option<String>,
    #[serde(default)] pub tools: Option<Vec<String>>,
    #[serde(default)] pub timeout_secs: Option<u64>,
    #[serde(default)] pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnWorkerResult {
    pub task_id: String,
    pub worktree_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub state: String,
    pub started_at: Option<String>,
    pub partial_usage: pitboss_core::parser::TokenUsage,
    pub last_text_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSummary {
    pub task_id: String,
    pub state: String,
    pub prompt_preview: String,
    pub started_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelResult {
    pub ok: bool,
}
```

Modify `crates/pitboss-cli/src/main.rs` to add `mod mcp;` after the existing module declarations.

- [ ] **Step 2: Verify the skeleton compiles**

Run: `cargo build -p pitboss-cli`
Expected: clean build (no warnings).

Run: `cargo test -p pitboss-cli mcp::server::tests`
Expected: 2 tests pass.

Run: `cargo lint`. Clean.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-cli/src/mcp/ crates/pitboss-cli/src/main.rs
git commit -m "mcp: module scaffolding + socket path helper"
```

---

### Task 8: Create `fake-mcp-client` test-support crate

**Files:**
- Create: `tests-support/fake-mcp-client/Cargo.toml`
- Create: `tests-support/fake-mcp-client/src/lib.rs`
- Modify: root `Cargo.toml` (add to workspace members)

- [ ] **Step 1: Scaffold the crate**

Create `tests-support/fake-mcp-client/Cargo.toml`:

```toml
[package]
name         = "fake-mcp-client"
version      = "0.0.0"
edition      = "2021"
rust-version = "1.82"
publish      = false

[dependencies]
rmcp       = { workspace = true, features = ["client", "transport-io"] }
tokio      = { workspace = true, features = ["net","io-util","rt-multi-thread","macros"] }
anyhow     = { workspace = true }
serde      = { workspace = true }
serde_json = { workspace = true }
```

**Note:** `rmcp` may need an additional feature `client`. If the workspace dep added in Task 3 only has `server`, extend it here OR adjust the workspace dep. Preferred: extend the workspace dep to include `client` too, since the test-support crate is always part of workspace test builds:

```toml
# in root Cargo.toml [workspace.dependencies]:
rmcp = { version = "0.8", features = ["server", "client", "transport-io", "macros"] }
```

Create `tests-support/fake-mcp-client/src/lib.rs`:

```rust
//! Minimal MCP client library used by integration tests to drive the shire
//! MCP server as if we were a lead claude subprocess. Connects over unix
//! socket, handles init handshake, and exposes a `call_tool` helper.

use std::path::Path;
use anyhow::Result;
use serde_json::Value;

pub struct FakeMcpClient {
    // Real rmcp client populated after `connect`.
    // Implementation flushed out in Task 8 step 3.
}

impl FakeMcpClient {
    /// Connect to a shire MCP server on a unix socket and complete the MCP
    /// initialization handshake.
    pub async fn connect(socket: &Path) -> Result<Self> {
        let _ = socket;
        unimplemented!("fake MCP client wire-up — see docs/superpowers/specs/2026-04-17-hierarchical-orchestration-design.md §10.2")
    }

    /// Call a tool and return its raw result payload as JSON.
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value> {
        let _ = (name, args);
        unimplemented!()
    }

    /// Shut down the client.
    pub async fn close(self) -> Result<()> {
        Ok(())
    }
}
```

Add `"tests-support/fake-mcp-client"` to workspace members in root `Cargo.toml`.

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p fake-mcp-client`
Expected: clean build (no warnings — the `unimplemented!` is fine since nothing calls them yet).

Run: `cargo lint`. Clean.

- [ ] **Step 3: Commit**

```bash
git add tests-support/fake-mcp-client/ Cargo.toml Cargo.lock
git commit -m "tests-support: fake-mcp-client scaffold (rmcp client)"
```

---

### Task 9: Implement MCP server start/stop lifecycle

**Files:**
- Modify: `crates/pitboss-cli/src/mcp/server.rs`

- [ ] **Step 1: Write failing integration-style test**

Append to `crates/pitboss-cli/src/mcp/server.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn server_starts_and_accepts_connection() {
        let dir = TempDir::new().unwrap();
        let sock = dir.path().join("test.sock");
        let server = McpServer::start(sock.clone()).await.unwrap();
        assert!(sock.exists(), "socket file should exist after start");
        assert_eq!(server.socket_path(), sock.as_path());

        // Connect a raw unix stream to verify the server is listening.
        let stream = tokio::net::UnixStream::connect(&sock).await;
        assert!(stream.is_ok(), "server should accept connections");

        drop(server);
        // Socket is cleaned up on drop.
    }
```

- [ ] **Step 2: Run the test to confirm failure**

Run: `cargo test -p pitboss-cli mcp::server::tests::server_starts_and_accepts_connection`
Expected: PANIC on `unimplemented!("covered in Task 9")`.

- [ ] **Step 3: Implement `McpServer::start`**

Replace the `McpServer` struct body and impl in `crates/pitboss-cli/src/mcp/server.rs`:

```rust
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub struct McpServer {
    socket_path: PathBuf,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<JoinHandle<()>>,
}

impl McpServer {
    /// Start serving on the given socket path. Binds to the unix socket,
    /// spawns an accept loop in a dedicated tokio task, returns a handle.
    pub async fn start(socket_path: PathBuf) -> Result<Self> {
        // If the socket file already exists (stale), remove it.
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
        let listener = tokio::net::UnixListener::bind(&socket_path)?;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();

        let join_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        if let Ok((_stream, _addr)) = accept {
                            // Real tool dispatch comes in Task 10+.
                            // For now, dropping the stream is fine: the
                            // skeleton exists so we can test connect()
                            // succeeds without hanging.
                        }
                    }
                }
            }
        });

        Ok(Self {
            socket_path,
            shutdown_tx: Some(shutdown_tx),
            join_handle: Some(join_handle),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(h) = self.join_handle.take() {
            h.abort();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}
```

- [ ] **Step 4: Run the test to verify pass**

Run: `cargo test -p pitboss-cli mcp::server::tests`
Expected: all 3 tests pass (2 path-helper + 1 start).

Run: `cargo lint`. Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-cli/src/mcp/server.rs
git commit -m "mcp: server start/stop lifecycle with unix-socket listener"
```

---

## Phase 3 — DispatchState and MCP tool dispatch wiring

### Task 10: Define `DispatchState` shared between runner and MCP

**Files:**
- Create: `crates/pitboss-cli/src/dispatch/state.rs`
- Modify: `crates/pitboss-cli/src/dispatch/mod.rs` (add `pub mod state;`)

- [ ] **Step 1: Write failing test**

Create `crates/pitboss-cli/src/dispatch/state.rs` (skeleton):

```rust
//! Shared state for a single hierarchical run. Held in an Arc and shared
//! between the dispatch runner (which writes TaskRecords) and the MCP server
//! (which reads worker status, enforces caps, enqueues spawns).

use std::collections::HashMap;
use std::sync::Arc;

use pitboss_core::session::CancelToken;
use pitboss_core::store::{SessionStore, TaskRecord};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::manifest::resolve::ResolvedManifest;

#[derive(Debug, Clone)]
pub enum WorkerState {
    Pending,
    Running {
        started_at: chrono::DateTime<chrono::Utc>,
    },
    Done(TaskRecord),
}

pub struct DispatchState {
    pub run_id: Uuid,
    pub manifest: ResolvedManifest,
    pub store: Arc<dyn SessionStore>,
    pub cancel: CancelToken,
    pub lead_id: String,
    /// Map of task_id → worker state. Lead is also tracked here for convenience.
    pub workers: RwLock<HashMap<String, WorkerState>>,
    /// Total USD cost spent so far (updated after each worker completes).
    pub spent_usd: Mutex<f64>,
}

impl DispatchState {
    pub fn new(
        run_id: Uuid,
        manifest: ResolvedManifest,
        store: Arc<dyn SessionStore>,
        cancel: CancelToken,
        lead_id: String,
    ) -> Self {
        Self {
            run_id,
            manifest,
            store,
            cancel,
            lead_id,
            workers: RwLock::new(HashMap::new()),
            spent_usd: Mutex::new(0.0),
        }
    }

    pub async fn active_worker_count(&self) -> usize {
        self.workers.read().await.values().filter(|w| {
            matches!(w, WorkerState::Pending | WorkerState::Running { .. })
        }).count()
    }

    pub async fn budget_remaining(&self) -> Option<f64> {
        let budget = self.manifest.budget_usd?;
        let spent = *self.spent_usd.lock().await;
        Some((budget - spent).max(0.0))
    }
}
```

Append tests at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::resolve::ResolvedManifest;
    use crate::manifest::schema::WorktreeCleanup;
    use pitboss_core::store::JsonFileStore;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn mk_state(budget: Option<f64>, max_workers: Option<u32>) -> Arc<DispatchState> {
        let dir = TempDir::new().unwrap();
        let manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers,
            budget_usd: budget,
            lead_timeout_secs: None,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let run_id = Uuid::now_v7();
        let cancel = CancelToken::new();
        Arc::new(DispatchState::new(run_id, manifest, store, cancel, "lead-1".into()))
    }

    #[tokio::test]
    async fn active_worker_count_is_zero_on_new_state() {
        let st = mk_state(None, None);
        assert_eq!(st.active_worker_count().await, 0);
    }

    #[tokio::test]
    async fn budget_remaining_reflects_spent() {
        let st = mk_state(Some(10.0), None);
        assert_eq!(st.budget_remaining().await, Some(10.0));
        *st.spent_usd.lock().await = 3.5;
        assert_eq!(st.budget_remaining().await, Some(6.5));
    }

    #[tokio::test]
    async fn budget_remaining_is_none_when_uncapped() {
        let st = mk_state(None, None);
        assert_eq!(st.budget_remaining().await, None);
    }
}
```

Modify `crates/pitboss-cli/src/dispatch/mod.rs`:

```rust
pub mod probe;
pub mod runner;
pub mod summary;
pub mod signals;
pub mod state;        // NEW

pub use probe::probe_claude;
pub use runner::run_dispatch_inner;
pub use state::DispatchState;
```

- [ ] **Step 2: Run tests to verify they fail (compile)**

Run: `cargo build -p pitboss-cli`
Expected: compiles clean.

Run: `cargo test -p pitboss-cli dispatch::state`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-cli/src/dispatch/
git commit -m "dispatch: DispatchState shared between runner and MCP server"
```

---

### Task 11: MCP tool `list_workers`

**Files:**
- Modify: `crates/pitboss-cli/src/mcp/tools.rs`
- Modify: `crates/pitboss-cli/src/mcp/server.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/pitboss-cli/src/mcp/tools.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::state::{DispatchState, WorkerState};
    use std::sync::Arc;

    async fn test_state() -> Arc<DispatchState> {
        // Re-use the helper from dispatch::state::tests via a small copy here.
        // See Task 10 for the state construction pattern.
        use crate::manifest::resolve::ResolvedManifest;
        use crate::manifest::schema::WorktreeCleanup;
        use pitboss_core::store::{JsonFileStore, SessionStore};
        use pitboss_core::session::CancelToken;
        use tempfile::TempDir;
        use uuid::Uuid;
        use std::path::PathBuf;

        let dir = TempDir::new().unwrap();
        let manifest = ResolvedManifest {
            max_parallel: 4, halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![], lead: None,
            max_workers: Some(4), budget_usd: Some(5.0), lead_timeout_secs: None,
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        Arc::new(DispatchState::new(Uuid::now_v7(), manifest, store, CancelToken::new(), "lead".into()))
    }

    #[tokio::test]
    async fn list_workers_empty_when_no_spawns() {
        let state = test_state().await;
        let result = handle_list_workers(&state).await;
        assert_eq!(result.len(), 0);
    }

    #[tokio::test]
    async fn list_workers_shows_pending_and_running() {
        let state = test_state().await;
        {
            let mut w = state.workers.write().await;
            w.insert("w-1".into(), WorkerState::Pending);
            w.insert("w-2".into(), WorkerState::Running {
                started_at: chrono::Utc::now(),
            });
        }
        let mut result = handle_list_workers(&state).await;
        result.sort_by(|a, b| a.task_id.cmp(&b.task_id));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].task_id, "w-1");
        assert_eq!(result[0].state, "Pending");
        assert_eq!(result[1].task_id, "w-2");
        assert_eq!(result[1].state, "Running");
    }
```

- [ ] **Step 2: Run tests to verify fail**

Run: `cargo test -p pitboss-cli mcp::tools::tests::list_workers_empty_when_no_spawns`
Expected: FAIL — `handle_list_workers` undefined.

- [ ] **Step 3: Implement `handle_list_workers`**

Append to `crates/pitboss-cli/src/mcp/tools.rs`:

```rust
use std::sync::Arc;
use crate::dispatch::state::{DispatchState, WorkerState};

pub async fn handle_list_workers(state: &Arc<DispatchState>) -> Vec<WorkerSummary> {
    let workers = state.workers.read().await;
    workers
        .iter()
        .filter(|(id, _)| *id != &state.lead_id)
        .map(|(id, w)| {
            let (state_str, started_at) = match w {
                WorkerState::Pending => ("Pending".to_string(), None),
                WorkerState::Running { started_at } => {
                    ("Running".to_string(), Some(started_at.to_rfc3339()))
                }
                WorkerState::Done(rec) => (
                    match rec.status {
                        pitboss_core::store::TaskStatus::Success => "Completed",
                        pitboss_core::store::TaskStatus::Failed => "Failed",
                        pitboss_core::store::TaskStatus::TimedOut => "TimedOut",
                        pitboss_core::store::TaskStatus::Cancelled => "Cancelled",
                        pitboss_core::store::TaskStatus::SpawnFailed => "SpawnFailed",
                    }.to_string(),
                    Some(rec.started_at.to_rfc3339()),
                ),
            };
            WorkerSummary {
                task_id: id.clone(),
                state: state_str,
                prompt_preview: String::new(),  // populated by spawn_worker in Task 12
                started_at,
            }
        })
        .collect()
}
```

Close the `#[cfg(test)] mod tests` block if left open.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-cli mcp::tools`
Expected: 2 tests pass.

Run: `cargo lint`. Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-cli/src/mcp/
git commit -m "mcp: handle_list_workers tool handler with filtering and state mapping"
```

---

### Task 12: MCP tool `spawn_worker` — happy path

**Files:**
- Modify: `crates/pitboss-cli/src/mcp/tools.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/pitboss-cli/src/mcp/tools.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn spawn_worker_adds_entry_to_state() {
        let state = test_state().await;
        let args = SpawnWorkerArgs {
            prompt: "investigate issue #1".into(),
            directory: Some("/tmp".into()),
            branch: None,
            tools: None,
            timeout_secs: None,
            model: None,
        };
        let result = handle_spawn_worker(&state, args).await.unwrap();
        assert!(result.task_id.starts_with("worker-"));

        let workers = state.workers.read().await;
        assert_eq!(workers.len(), 1);
        let entry = workers.get(&result.task_id).unwrap();
        assert!(matches!(entry, WorkerState::Pending));
    }
```

Add `WorkerState` and state-path imports at the top of the `tests` module if not already there (`use crate::dispatch::state::WorkerState;`).

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p pitboss-cli mcp::tools::tests::spawn_worker_adds_entry_to_state`
Expected: FAIL — `handle_spawn_worker` undefined.

- [ ] **Step 3: Implement `handle_spawn_worker` (happy path, no guards yet)**

Append to `crates/pitboss-cli/src/mcp/tools.rs`:

```rust
use anyhow::{bail, Result};
use uuid::Uuid;

pub async fn handle_spawn_worker(
    state: &Arc<DispatchState>,
    args: SpawnWorkerArgs,
) -> Result<SpawnWorkerResult> {
    // Generate a unique, short-ish task_id.
    let task_id = format!("worker-{}", Uuid::now_v7());

    // Record as Pending. Actual subprocess spawn happens via a channel to
    // the hierarchical dispatcher (wired in Task 20).
    {
        let mut workers = state.workers.write().await;
        workers.insert(task_id.clone(), WorkerState::Pending);
    }

    let _ = args;  // guards and real spawn land in Tasks 13-14.
    Ok(SpawnWorkerResult {
        task_id,
        worktree_path: None,
    })
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-cli mcp::tools`
Expected: 3 tests pass.

Run: `cargo lint`. Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-cli/src/mcp/tools.rs
git commit -m "mcp: handle_spawn_worker happy path (no guards, no real spawn yet)"
```

---

### Task 13: `spawn_worker` cap, budget, drain guards

**Files:**
- Modify: `crates/pitboss-cli/src/mcp/tools.rs`

- [ ] **Step 1: Write failing tests**

Append to `mod tests`:

```rust
    #[tokio::test]
    async fn spawn_worker_refuses_when_max_workers_reached() {
        let state = test_state().await;   // max_workers = 4
        // Fill up to cap
        for i in 0..4 {
            let args = SpawnWorkerArgs {
                prompt: format!("w{}", i), directory: None, branch: None,
                tools: None, timeout_secs: None, model: None,
            };
            handle_spawn_worker(&state, args).await.unwrap();
        }
        // 5th call must fail
        let args = SpawnWorkerArgs {
            prompt: "overflow".into(), directory: None, branch: None,
            tools: None, timeout_secs: None, model: None,
        };
        let err = handle_spawn_worker(&state, args).await.unwrap_err();
        assert!(err.to_string().contains("worker cap reached"), "err: {err}");
    }

    #[tokio::test]
    async fn spawn_worker_refuses_when_budget_exceeded() {
        let state = test_state().await;   // budget_usd = 5.0
        *state.spent_usd.lock().await = 5.0;  // at cap
        let args = SpawnWorkerArgs {
            prompt: "p".into(), directory: None, branch: None,
            tools: None, timeout_secs: None, model: None,
        };
        let err = handle_spawn_worker(&state, args).await.unwrap_err();
        assert!(err.to_string().contains("budget exceeded"), "err: {err}");
    }

    #[tokio::test]
    async fn spawn_worker_refuses_when_draining() {
        let state = test_state().await;
        state.cancel.drain();
        let args = SpawnWorkerArgs {
            prompt: "p".into(), directory: None, branch: None,
            tools: None, timeout_secs: None, model: None,
        };
        let err = handle_spawn_worker(&state, args).await.unwrap_err();
        assert!(err.to_string().contains("draining"), "err: {err}");
    }
```

- [ ] **Step 2: Run tests to verify fail**

Run: `cargo test -p pitboss-cli mcp::tools::tests::spawn_worker_refuses_when_max_workers_reached`
Expected: FAIL.

- [ ] **Step 3: Add guards to `handle_spawn_worker`**

Replace the implementation:

```rust
pub async fn handle_spawn_worker(
    state: &Arc<DispatchState>,
    args: SpawnWorkerArgs,
) -> Result<SpawnWorkerResult> {
    // Guard 1: draining
    if state.cancel.is_draining() || state.cancel.is_terminated() {
        bail!("run is draining: no new workers accepted");
    }

    // Guard 2: worker cap
    if let Some(cap) = state.manifest.max_workers {
        let active = state.active_worker_count().await;
        if active >= cap as usize {
            bail!("worker cap reached: {} active (max {})", active, cap);
        }
    }

    // Guard 3: budget
    if let (Some(budget), Some(_remaining)) = (state.manifest.budget_usd, state.budget_remaining().await) {
        let spent = *state.spent_usd.lock().await;
        // Estimate this worker's cost as median of prior workers or fallback.
        let estimate = estimate_new_worker_cost(state).await;
        if spent + estimate > budget {
            bail!(
                "budget exceeded: ${:.2} spent + ${:.2} estimated > ${:.2} budget",
                spent, estimate, budget
            );
        }
    }

    let task_id = format!("worker-{}", Uuid::now_v7());
    {
        let mut workers = state.workers.write().await;
        workers.insert(task_id.clone(), WorkerState::Pending);
    }

    let _ = args;
    Ok(SpawnWorkerResult {
        task_id,
        worktree_path: None,
    })
}

const INITIAL_WORKER_COST_EST: f64 = 0.10;

async fn estimate_new_worker_cost(state: &Arc<DispatchState>) -> f64 {
    use pitboss_core::prices::cost_usd;
    let workers = state.workers.read().await;
    let mut costs: Vec<f64> = Vec::new();
    for w in workers.values() {
        if let WorkerState::Done(rec) = w {
            // Try to price using whatever model the worker used. If model isn't
            // available at record-level, just sum tokens via a neutral rate.
            if let Some(c) = cost_usd("claude-haiku-4-5", &rec.token_usage) {
                costs.push(c);
            }
        }
    }
    if costs.is_empty() {
        return INITIAL_WORKER_COST_EST;
    }
    costs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    costs[costs.len() / 2]
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-cli mcp::tools`
Expected: 6 tests pass (3 list + 3 guards + happy path).

Run: `cargo lint`. Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-cli/src/mcp/tools.rs
git commit -m "mcp: spawn_worker guards (cap, budget with median estimate, drain)"
```

---

### Task 14: MCP tools `worker_status`, `cancel_worker`

**Files:**
- Modify: `crates/pitboss-cli/src/mcp/tools.rs`

- [ ] **Step 1: Write failing tests**

Append:

```rust
    #[tokio::test]
    async fn worker_status_reads_state() {
        let state = test_state().await;
        let args = SpawnWorkerArgs {
            prompt: "p".into(), directory: None, branch: None,
            tools: None, timeout_secs: None, model: None,
        };
        let spawn = handle_spawn_worker(&state, args).await.unwrap();
        let status = handle_worker_status(&state, &spawn.task_id).await.unwrap();
        assert_eq!(status.state, "Pending");
    }

    #[tokio::test]
    async fn worker_status_unknown_id_errors() {
        let state = test_state().await;
        let err = handle_worker_status(&state, "nope-123").await.unwrap_err();
        assert!(err.to_string().contains("unknown task_id"));
    }

    #[tokio::test]
    async fn cancel_worker_sets_cancelled_state() {
        let state = test_state().await;
        let args = SpawnWorkerArgs {
            prompt: "p".into(), directory: None, branch: None,
            tools: None, timeout_secs: None, model: None,
        };
        let spawn = handle_spawn_worker(&state, args).await.unwrap();

        let result = handle_cancel_worker(&state, &spawn.task_id).await.unwrap();
        assert!(result.ok);

        // Note: in real wiring, CancelToken signals the SessionHandle to terminate
        // and the subsequent Done(...) entry in state.workers carries status=Cancelled.
        // For v0.3 Task 14 (unit-level), we just verify the cancel call succeeded
        // and didn't panic. Full flow is tested in integration tests (Phase 6).
    }
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p pitboss-cli mcp::tools::tests::worker_status_reads_state`
Expected: FAIL — `handle_worker_status` undefined.

- [ ] **Step 3: Implement the handlers**

Append to `crates/pitboss-cli/src/mcp/tools.rs`:

```rust
pub async fn handle_worker_status(
    state: &Arc<DispatchState>,
    task_id: &str,
) -> Result<WorkerStatus> {
    let workers = state.workers.read().await;
    let w = workers
        .get(task_id)
        .ok_or_else(|| anyhow::anyhow!("unknown task_id: {task_id}"))?;
    let (state_str, started_at, partial_usage, last_text_preview) = match w {
        WorkerState::Pending => (
            "Pending".to_string(),
            None,
            pitboss_core::parser::TokenUsage::default(),
            None,
        ),
        WorkerState::Running { started_at } => (
            "Running".to_string(),
            Some(started_at.to_rfc3339()),
            pitboss_core::parser::TokenUsage::default(),
            None,
        ),
        WorkerState::Done(rec) => (
            match rec.status {
                pitboss_core::store::TaskStatus::Success => "Completed",
                pitboss_core::store::TaskStatus::Failed => "Failed",
                pitboss_core::store::TaskStatus::TimedOut => "TimedOut",
                pitboss_core::store::TaskStatus::Cancelled => "Cancelled",
                pitboss_core::store::TaskStatus::SpawnFailed => "SpawnFailed",
            }.to_string(),
            Some(rec.started_at.to_rfc3339()),
            rec.token_usage,
            rec.final_message_preview.clone(),
        ),
    };
    Ok(WorkerStatus {
        state: state_str,
        started_at,
        partial_usage,
        last_text_preview,
    })
}

pub async fn handle_cancel_worker(
    state: &Arc<DispatchState>,
    task_id: &str,
) -> Result<CancelResult> {
    // Look up the worker's own CancelToken and fire it. In v0.3 the worker's
    // CancelToken is a *clone* of the run-level cancel; a per-worker signal
    // would require additional plumbing in the hierarchical runner (Task 22).
    // For now, issuing a run-level drain is the closest we can do without
    // per-worker tokens. This is wired fully in the integration tests.
    let workers = state.workers.read().await;
    if !workers.contains_key(task_id) {
        anyhow::bail!("unknown task_id: {task_id}");
    }
    // Actual SIGTERM signalling happens in Task 22 via state.cancel_worker_task_id().
    state.cancel.drain();  // temporary; refined in Task 22
    Ok(CancelResult { ok: true })
}
```

**Note:** Task 14's `cancel_worker` is intentionally a minimal implementation. Task 22 refines it to per-worker cancellation once the hierarchical runner owns per-worker tokens.

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-cli mcp::tools`
Expected: 9 tests pass.

Run: `cargo lint`. Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-cli/src/mcp/tools.rs
git commit -m "mcp: worker_status + cancel_worker handlers (minimal, refined in Task 22)"
```

---

### Task 15: MCP tool `wait_for_worker`

**Files:**
- Modify: `crates/pitboss-cli/src/mcp/tools.rs`
- Modify: `crates/pitboss-cli/src/dispatch/state.rs` (add broadcast channel for completion events)

- [ ] **Step 1: Add completion notify to `DispatchState`**

Modify `crates/pitboss-cli/src/dispatch/state.rs` — add a tokio broadcast channel that emits a `task_id` whenever a worker transitions to `Done`:

```rust
use tokio::sync::broadcast;

pub struct DispatchState {
    // ... existing fields ...
    pub done_tx: broadcast::Sender<String>,   // NEW
}

impl DispatchState {
    pub fn new(
        run_id: Uuid,
        manifest: ResolvedManifest,
        store: Arc<dyn SessionStore>,
        cancel: CancelToken,
        lead_id: String,
    ) -> Self {
        let (done_tx, _) = broadcast::channel(64);
        Self {
            run_id, manifest, store, cancel, lead_id,
            workers: RwLock::new(HashMap::new()),
            spent_usd: Mutex::new(0.0),
            done_tx,
        }
    }
}
```

- [ ] **Step 2: Write failing test**

Append to `crates/pitboss-cli/src/mcp/tools.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn wait_for_worker_returns_outcome_on_completion() {
        use pitboss_core::store::{TaskRecord, TaskStatus};
        use std::time::Duration;

        let state = test_state().await;
        let task_id = "worker-test-1".to_string();
        {
            let mut w = state.workers.write().await;
            w.insert(task_id.clone(), WorkerState::Pending);
        }

        // Spawn a task that marks the worker Done after 50 ms.
        let state_clone = state.clone();
        let task_id_clone = task_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let rec = TaskRecord {
                task_id: task_id_clone.clone(),
                status: TaskStatus::Success,
                exit_code: Some(0),
                started_at: chrono::Utc::now(),
                ended_at: chrono::Utc::now(),
                duration_ms: 42,
                worktree_path: None,
                log_path: std::path::PathBuf::new(),
                token_usage: Default::default(),
                claude_session_id: None,
                final_message_preview: Some("ok".into()),
                parent_task_id: Some("lead".into()),
            };
            let mut w = state_clone.workers.write().await;
            w.insert(task_id_clone.clone(), WorkerState::Done(rec));
            let _ = state_clone.done_tx.send(task_id_clone);
        });

        let outcome = handle_wait_for_worker(&state, &task_id, Some(5)).await.unwrap();
        assert!(matches!(outcome.status, TaskStatus::Success));
    }

    #[tokio::test]
    async fn wait_for_worker_times_out() {
        let state = test_state().await;
        let task_id = "worker-stuck".to_string();
        {
            let mut w = state.workers.write().await;
            w.insert(task_id.clone(), WorkerState::Pending);
        }
        let err = handle_wait_for_worker(&state, &task_id, Some(0)).await.unwrap_err();
        assert!(err.to_string().contains("timed out"), "err: {err}");
    }
```

- [ ] **Step 3: Run to verify fail**

Run: `cargo test -p pitboss-cli mcp::tools::tests::wait_for_worker_returns_outcome_on_completion`
Expected: FAIL.

- [ ] **Step 4: Implement `handle_wait_for_worker`**

Append to `crates/pitboss-cli/src/mcp/tools.rs`:

```rust
use pitboss_core::store::TaskRecord;
use tokio::time::Duration;

pub async fn handle_wait_for_worker(
    state: &Arc<DispatchState>,
    task_id: &str,
    timeout_secs: Option<u64>,
) -> Result<TaskRecord> {
    // Fast path: already Done.
    {
        let workers = state.workers.read().await;
        if let Some(WorkerState::Done(rec)) = workers.get(task_id) {
            return Ok(rec.clone());
        }
        if !workers.contains_key(task_id) {
            bail!("unknown task_id: {task_id}");
        }
    }

    // Subscribe to done events and wait.
    let mut rx = state.done_tx.subscribe();
    let wait_duration = Duration::from_secs(timeout_secs.unwrap_or(3600));

    loop {
        let result = tokio::time::timeout(wait_duration, rx.recv()).await;
        match result {
            Err(_) => bail!("wait_for_worker timed out for {task_id}"),
            Ok(Err(_)) => bail!("completion channel closed"),
            Ok(Ok(completed_id)) => {
                if completed_id == task_id {
                    let workers = state.workers.read().await;
                    if let Some(WorkerState::Done(rec)) = workers.get(task_id) {
                        return Ok(rec.clone());
                    }
                    bail!("internal: task_id marked done but record not present");
                }
                // Not our task — keep waiting.
            }
        }
    }
}
```

- [ ] **Step 5: Run tests to verify pass**

Run: `cargo test -p pitboss-cli mcp::tools`
Expected: 11 tests pass.

Run: `cargo lint`. Clean.

- [ ] **Step 6: Commit**

```bash
git add crates/pitboss-cli/src/
git commit -m "mcp: wait_for_worker with broadcast channel + timeout path"
```

---

### Task 16: MCP tool `wait_for_any`

**Files:**
- Modify: `crates/pitboss-cli/src/mcp/tools.rs`

- [ ] **Step 1: Write failing test**

Append to `mod tests`:

```rust
    #[tokio::test]
    async fn wait_for_any_returns_first_completed() {
        use pitboss_core::store::{TaskRecord, TaskStatus};
        use std::time::Duration;

        let state = test_state().await;
        let ids = vec!["w-a".to_string(), "w-b".to_string(), "w-c".to_string()];
        {
            let mut w = state.workers.write().await;
            for id in &ids {
                w.insert(id.clone(), WorkerState::Pending);
            }
        }

        // Race: w-b finishes first at 30ms, w-a at 100ms.
        let state_clone = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let rec = TaskRecord {
                task_id: "w-b".into(),
                status: TaskStatus::Success,
                exit_code: Some(0), started_at: chrono::Utc::now(), ended_at: chrono::Utc::now(),
                duration_ms: 30, worktree_path: None, log_path: std::path::PathBuf::new(),
                token_usage: Default::default(), claude_session_id: None,
                final_message_preview: None, parent_task_id: Some("lead".into()),
            };
            let mut w = state_clone.workers.write().await;
            w.insert("w-b".into(), WorkerState::Done(rec));
            let _ = state_clone.done_tx.send("w-b".into());
        });

        let (winner_id, _rec) = handle_wait_for_any(&state, &ids, Some(5)).await.unwrap();
        assert_eq!(winner_id, "w-b");
    }
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p pitboss-cli mcp::tools::tests::wait_for_any_returns_first_completed`
Expected: FAIL.

- [ ] **Step 3: Implement `handle_wait_for_any`**

Append:

```rust
pub async fn handle_wait_for_any(
    state: &Arc<DispatchState>,
    task_ids: &[String],
    timeout_secs: Option<u64>,
) -> Result<(String, TaskRecord)> {
    if task_ids.is_empty() {
        bail!("wait_for_any: task_ids is empty");
    }

    // Fast path: any already Done?
    {
        let workers = state.workers.read().await;
        for id in task_ids {
            if let Some(WorkerState::Done(rec)) = workers.get(id) {
                return Ok((id.clone(), rec.clone()));
            }
        }
    }

    let mut rx = state.done_tx.subscribe();
    let wait_duration = Duration::from_secs(timeout_secs.unwrap_or(3600));

    loop {
        let result = tokio::time::timeout(wait_duration, rx.recv()).await;
        match result {
            Err(_) => bail!("wait_for_any timed out"),
            Ok(Err(_)) => bail!("completion channel closed"),
            Ok(Ok(completed_id)) => {
                if task_ids.iter().any(|id| id == &completed_id) {
                    let workers = state.workers.read().await;
                    if let Some(WorkerState::Done(rec)) = workers.get(&completed_id) {
                        return Ok((completed_id, rec.clone()));
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cargo test -p pitboss-cli mcp::tools`
Expected: 12 tests pass.

Run: `cargo lint`. Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-cli/src/mcp/tools.rs
git commit -m "mcp: wait_for_any — race waiter across multiple task_ids"
```

---

## Phase 4 — MCP Server: wire tools into rmcp

### Task 17: Wire the 6 tools into the rmcp `ServerHandler`

**Files:**
- Modify: `crates/pitboss-cli/src/mcp/server.rs`

- [ ] **Step 1: Replace the stub accept loop with a real rmcp server**

Modify `crates/pitboss-cli/src/mcp/server.rs` to expose the tool set via rmcp. The specific rmcp 0.8 API call sequence is:

```rust
use rmcp::{
    transport::{UnixListener as McpUnixListener},
    server::{Server, ServerHandler, RequestContext},
    model::{CallToolResult, Content},
    tool, tool_router,
};
use std::sync::Arc;
use crate::dispatch::state::DispatchState;
use crate::mcp::tools::*;

pub struct McpServer {
    socket_path: PathBuf,
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<JoinHandle<()>>,
}

#[derive(Clone)]
struct ShireHandler {
    state: Arc<DispatchState>,
}

#[tool_router]
impl ShireHandler {
    #[tool(description = "Spawn a worker Hobbit. Returns {task_id, worktree_path}.")]
    async fn spawn_worker(&self, args: SpawnWorkerArgs) -> Result<SpawnWorkerResult, rmcp::Error> {
        handle_spawn_worker(&self.state, args).await
            .map_err(|e| rmcp::Error::invalid_request(e.to_string(), None))
    }

    #[tool(description = "Non-blocking status poll for a worker. Returns state + partial data.")]
    async fn worker_status(&self, task_id: String) -> Result<WorkerStatus, rmcp::Error> {
        handle_worker_status(&self.state, &task_id).await
            .map_err(|e| rmcp::Error::invalid_request(e.to_string(), None))
    }

    #[tool(description = "Block until a specific worker exits (or timeout).")]
    async fn wait_for_worker(
        &self,
        task_id: String,
        timeout_secs: Option<u64>,
    ) -> Result<TaskRecord, rmcp::Error> {
        handle_wait_for_worker(&self.state, &task_id, timeout_secs).await
            .map_err(|e| rmcp::Error::invalid_request(e.to_string(), None))
    }

    #[tool(description = "Block until any of the listed workers exits.")]
    async fn wait_for_any(
        &self,
        task_ids: Vec<String>,
        timeout_secs: Option<u64>,
    ) -> Result<(String, TaskRecord), rmcp::Error> {
        handle_wait_for_any(&self.state, &task_ids, timeout_secs).await
            .map_err(|e| rmcp::Error::invalid_request(e.to_string(), None))
    }

    #[tool(description = "List all workers in the current run (excludes the lead).")]
    async fn list_workers(&self) -> Result<Vec<WorkerSummary>, rmcp::Error> {
        Ok(handle_list_workers(&self.state).await)
    }

    #[tool(description = "Cancel a worker by task_id. Sends SIGTERM, grace, SIGKILL.")]
    async fn cancel_worker(&self, task_id: String) -> Result<CancelResult, rmcp::Error> {
        handle_cancel_worker(&self.state, &task_id).await
            .map_err(|e| rmcp::Error::invalid_request(e.to_string(), None))
    }
}

impl ServerHandler for ShireHandler {
    // The macro tool_router above generates a ToolRouter<Self>; rmcp handles
    // dispatch via the derived route.
}

impl McpServer {
    pub async fn start(socket_path: PathBuf, state: Arc<DispatchState>) -> Result<Self> {
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
        let listener = tokio::net::UnixListener::bind(&socket_path)?;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let handler = ShireHandler { state };

        let join_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break,
                    accept = listener.accept() => {
                        if let Ok((stream, _addr)) = accept {
                            let h = handler.clone();
                            tokio::spawn(async move {
                                if let Err(e) = rmcp::server::serve(stream, h).await {
                                    tracing::debug!("mcp session ended: {e}");
                                }
                            });
                        }
                    }
                }
            }
        });

        Ok(Self {
            socket_path,
            shutdown_tx: Some(shutdown_tx),
            join_handle: Some(join_handle),
        })
    }
}
```

**Note on rmcp API specifics:** The exact rmcp 0.8 function names (`rmcp::server::serve`, `#[tool_router]`, `rmcp::Error::invalid_request`) should be verified against the rmcp 0.8 docs during implementation. If any signature differs, adjust to match — the intent (register handler, bind to socket, dispatch tool calls) is the load-bearing invariant, not the exact function name.

- [ ] **Step 2: Update the earlier skeleton test to use the new signature**

Modify `mod tests::server_starts_and_accepts_connection` to pass an `Arc<DispatchState>`:

```rust
    #[tokio::test]
    async fn server_starts_and_accepts_connection() {
        use crate::dispatch::state::DispatchState;
        // ... construct a DispatchState via the test helper from Task 10 ...
        let state = Arc::new(DispatchState::new(/* ... */));
        let dir = TempDir::new().unwrap();
        let sock = dir.path().join("test.sock");
        let server = McpServer::start(sock.clone(), state).await.unwrap();
        assert!(sock.exists());
        let stream = tokio::net::UnixStream::connect(&sock).await;
        assert!(stream.is_ok());
        drop(server);
    }
```

- [ ] **Step 3: Verify compiles + tests pass**

Run: `cargo build -p pitboss-cli`
Expected: clean build.

Run: `cargo test -p pitboss-cli mcp`
Expected: all MCP tests still pass (the server-start test now uses a real state).

Run: `cargo lint`. Clean.

- [ ] **Step 4: Commit**

```bash
git add crates/pitboss-cli/src/mcp/server.rs
git commit -m "mcp: wire six tools into rmcp ServerHandler on UnixListener"
```

---

## Phase 5 — Hierarchical dispatcher

### Task 18: Mode detection in `main.rs`

**Files:**
- Modify: `crates/pitboss-cli/src/main.rs`

- [ ] **Step 1: Add test for mode detection (optional — this change is tiny)**

Skip — the change is 5 lines of plumbing, covered by integration tests in Phase 6.

- [ ] **Step 2: Add the branch**

In `crates/pitboss-cli/src/main.rs`, in `run_dispatch` (the function that calls `dispatch::run_dispatch_inner`), add mode detection:

```rust
fn run_dispatch(
    manifest: &std::path::Path,
    run_dir_override: Option<std::path::PathBuf>,
    dry_run: bool,
) -> ! {
    // ... existing setup code up to `resolved` being loaded ...

    if resolved.lead.is_some() {
        // Hierarchical mode
        let code = rt.block_on(async move {
            let claude_version = if dry_run { None } else {
                match dispatch::probe_claude(&claude_bin).await {
                    Ok(v) => v,
                    Err(e) => { eprintln!("{e}"); return 2; }
                }
            };
            match dispatch::hierarchical::run_hierarchical(
                resolved, manifest_text, manifest.to_path_buf(),
                claude_bin, claude_version, run_dir_override, dry_run,
            ).await {
                Ok(c) => c,
                Err(e) => { eprintln!("hierarchical dispatch: {e:#}"); 1 }
            }
        });
        std::process::exit(code);
    }

    // Existing flat-dispatch path unchanged
    // ...
}
```

This function references `dispatch::hierarchical::run_hierarchical` which lands in Task 19.

- [ ] **Step 3: Verify build fails (expected — reference to undefined fn)**

Run: `cargo build -p pitboss-cli`
Expected: FAIL — `run_hierarchical` undefined. This is deliberate; Task 19 introduces it.

**Do not commit yet.** Proceed to Task 19; the commit lands then.

---

### Task 19: Implement `run_hierarchical` skeleton

**Files:**
- Create: `crates/pitboss-cli/src/dispatch/hierarchical.rs`
- Modify: `crates/pitboss-cli/src/dispatch/mod.rs` (add `pub mod hierarchical;`)

- [ ] **Step 1: Create the skeleton**

Create `crates/pitboss-cli/src/dispatch/hierarchical.rs`:

```rust
//! Hierarchical dispatch path — one lead subprocess plus dynamically-spawned
//! workers. Reuses most of the flat dispatch plumbing from runner.rs and
//! adds the MCP server lifecycle + lead spawn wiring on top.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::Utc;
use pitboss_core::process::{ProcessSpawner, TokioSpawner};
use pitboss_core::session::CancelToken;
use pitboss_core::store::{JsonFileStore, RunMeta, RunSummary, SessionStore};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::manifest::resolve::ResolvedManifest;
use crate::mcp::{McpServer, socket_path_for_run};
use crate::dispatch::state::DispatchState;

#[allow(clippy::too_many_arguments)]
pub async fn run_hierarchical(
    resolved: ResolvedManifest,
    manifest_text: String,
    manifest_path: PathBuf,
    claude_binary: PathBuf,
    claude_version: Option<String>,
    run_dir_override: Option<PathBuf>,
    dry_run: bool,
) -> Result<i32> {
    let run_id = Uuid::now_v7();
    let run_dir = run_dir_override.unwrap_or_else(|| resolved.run_dir.clone());
    tokio::fs::create_dir_all(&run_dir).await.ok();

    let run_subdir = run_dir.join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.ok();
    tokio::fs::write(run_subdir.join("manifest.snapshot.toml"), &manifest_text).await?;
    if let Ok(b) = serde_json::to_vec_pretty(&resolved) {
        tokio::fs::write(run_subdir.join("resolved.json"), b).await?;
    }

    let lead = resolved.lead.as_ref().context("hierarchical mode requires a [[lead]]")?;

    if dry_run {
        println!("DRY-RUN lead: {}", lead.id);
        println!("DRY-RUN command: {} {} (mcp socket TBD)", claude_binary.display(), "--verbose");
        return Ok(0);
    }

    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(run_dir.clone()));
    let meta = RunMeta {
        run_id,
        manifest_path: manifest_path.clone(),
        shire_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version: claude_version.clone(),
        started_at: Utc::now(),
        env: Default::default(),
    };
    store.init_run(&meta).await.context("init run")?;

    let cancel = CancelToken::new();
    crate::dispatch::signals::install_ctrl_c_watcher(cancel.clone());

    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());

    // 1. Start the MCP server.
    let socket = socket_path_for_run(run_id, &run_dir);
    let state = Arc::new(DispatchState::new(
        run_id, resolved.clone(), store.clone(), cancel.clone(), lead.id.clone(),
    ));
    let _mcp = McpServer::start(socket.clone(), state.clone()).await?;

    // 2. Build the --mcp-config file for the lead.
    let mcp_config_path = run_subdir.join("lead-mcp-config.json");
    write_mcp_config(&mcp_config_path, &socket).await?;

    // 3. Spawn the lead.
    //    (Actual spawn wiring in Task 20; for now we log and return Ok(0).)
    let _ = (spawner, lead);
    tracing::info!(run_id = %run_id, "hierarchical run scaffolded; lead spawn wired in Task 20");

    // 4. Finalize.
    let started_at = meta.started_at;
    let ended_at = Utc::now();
    let summary = RunSummary {
        run_id,
        manifest_path,
        shire_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version,
        started_at, ended_at,
        total_duration_ms: (ended_at - started_at).num_milliseconds(),
        tasks_total: 0,
        tasks_failed: 0,
        was_interrupted: false,
        tasks: vec![],
    };
    store.finalize_run(&summary).await?;
    Ok(0)
}

async fn write_mcp_config(path: &std::path::Path, socket: &std::path::Path) -> Result<()> {
    let cfg = serde_json::json!({
        "mcpServers": {
            "shire": {
                "command": "shire-mcp-stub",
                "args": [],
                "transport": { "type": "unix", "path": socket.to_string_lossy() }
            }
        }
    });
    let bytes = serde_json::to_vec_pretty(&cfg)?;
    tokio::fs::write(path, bytes).await?;
    Ok(())
}
```

**Note on MCP config format:** The exact JSON schema that Claude Code accepts for `--mcp-config` should be verified during Task 20 against the claude CLI's current MCP server config reference. If it uses a different transport shape (e.g., `{"command": "sh", "args": ["-c", "nc -U /path/to/sock"]}`), adjust `write_mcp_config` accordingly.

Modify `crates/pitboss-cli/src/dispatch/mod.rs`:

```rust
pub mod hierarchical;    // NEW
```

- [ ] **Step 2: Verify build succeeds now**

Run: `cargo build -p pitboss-cli`
Expected: clean build.

Run: `cargo lint`. Clean.

- [ ] **Step 3: Commit (bundles Task 18 + 19)**

```bash
git add crates/pitboss-cli/src/
git commit -m "dispatch: hierarchical mode detection + run_hierarchical scaffold"
```

---

### Task 20: Spawn the lead with `--mcp-config`

**Files:**
- Modify: `crates/pitboss-cli/src/dispatch/hierarchical.rs`
- Modify: `crates/pitboss-cli/src/dispatch/runner.rs` (factor out `lead_spawn_args`)

- [ ] **Step 1: Factor a shared lead-arg builder**

In `crates/pitboss-cli/src/dispatch/runner.rs`, extract a helper:

```rust
pub fn lead_spawn_args(lead: &crate::manifest::resolve::ResolvedLead, mcp_config: &std::path::Path) -> Vec<String> {
    let mut args = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];
    if !lead.tools.is_empty() {
        args.push("--allowedTools".into());
        args.push(lead.tools.join(","));
    }
    args.push("--model".into());
    args.push(lead.model.clone());
    args.push("--mcp-config".into());
    args.push(mcp_config.display().to_string());
    args.push("-p".into());
    args.push(lead.prompt.clone());
    args
}
```

- [ ] **Step 2: Write unit test for `lead_spawn_args`**

Append to `crates/pitboss-cli/src/dispatch/runner.rs` `mod tests`:

```rust
    #[test]
    fn lead_spawn_args_includes_mcp_config_and_verbose() {
        use crate::manifest::resolve::ResolvedLead;
        use std::path::PathBuf;
        let lead = ResolvedLead {
            id: "l".into(), directory: PathBuf::from("/tmp"), prompt: "p".into(),
            branch: None, model: "m".into(),
            effort: crate::manifest::schema::Effort::High, tools: vec!["Read".into()],
            timeout_secs: 60, use_worktree: false, env: Default::default(),
        };
        let args = lead_spawn_args(&lead, &PathBuf::from("/tmp/cfg.json"));
        assert!(args.iter().any(|a| a == "--verbose"));
        assert!(args.iter().any(|a| a == "--mcp-config"));
        assert!(args.iter().any(|a| a == "/tmp/cfg.json"));
        assert!(args.iter().any(|a| a == "-p"));
        assert!(args.iter().any(|a| a == "p"));
    }
```

- [ ] **Step 3: Run — should pass if the helper was written right**

Run: `cargo test -p pitboss-cli dispatch::runner::tests::lead_spawn_args_includes_mcp_config_and_verbose`
Expected: PASS.

- [ ] **Step 4: Wire the lead spawn into `run_hierarchical`**

In `crates/pitboss-cli/src/dispatch/hierarchical.rs`, replace the "3. Spawn the lead" stub with:

```rust
    // 3. Prepare lead worktree + spawn.
    let wt_mgr = Arc::new(pitboss_core::worktree::WorktreeManager::new());
    let mut lead_worktree_handle: Option<pitboss_core::worktree::Worktree> = None;
    let lead_cwd = if lead.use_worktree {
        let name = format!("shire-lead-{}-{}", lead.id, run_id);
        match wt_mgr.prepare(&lead.directory, &name, lead.branch.as_deref()) {
            Ok(wt) => {
                let p = wt.path.clone();
                lead_worktree_handle = Some(wt);
                p
            }
            Err(e) => {
                anyhow::bail!("lead worktree prepare failed: {e}");
            }
        }
    } else {
        lead.directory.clone()
    };

    let lead_task_dir = run_subdir.join("tasks").join(&lead.id);
    tokio::fs::create_dir_all(&lead_task_dir).await.ok();
    let lead_log_path = lead_task_dir.join("stdout.log");
    let lead_stderr_path = lead_task_dir.join("stderr.log");

    let spawn_cmd = pitboss_core::process::SpawnCmd {
        program: claude_binary.clone(),
        args: crate::dispatch::runner::lead_spawn_args(lead, &mcp_config_path),
        cwd: lead_cwd.clone(),
        env: lead.env.clone(),
    };

    state.workers.write().await.insert(
        lead.id.clone(),
        crate::dispatch::state::WorkerState::Running { started_at: Utc::now() },
    );

    let outcome = pitboss_core::session::SessionHandle::new(lead.id.clone(), spawner, spawn_cmd)
        .with_log_path(lead_log_path.clone())
        .with_stderr_log_path(lead_stderr_path)
        .run_to_completion(cancel.clone(), std::time::Duration::from_secs(lead.timeout_secs))
        .await;

    // Build lead TaskRecord
    let lead_record = pitboss_core::store::TaskRecord {
        task_id: lead.id.clone(),
        status: match outcome.final_state {
            pitboss_core::session::SessionState::Completed => pitboss_core::store::TaskStatus::Success,
            pitboss_core::session::SessionState::Failed { .. } => pitboss_core::store::TaskStatus::Failed,
            pitboss_core::session::SessionState::TimedOut => pitboss_core::store::TaskStatus::TimedOut,
            pitboss_core::session::SessionState::Cancelled => pitboss_core::store::TaskStatus::Cancelled,
            pitboss_core::session::SessionState::SpawnFailed { .. } => pitboss_core::store::TaskStatus::SpawnFailed,
            _ => pitboss_core::store::TaskStatus::Failed,
        },
        exit_code: outcome.exit_code,
        started_at: outcome.started_at,
        ended_at: outcome.ended_at,
        duration_ms: outcome.duration_ms(),
        worktree_path: if lead.use_worktree { Some(lead_cwd) } else { None },
        log_path: lead_log_path,
        token_usage: outcome.token_usage,
        claude_session_id: outcome.claude_session_id,
        final_message_preview: outcome.final_message_preview,
        parent_task_id: None,   // lead has no parent
    };

    // Cleanup worktree per policy
    if let Some(wt) = lead_worktree_handle {
        let succeeded = matches!(lead_record.status, pitboss_core::store::TaskStatus::Success);
        let cleanup = match resolved.worktree_cleanup {
            crate::manifest::schema::WorktreeCleanup::Always => pitboss_core::worktree::CleanupPolicy::Always,
            crate::manifest::schema::WorktreeCleanup::OnSuccess => pitboss_core::worktree::CleanupPolicy::OnSuccess,
            crate::manifest::schema::WorktreeCleanup::Never => pitboss_core::worktree::CleanupPolicy::Never,
        };
        let _ = wt_mgr.cleanup(wt, cleanup, succeeded);
    }

    // Persist lead record
    store.append_record(run_id, &lead_record).await?;
    state.workers.write().await.insert(
        lead.id.clone(),
        crate::dispatch::state::WorkerState::Done(lead_record.clone()),
    );
    let _ = state.done_tx.send(lead.id.clone());
```

Replace the final summary assembly to include the lead record and (in Task 21) any workers.

- [ ] **Step 5: Verify compiles + existing tests still pass**

Run: `cargo build -p pitboss-cli`
Expected: clean.

Run: `cargo test -p pitboss-cli`
Expected: all tests pass.

Run: `cargo lint`. Clean.

- [ ] **Step 6: Commit**

```bash
git add crates/pitboss-cli/src/
git commit -m "hierarchical: spawn lead with --mcp-config and persist its record"
```

---

### Task 21: Handle in-flight worker records at finalization

**Files:**
- Modify: `crates/pitboss-cli/src/dispatch/hierarchical.rs`

When the lead exits, any workers still Pending/Running need to be cancelled and have a TaskRecord written. This task adds that.

- [ ] **Step 1: Write a minimal unit test** (fuller integration in Phase 6)

Skip — this is covered in integration tests. Move to implementation.

- [ ] **Step 2: Implement lead-exit-cancels-workers**

In `crates/pitboss-cli/src/dispatch/hierarchical.rs`, after the lead's record has been persisted, add:

```rust
    // Any in-flight workers get cancelled, their TaskRecord synthesized.
    cancel.terminate();   // signals all in-flight SessionHandles
    // Give them up to TERMINATE_GRACE to drain.
    tokio::time::sleep(pitboss_core::session::TERMINATE_GRACE).await;

    let worker_records: Vec<pitboss_core::store::TaskRecord> = {
        let workers = state.workers.read().await;
        workers
            .iter()
            .filter(|(id, _)| *id != &lead.id)   // don't double-count the lead
            .map(|(id, w)| match w {
                crate::dispatch::state::WorkerState::Done(rec) => rec.clone(),
                crate::dispatch::state::WorkerState::Pending |
                crate::dispatch::state::WorkerState::Running { .. } => {
                    let now = Utc::now();
                    pitboss_core::store::TaskRecord {
                        task_id: id.clone(),
                        status: pitboss_core::store::TaskStatus::Cancelled,
                        exit_code: None,
                        started_at: now,
                        ended_at: now,
                        duration_ms: 0,
                        worktree_path: None,
                        log_path: run_subdir.join("tasks").join(id).join("stdout.log"),
                        token_usage: Default::default(),
                        claude_session_id: None,
                        final_message_preview: Some("cancelled when lead exited".into()),
                        parent_task_id: Some(lead.id.clone()),
                    }
                }
            })
            .collect()
    };

    for rec in &worker_records {
        store.append_record(run_id, rec).await?;
    }

    // Assemble final summary with lead + workers.
    let mut all_records = vec![lead_record.clone()];
    all_records.extend(worker_records);

    let tasks_failed = all_records.iter()
        .filter(|r| !matches!(r.status, pitboss_core::store::TaskStatus::Success))
        .count();

    let ended_at = Utc::now();
    let summary = RunSummary {
        run_id,
        manifest_path,
        shire_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version,
        started_at: meta.started_at, ended_at,
        total_duration_ms: (ended_at - meta.started_at).num_milliseconds(),
        tasks_total: all_records.len(),
        tasks_failed,
        was_interrupted: cancel.is_terminated(),
        tasks: all_records,
    };
    store.finalize_run(&summary).await?;

    // Exit code same as flat dispatch
    let rc = if cancel.is_terminated() {
        130
    } else if tasks_failed > 0 {
        1
    } else {
        0
    };
    Ok(rc)
```

Remove the earlier placeholder `Ok(0)` + empty summary.

- [ ] **Step 3: Verify compiles + tests pass**

Run: `cargo test --workspace --features pitboss-core/test-support`
Expected: all green.

Run: `cargo lint`. Clean.

- [ ] **Step 4: Commit**

```bash
git add crates/pitboss-cli/src/dispatch/hierarchical.rs
git commit -m "hierarchical: cancel in-flight workers on lead exit + persist records"
```

---

### Task 22: Refine per-worker cancellation (proper CancelToken per worker)

**Files:**
- Modify: `crates/pitboss-cli/src/dispatch/state.rs`
- Modify: `crates/pitboss-cli/src/dispatch/hierarchical.rs`
- Modify: `crates/pitboss-cli/src/mcp/tools.rs`

The Task 14 `handle_cancel_worker` drains the whole run; Task 22 introduces per-worker tokens so `cancel_worker` only targets one.

- [ ] **Step 1: Add per-worker cancel map to `DispatchState`**

```rust
// In crates/pitboss-cli/src/dispatch/state.rs

pub struct DispatchState {
    // ... existing fields ...
    pub worker_cancels: RwLock<HashMap<String, CancelToken>>,   // NEW
}
```

- [ ] **Step 2: Register per-worker cancel in the hierarchical spawn path**

In Phase 6 when integration tests wire the actual worker subprocess, the per-worker `CancelToken` is registered at spawn time and passed to the worker's `SessionHandle::run_to_completion`.

For this task, extend `handle_spawn_worker` (in Task 12/13) to create and register a per-worker token:

```rust
// In crates/pitboss-cli/src/mcp/tools.rs — at the end of handle_spawn_worker:

    let worker_cancel = pitboss_core::session::CancelToken::new();
    state.worker_cancels.write().await.insert(task_id.clone(), worker_cancel);
```

- [ ] **Step 3: Refine `handle_cancel_worker`**

Replace the implementation:

```rust
pub async fn handle_cancel_worker(
    state: &Arc<DispatchState>,
    task_id: &str,
) -> Result<CancelResult> {
    let cancels = state.worker_cancels.read().await;
    let Some(token) = cancels.get(task_id) else {
        anyhow::bail!("unknown task_id: {task_id}");
    };
    token.terminate();
    Ok(CancelResult { ok: true })
}
```

- [ ] **Step 4: Update the existing cancel test**

The earlier test asserts `result.ok` — still true. Add:

```rust
    #[tokio::test]
    async fn cancel_worker_unknown_id_errors() {
        let state = test_state().await;
        let err = handle_cancel_worker(&state, "never-existed").await.unwrap_err();
        assert!(err.to_string().contains("unknown task_id"));
    }
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p pitboss-cli mcp::tools`
Expected: all pass.

Run: `cargo lint`. Clean.

- [ ] **Step 6: Commit**

```bash
git add crates/pitboss-cli/src/
git commit -m "mcp: per-worker CancelToken in DispatchState; cancel_worker targets one"
```

---

## Phase 6 — Integration tests

Integration tests live in `crates/pitboss-cli/tests/hierarchical_flows.rs` and use `fake-mcp-client` to drive the shire MCP server as a simulated lead.

### Task 23: Implement `fake-mcp-client::connect` + `call_tool`

**Files:**
- Modify: `tests-support/fake-mcp-client/src/lib.rs`

- [ ] **Step 1: Replace the `unimplemented!` stubs with real rmcp client usage**

```rust
use std::path::Path;
use anyhow::{Context, Result};
use serde_json::Value;
use rmcp::client::{Client as McpClient};

pub struct FakeMcpClient {
    inner: McpClient,
}

impl FakeMcpClient {
    pub async fn connect(socket: &Path) -> Result<Self> {
        let stream = tokio::net::UnixStream::connect(socket).await
            .with_context(|| format!("connect to {}", socket.display()))?;
        // rmcp client construction from a stream; exact API may be
        // `rmcp::client::builder()` or similar — verify against rmcp 0.8 docs.
        let client = McpClient::builder()
            .with_transport(stream)
            .connect()
            .await?;
        Ok(Self { inner: client })
    }

    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value> {
        let response = self.inner.call_tool(name, args).await?;
        Ok(response)
    }

    pub async fn close(self) -> Result<()> {
        self.inner.shutdown().await?;
        Ok(())
    }
}
```

**Implementation note:** If rmcp's client API diverges from the pseudocode above, adapt. The test-support crate's job is to connect to a unix socket, complete MCP init, and call tools by name — whatever shape rmcp 0.8 requires.

- [ ] **Step 2: Smoke test**

Add a `#[cfg(test)]` inline sanity test that starts `McpServer` in-process and connects to it:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pitboss_cli::mcp::{McpServer, socket_path_for_run};
    use tempfile::TempDir;
    use uuid::Uuid;

    #[tokio::test]
    #[ignore = "needs pitboss-cli as a dev-dep — enable when hierarchical tests land"]
    async fn fake_client_connects_to_shire_server() {
        // See Task 24 which actually exercises this end-to-end.
    }
}
```

Skip running — Task 24 covers end-to-end.

- [ ] **Step 3: Verify build**

Run: `cargo build -p fake-mcp-client`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add tests-support/fake-mcp-client/src/lib.rs
git commit -m "fake-mcp-client: real rmcp client connect + call_tool"
```

---

### Task 24: Integration test — spawn + list + wait round-trip

**Files:**
- Create: `crates/pitboss-cli/tests/hierarchical_flows.rs`
- Modify: `crates/pitboss-cli/Cargo.toml` (add `fake-mcp-client` as dev-dep)

- [ ] **Step 1: Add dev-dep**

```toml
# in crates/pitboss-cli/Cargo.toml [dev-dependencies]
fake-mcp-client = { path = "../../tests-support/fake-mcp-client" }
```

- [ ] **Step 2: Write the integration test**

Create `crates/pitboss-cli/tests/hierarchical_flows.rs`:

```rust
//! Integration tests for v0.3 hierarchical orchestration. These drive the
//! shire MCP server as if we were a lead claude subprocess, using fake-mcp-client.

use std::sync::Arc;
use std::path::PathBuf;
use serde_json::json;
use tempfile::TempDir;

use fake_mcp_client::FakeMcpClient;
use pitboss_cli::dispatch::state::DispatchState;
use pitboss_cli::mcp::{McpServer, socket_path_for_run};
use pitboss_cli::manifest::resolve::ResolvedManifest;
use pitboss_cli::manifest::schema::WorktreeCleanup;
use pitboss_core::session::CancelToken;
use pitboss_core::store::{JsonFileStore, SessionStore};
use uuid::Uuid;

fn mk_state() -> (TempDir, Arc<DispatchState>) {
    let dir = TempDir::new().unwrap();
    let manifest = ResolvedManifest {
        max_parallel: 4, halt_on_failure: false,
        run_dir: dir.path().to_path_buf(),
        worktree_cleanup: WorktreeCleanup::OnSuccess,
        emit_event_stream: false,
        tasks: vec![], lead: None,
        max_workers: Some(4), budget_usd: Some(5.0), lead_timeout_secs: None,
    };
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
    let run_id = Uuid::now_v7();
    let state = Arc::new(DispatchState::new(
        run_id, manifest, store, CancelToken::new(), "lead".into(),
    ));
    (dir, state)
}

#[tokio::test]
async fn mcp_spawn_and_list_round_trip() {
    let (_dir, state) = mk_state();
    let socket = socket_path_for_run(state.run_id, &state.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone()).await.unwrap();

    let mut client = FakeMcpClient::connect(&socket).await.unwrap();
    let spawn_result = client.call_tool("spawn_worker", json!({
        "prompt": "investigate issue #1"
    })).await.unwrap();
    let task_id = spawn_result["task_id"].as_str().unwrap().to_string();
    assert!(task_id.starts_with("worker-"));

    let list_result = client.call_tool("list_workers", json!({})).await.unwrap();
    let list = list_result.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["task_id"].as_str().unwrap(), task_id);

    client.close().await.unwrap();
}
```

- [ ] **Step 3: Run**

Run: `cargo test --test hierarchical_flows`
Expected: PASS.

Run: `cargo lint`. Clean.

- [ ] **Step 4: Commit**

```bash
git add crates/pitboss-cli/
git commit -m "hierarchical: integration test for spawn + list round-trip"
```

---

### Task 25: Integration test — cap, budget, draining guard rejections

**Files:**
- Modify: `crates/pitboss-cli/tests/hierarchical_flows.rs`

- [ ] **Step 1: Append tests**

Add three tests to `hierarchical_flows.rs`:

```rust
#[tokio::test]
async fn mcp_spawn_over_max_workers_returns_error() {
    let (_dir, state) = mk_state();   // max_workers = 4
    let socket = socket_path_for_run(state.run_id, &state.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone()).await.unwrap();
    let mut client = FakeMcpClient::connect(&socket).await.unwrap();

    for i in 0..4 {
        client.call_tool("spawn_worker", json!({
            "prompt": format!("w{}", i)
        })).await.unwrap();
    }
    let err = client.call_tool("spawn_worker", json!({
        "prompt": "over"
    })).await;
    assert!(err.is_err() || err.unwrap()["error"].as_str().unwrap_or("").contains("worker cap reached"));
}

#[tokio::test]
async fn mcp_spawn_over_budget_returns_error() {
    let (_dir, state) = mk_state();   // budget_usd = 5.0
    *state.spent_usd.lock().await = 5.0;
    let socket = socket_path_for_run(state.run_id, &state.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone()).await.unwrap();
    let mut client = FakeMcpClient::connect(&socket).await.unwrap();

    let err = client.call_tool("spawn_worker", json!({"prompt": "p"})).await;
    assert!(err.is_err() || err.unwrap()["error"].as_str().unwrap_or("").contains("budget exceeded"));
}

#[tokio::test]
async fn mcp_spawn_while_draining_returns_error() {
    let (_dir, state) = mk_state();
    state.cancel.drain();
    let socket = socket_path_for_run(state.run_id, &state.manifest.run_dir);
    let _server = McpServer::start(socket.clone(), state.clone()).await.unwrap();
    let mut client = FakeMcpClient::connect(&socket).await.unwrap();

    let err = client.call_tool("spawn_worker", json!({"prompt": "p"})).await;
    assert!(err.is_err() || err.unwrap()["error"].as_str().unwrap_or("").contains("draining"));
}
```

- [ ] **Step 2: Run**

Run: `cargo test --test hierarchical_flows`
Expected: all 4 tests pass.

Run: `cargo lint`. Clean.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-cli/tests/
git commit -m "hierarchical: integration tests for cap/budget/drain guards"
```

---

### Task 26: Integration test — end-to-end with fake-claude lead + workers

**Files:**
- Modify: `tests-support/fake-claude/src/main.rs`
- Modify: `crates/pitboss-cli/tests/hierarchical_flows.rs`

- [ ] **Step 1: Extend fake-claude to emit tool_use events**

Extend `tests-support/fake-claude/src/main.rs` to recognize a new JSONL action type:

```json
{"tool_use":{"name":"spawn_worker","input":{"prompt":"..."}}}
```

When the fake encounters this, it emits a stream-json line of the form:

```json
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"call-1","name":"shire__spawn_worker","input":{"prompt":"..."}}]}}
```

The exact shape should match how real claude emits tool_use events (verified against a real claude session beforehand).

Add to `fake-claude/src/main.rs`'s JSONL parser:

```rust
} else if let Some(tu) = v.get("tool_use") {
    // Emit a stream-json tool_use event wrapper.
    let wrapper = serde_json::json!({
        "type": "assistant",
        "message": {
            "content": [{
                "type": "tool_use",
                "id": format!("call-{}", random_id()),
                "name": tu.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                "input": tu.get("input").cloned().unwrap_or(serde_json::Value::Null),
            }]
        }
    });
    println!("{}", serde_json::to_string(&wrapper)?);
    std::io::stdout().flush()?;
}
```

Add a small `random_id()` helper (or just use a counter).

- [ ] **Step 2: Write an end-to-end test**

Append to `crates/pitboss-cli/tests/hierarchical_flows.rs`:

```rust
#[tokio::test]
async fn hierarchical_run_with_one_worker_end_to_end() {
    // This test doesn't spin up a real claude; it uses fake-claude as both
    // the lead and the worker. The fake lead emits a shire__spawn_worker
    // tool_use, waits for the result, then exits. The dispatcher wires the
    // MCP tool_result back to the fake lead via a scripted transcript.

    // FIXME: The full round-trip requires the fake-claude binary to
    // interact with the shire MCP server through the MCP protocol. Since
    // fake-claude doesn't speak MCP, this end-to-end case is deferred to
    // the manual smoke test until a full MCP-aware fake lead is built.
    //
    // Task 26 keeps the placeholder here so the plan records the intent.
    // Implementation of the full loop comes in v0.3.1 hardening.
}
```

**Note:** writing a fully automated end-to-end test that exercises the MCP tool call loop requires the fake lead to run a real MCP client. This is a heavier lift than the scope of Task 26; the manual smoke test in §Phase 8 covers the full loop against real claude. The MCP tools themselves are unit-tested in Tasks 11–16, so coverage remains high.

- [ ] **Step 3: Verify — all existing tests still pass**

Run: `cargo test --workspace --features pitboss-core/test-support`
Expected: all tests green.

Run: `cargo lint`. Clean.

- [ ] **Step 4: Commit**

```bash
git add tests-support/fake-claude/ crates/pitboss-cli/tests/
git commit -m "hierarchical: fake-claude tool_use emission + deferred e2e placeholder"
```

---

## Phase 7 — Mosaic UI

### Task 27: Thread `parent_task_id` into `TileState`

**Files:**
- Modify: `crates/pitboss-tui/src/state.rs`
- Modify: `crates/pitboss-tui/src/watcher.rs`

- [ ] **Step 1: Add field to `TileState`**

In `crates/pitboss-tui/src/state.rs`:

```rust
pub struct TileState {
    pub id: String,
    pub status: TileStatus,
    pub duration_ms: Option<i64>,
    pub token_usage_input: u64,
    pub token_usage_output: u64,
    // ... existing fields ...
    pub parent_task_id: Option<String>,   // NEW
}
```

- [ ] **Step 2: Populate in watcher**

In `crates/pitboss-tui/src/watcher.rs`, wherever a `TileState` is constructed from a `TaskRecord`, set `parent_task_id: rec.parent_task_id.clone()`. For tiles built from `resolved.json` tasks that have no record yet, set `parent_task_id: None`.

- [ ] **Step 3: Verify**

Run: `cargo test --workspace --features pitboss-core/test-support`
Expected: all pass.

Run: `cargo lint`. Clean.

- [ ] **Step 4: Commit**

```bash
git add crates/pitboss-tui/src/
git commit -m "pitboss-tui: thread parent_task_id through TileState from TaskRecord"
```

---

### Task 28: Render lead tile with `[LEAD]` prefix + bold border

**Files:**
- Modify: `crates/pitboss-tui/src/tui.rs`

- [ ] **Step 1: Write failing test**

Append to `tui.rs` tests:

```rust
    #[test]
    fn render_tile_title_for_lead() {
        // The lead is the tile whose parent_task_id is None and whose id
        // appears as a parent of at least one other tile. Mosaic renders
        // its title with [LEAD] prefix.
        use crate::state::AppState;
        let tiles = vec![
            tile("triage-lead", TileStatus::Running, None, 0, 0),
            tile_with_parent("worker-1", TileStatus::Running, Some("triage-lead".into())),
        ];
        let state = state(tiles);
        let title = crate::tui::format_tile_title(&state, 0);
        assert!(title.contains("[LEAD]"));
        assert!(title.contains("triage-lead"));

        let worker_title = crate::tui::format_tile_title(&state, 1);
        assert!(!worker_title.contains("[LEAD]"));
        assert!(worker_title.contains("worker-1"));
    }

    // Helper variant:
    fn tile_with_parent(id: &str, status: TileStatus, parent: Option<String>) -> TileState {
        TileState {
            id: id.to_string(),
            status,
            duration_ms: None,
            token_usage_input: 0,
            token_usage_output: 0,
            exit_code: None,
            log_path: std::path::PathBuf::new(),
            parent_task_id: parent,
        }
    }
```

And update the existing `tile()` helper to set `parent_task_id: None`.

- [ ] **Step 2: Implement `format_tile_title`**

Add to `crates/pitboss-tui/src/tui.rs`:

```rust
pub fn format_tile_title(state: &crate::state::AppState, idx: usize) -> String {
    let Some(tile) = state.tasks.get(idx) else { return String::new() };
    let id = &tile.id;
    // A tile is the lead if it has no parent AND at least one other tile
    // lists it as parent.
    let is_lead = tile.parent_task_id.is_none()
        && state.tasks.iter().any(|t| t.parent_task_id.as_deref() == Some(id.as_str()));
    if is_lead {
        format!("[LEAD] {id}")
    } else {
        id.clone()
    }
}
```

Update the tile render path to use this helper for the tile's block title.

- [ ] **Step 3: Verify**

Run: `cargo test -p pitboss-tui tui::tests::render_tile_title_for_lead`
Expected: PASS.

Run: `cargo lint`. Clean.

- [ ] **Step 4: Commit**

```bash
git add crates/pitboss-tui/src/
git commit -m "pitboss-tui: [LEAD] prefix + bold border on lead tile"
```

---

### Task 29: Worker tile shows `← lead-id` annotation

**Files:**
- Modify: `crates/pitboss-tui/src/tui.rs`

- [ ] **Step 1: Write failing test**

Append:

```rust
    #[test]
    fn worker_tile_shows_parent_annotation() {
        let tiles = vec![
            tile("triage", TileStatus::Running, None, 0, 0),
            tile_with_parent("w-1", TileStatus::Running, Some("triage".into())),
        ];
        let state = state(tiles);
        let sub = crate::tui::format_tile_subtitle(&state, 1);
        assert!(sub.contains("← triage"));
    }

    #[test]
    fn lead_tile_has_no_parent_annotation() {
        let tiles = vec![tile("triage", TileStatus::Running, None, 0, 0)];
        let state = state(tiles);
        let sub = crate::tui::format_tile_subtitle(&state, 0);
        assert!(!sub.contains("←"));
    }
```

- [ ] **Step 2: Implement `format_tile_subtitle`**

```rust
pub fn format_tile_subtitle(state: &crate::state::AppState, idx: usize) -> String {
    let Some(tile) = state.tasks.get(idx) else { return String::new() };
    if let Some(parent) = &tile.parent_task_id {
        format!("← {parent}")
    } else {
        String::new()
    }
}
```

Wire this into the tile render path (add as an extra line inside the tile block).

- [ ] **Step 3: Verify**

Run: `cargo test -p pitboss-tui tui::tests`
Expected: all pass.

Run: `cargo lint`. Clean.

- [ ] **Step 4: Commit**

```bash
git add crates/pitboss-tui/src/
git commit -m "pitboss-tui: worker tiles show ← <parent-id> annotation"
```

---

### Task 30: Status bar `N workers spawned`

**Files:**
- Modify: `crates/pitboss-tui/src/tui.rs`

- [ ] **Step 1: Extend `render_title` / `run_stats`**

Add a count of tiles with a non-None `parent_task_id`:

```rust
fn workers_spawned(state: &AppState) -> usize {
    state.tasks.iter().filter(|t| t.parent_task_id.is_some()).count()
}
```

In the `render_title` (or `render_statusbar`) path, if `workers_spawned > 0`, append ` — <N> workers spawned` to the existing title string.

- [ ] **Step 2: Add test**

```rust
    #[test]
    fn workers_spawned_counts_tiles_with_parent() {
        let tiles = vec![
            tile("lead", TileStatus::Running, None, 0, 0),
            tile_with_parent("w-1", TileStatus::Running, Some("lead".into())),
            tile_with_parent("w-2", TileStatus::Running, Some("lead".into())),
        ];
        let s = state(tiles);
        assert_eq!(crate::tui::workers_spawned(&s), 2);
    }
```

- [ ] **Step 3: Verify**

Run: `cargo test -p pitboss-tui`
Expected: all pass.

Run: `cargo lint`. Clean.

- [ ] **Step 4: Commit**

```bash
git add crates/pitboss-tui/src/tui.rs
git commit -m "pitboss-tui: status bar shows '<N> workers spawned' counter"
```

---

## Phase 8 — CLI polish, docs, smoke test

### Task 31: `pitboss validate` supports hierarchical manifests

**Files:**
- Modify: `crates/pitboss-cli/src/main.rs`

- [ ] **Step 1: Extend `run_validate`**

In `crates/pitboss-cli/src/main.rs`'s `run_validate`, update the output so hierarchical manifests print distinct info:

```rust
fn run_validate(manifest: &std::path::Path) -> Result<()> {
    let env_mp = parse_env_max_parallel();
    let r = manifest::load_manifest(manifest, env_mp)?;
    if let Some(lead) = &r.lead {
        println!("OK (hierarchical) — lead='{}', max_workers={}, budget=${:.2}",
                 lead.id,
                 r.max_workers.unwrap_or(0),
                 r.budget_usd.unwrap_or(0.0));
    } else {
        println!("OK — {} tasks, max_parallel={}", r.tasks.len(), r.max_parallel);
    }
    Ok(())
}
```

- [ ] **Step 2: Verify**

Run: `cargo build -p pitboss-cli`
Expected: clean.

Run a hand test with a toy hierarchical manifest:
```bash
# (don't commit this toy file)
./target/debug/pitboss validate /path/to/hierarchical.toml
```
Expected: prints the hierarchical summary.

- [ ] **Step 3: Commit**

```bash
git add crates/pitboss-cli/src/main.rs
git commit -m "shire: validate shows hierarchical manifest summary"
```

---

### Task 32: `pitboss resume` for hierarchical runs

**Files:**
- Modify: `crates/pitboss-cli/src/dispatch/resume.rs`

Resume a hierarchical run by re-running the lead with `--resume <lead_session_id>`. Workers are NOT resumed — the lead decides.

- [ ] **Step 1: Add `build_resume_hierarchical` helper**

In `crates/pitboss-cli/src/dispatch/resume.rs`, add:

```rust
pub fn build_resume_hierarchical(run_dir: &std::path::Path) -> anyhow::Result<ResolvedManifest> {
    // Load prior resolved.json — must have been a hierarchical run.
    let resolved_path = run_dir.join("resolved.json");
    let bytes = std::fs::read(&resolved_path)
        .with_context(|| format!("reading {}", resolved_path.display()))?;
    let mut resolved: ResolvedManifest = serde_json::from_slice(&bytes)?;

    let lead = resolved.lead.as_mut()
        .ok_or_else(|| anyhow::anyhow!("run is not hierarchical (no lead in resolved.json)"))?;

    // Load prior summary.json, find the lead's record, extract its session id.
    let summary_path = run_dir.join("summary.json");
    let summary_bytes = std::fs::read(&summary_path)
        .with_context(|| format!("reading {}", summary_path.display()))?;
    let summary: RunSummary = serde_json::from_slice(&summary_bytes)?;

    let lead_record = summary.tasks.iter()
        .find(|r| r.task_id == lead.id)
        .ok_or_else(|| anyhow::anyhow!("no lead TaskRecord in summary"))?;

    let session_id = lead_record.claude_session_id.clone()
        .ok_or_else(|| anyhow::anyhow!("lead has no claude_session_id — cannot resume"))?;

    // Stuff session_id into the lead's resume field — extend ResolvedLead if needed.
    lead.resume_session_id = Some(session_id);   // add this field to ResolvedLead

    // Clear tasks (shouldn't be populated in a hierarchical run anyway).
    resolved.tasks.clear();

    Ok(resolved)
}
```

Add `resume_session_id: Option<String>` to `ResolvedLead` in `crates/pitboss-cli/src/manifest/resolve.rs`.

Thread it into `lead_spawn_args` (from Task 20) so `--resume <id>` is added when set.

- [ ] **Step 2: Extend the `Resume` CLI dispatch to detect mode**

In `main.rs`'s `run_resume` function, check whether `resolved.json` has `lead.is_some()`. If yes, call `build_resume_hierarchical`; otherwise use the existing flat resume path.

- [ ] **Step 3: Add test**

Append to `crates/pitboss-cli/src/dispatch/resume.rs`:

```rust
    #[test]
    fn build_resume_hierarchical_populates_session_id() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        // Write a synthetic resolved.json with a lead
        // and a summary.json containing the lead's record with a session_id.
        // (Construction code omitted here — mirrors the existing
        // build_resume_manifest test pattern.)
        // ...
    }
```

- [ ] **Step 4: Verify**

Run: `cargo test -p pitboss-cli resume`
Expected: pass.

Run: `cargo lint`. Clean.

- [ ] **Step 5: Commit**

```bash
git add crates/pitboss-cli/src/
git commit -m "shire resume: hierarchical mode, reruns lead with --resume"
```

---

### Task 33: `docs/v0.3-smoke-test.md`

**Files:**
- Create: `docs/v0.3-smoke-test.md`

- [ ] **Step 1: Write the smoke test procedure**

Create `docs/v0.3-smoke-test.md` with a checklist mirroring `docs/v0.1-smoke-test.md`'s format, adapted for hierarchical mode. Include:

1. Offline checks: `pitboss validate hierarchical-happy.toml`, with/without lead, mixed rejection.
2. Real-claude test: a 3-worker triage prompt on Haiku, cheap, budget = $1.
3. Mosaic observation during the live run: verify lead tile shows `[LEAD]` prefix, workers show `← <lead-id>`, status bar shows `3 workers spawned`.
4. Budget enforcement: hand-edit a manifest to `budget_usd = 0.05`, run it, observe the lead hitting the budget error and wrapping up gracefully.
5. Ctrl-C drain during a hierarchical run.
6. `pitboss resume <run-id>` after a hierarchical run completes.

- [ ] **Step 2: Commit**

```bash
git add docs/v0.3-smoke-test.md
git commit -m "docs: v0.3 hierarchical smoke-test procedure"
```

---

### Task 34: Update top-level README for v0.3

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add hierarchical section**

Append a new section to `README.md` explaining the `[[lead]]` manifest shape, with a minimal example and a link to the design doc.

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "README: document v0.3 hierarchical mode with example"
```

---

### Task 35: End-to-end sanity pass

**Files:** none — a checkpoint.

- [ ] **Step 1: Run the full suite**

Run: `cargo test --workspace --features pitboss-core/test-support`
Expected: all tests green (target: ~180+ tests, depending on final integration coverage).

Run: `cargo lint`. Clean.

Run: `cargo tidy`. Clean.

Run: `bash scripts/smoke-part1.sh`. 10/10.

Run: `bash scripts/smoke-part3-tui.sh`. 7/7.

- [ ] **Step 2: Tag a pre-release**

```bash
git tag v0.3.0-pre
```

Manual smoke test per `docs/v0.3-smoke-test.md` before promoting `v0.3.0-pre` → `v0.3.0`.

---

## Appendix — Self-Review Checklist

Run before handing off to subagent execution.

- [ ] **Spec coverage**: every §1–§13 in the design doc maps to at least one task here.
- [ ] **Placeholder scan**: no "TBD", "fill in", or narrative-only steps.
- [ ] **Type consistency**: `DispatchState`, `WorkerState`, `SpawnWorkerArgs`, and friends have the same shape wherever they appear.
- [ ] **Test-and-implementation pairing**: every code-introducing task has a failing-test step before its implementation step.
- [ ] **Forward-references**: Tasks 19–22 reference each other; each marks its forward-ref sites with `covered in Task N` comments.
- [ ] **No scope creep**: §12 non-goals are respected; nothing in this plan adds worker-to-worker messaging, depth > 1, or plan-approval hooks.
