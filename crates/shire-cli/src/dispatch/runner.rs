#![allow(clippy::large_futures, clippy::needless_pass_by_value)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use mosaic_core::process::{ProcessSpawner, SpawnCmd, TokioSpawner};
use mosaic_core::session::{CancelToken, SessionHandle};
use mosaic_core::store::{
    JsonFileStore, RunMeta, RunSummary, SessionStore, TaskRecord, TaskStatus,
};
use mosaic_core::worktree::{CleanupPolicy, WorktreeManager};
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

use crate::manifest::resolve::{ResolvedManifest, ResolvedTask};

/// Public entry — main.rs calls this. Constructs production spawner + store.
pub async fn run_dispatch_inner(
    resolved: ResolvedManifest,
    manifest_text: String,
    manifest_path: PathBuf,
    claude_binary: PathBuf,
    claude_version: Option<String>,
    run_dir_override: Option<PathBuf>,
    dry_run: bool,
) -> Result<i32> {
    let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
    let run_dir = run_dir_override.unwrap_or_else(|| resolved.run_dir.clone());
    tokio::fs::create_dir_all(&run_dir).await.ok();
    let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(run_dir.clone()));

    execute(
        resolved,
        manifest_text,
        manifest_path,
        claude_binary,
        claude_version,
        spawner,
        store,
        dry_run,
    )
    .await
}

/// Inner workhorse — takes its dependencies injected for testability.
#[allow(clippy::too_many_arguments)]
pub async fn execute(
    resolved: ResolvedManifest,
    manifest_text: String,
    manifest_path: PathBuf,
    claude_binary: PathBuf,
    claude_version: Option<String>,
    spawner: Arc<dyn ProcessSpawner>,
    store: Arc<dyn SessionStore>,
    dry_run: bool,
) -> Result<i32> {
    let run_id = Uuid::now_v7();

    let run_dir = resolved.run_dir.clone();
    let run_subdir = run_dir.join(run_id.to_string());
    tokio::fs::create_dir_all(&run_subdir).await.ok();
    tokio::fs::write(run_subdir.join("manifest.snapshot.toml"), &manifest_text).await?;
    if let Ok(b) = serde_json::to_vec_pretty(&resolved) {
        tokio::fs::write(run_subdir.join("resolved.json"), b).await?;
    }

    let meta = RunMeta {
        run_id,
        manifest_path: manifest_path.clone(),
        shire_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version: claude_version.clone(),
        started_at: Utc::now(),
        env: Default::default(),
    };
    store.init_run(&meta).await.context("init run")?;

    if dry_run {
        for t in &resolved.tasks {
            println!(
                "DRY-RUN {}: {} {}",
                t.id,
                claude_binary.display(),
                spawn_args(t).join(" ")
            );
        }
        return Ok(0);
    }

    let is_tty = atty::is(atty::Stream::Stdout);
    let table = Arc::new(Mutex::new(crate::tui_table::ProgressTable::new(is_tty)));
    for t in &resolved.tasks {
        table.lock().await.register(&t.id);
    }

    let semaphore = Arc::new(Semaphore::new(resolved.max_parallel as usize));
    let cancel = CancelToken::new();
    crate::dispatch::signals::install_ctrl_c_watcher(cancel.clone());
    let wt_mgr = Arc::new(WorktreeManager::new());
    let records: Arc<Mutex<Vec<TaskRecord>>> = Arc::new(Mutex::new(Vec::new()));

    let mut handles = Vec::new();

    for task in resolved.tasks.clone() {
        if cancel.is_draining() {
            break;
        }
        let permit = semaphore.clone().acquire_owned().await?;
        // Re-check after potentially blocking on the semaphore.
        if cancel.is_draining() {
            break;
        }
        let spawner = spawner.clone();
        let store = store.clone();
        let cancel = cancel.clone();
        let claude = claude_binary.clone();
        let records = records.clone();
        let wt_mgr = wt_mgr.clone();
        let halt_on_failure = resolved.halt_on_failure;
        let run_dir = resolved.run_dir.clone();
        let cleanup_policy = match resolved.worktree_cleanup {
            crate::manifest::schema::WorktreeCleanup::Always => CleanupPolicy::Always,
            crate::manifest::schema::WorktreeCleanup::OnSuccess => CleanupPolicy::OnSuccess,
            crate::manifest::schema::WorktreeCleanup::Never => CleanupPolicy::Never,
        };
        let table = table.clone();

        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let record = execute_task(
                &task,
                &claude,
                spawner,
                store.clone(),
                cancel.clone(),
                wt_mgr,
                cleanup_policy,
                run_id,
                run_dir,
                table.clone(),
            )
            .await;
            let failed = !matches!(record.status, TaskStatus::Success);
            table.lock().await.mark_done(&record);
            records.lock().await.push(record);
            if failed && halt_on_failure {
                cancel.drain();
            }
        }));
    }

    for h in handles {
        let _ = h.await;
    }

    let records = Arc::try_unwrap(records)
        .map_err(|_| anyhow::anyhow!("records locked"))?
        .into_inner();
    let tasks_failed = records
        .iter()
        .filter(|r| !matches!(r.status, TaskStatus::Success))
        .count();

    let started_at = meta.started_at;
    let ended_at = Utc::now();
    let summary = RunSummary {
        run_id,
        manifest_path: manifest_path.clone(),
        shire_version: env!("CARGO_PKG_VERSION").to_string(),
        claude_version: claude_version.clone(),
        started_at,
        ended_at,
        total_duration_ms: (ended_at - started_at).num_milliseconds(),
        tasks_total: records.len(),
        tasks_failed,
        was_interrupted: cancel.is_draining() || cancel.is_terminated(),
        tasks: records,
    };
    store.finalize_run(&summary).await?;

    let rc = if cancel.is_terminated() {
        130
    } else if tasks_failed > 0 {
        1
    } else {
        0
    };
    Ok(rc)
}

#[allow(clippy::too_many_arguments)]
async fn execute_task(
    task: &ResolvedTask,
    claude: &Path,
    spawner: Arc<dyn ProcessSpawner>,
    _store: Arc<dyn SessionStore>,
    cancel: CancelToken,
    wt_mgr: Arc<WorktreeManager>,
    cleanup: CleanupPolicy,
    run_id: Uuid,
    run_dir: PathBuf,
    table: Arc<Mutex<crate::tui_table::ProgressTable>>,
) -> TaskRecord {
    let task_dir = run_dir
        .join(run_id.to_string())
        .join("tasks")
        .join(&task.id);
    tokio::fs::create_dir_all(&task_dir).await.ok();
    let log_path = task_dir.join("stdout.log");

    // Worktree preparation (optional).
    let mut worktree_handle = None;
    let cwd = if task.use_worktree {
        let name = format!("shire-{}-{}", task.id, run_id);
        match wt_mgr.prepare(&task.directory, &name, task.branch.as_deref()) {
            Ok(wt) => {
                let p = wt.path.clone();
                worktree_handle = Some(wt);
                p
            }
            Err(e) => {
                return TaskRecord {
                    task_id: task.id.clone(),
                    status: TaskStatus::SpawnFailed,
                    exit_code: None,
                    started_at: Utc::now(),
                    ended_at: Utc::now(),
                    duration_ms: 0,
                    worktree_path: None,
                    log_path,
                    token_usage: Default::default(),
                    claude_session_id: None,
                    final_message_preview: Some(format!("worktree error: {e}")),
                };
            }
        }
    } else {
        task.directory.clone()
    };

    let cmd = SpawnCmd {
        program: claude.to_path_buf(),
        args: spawn_args(task),
        cwd: cwd.clone(),
        env: task.env.clone(),
    };

    table.lock().await.mark_running(&task.id);

    let outcome = SessionHandle::new(task.id.clone(), spawner, cmd)
        .with_log_path(log_path.clone())
        .run_to_completion(cancel, Duration::from_secs(task.timeout_secs))
        .await;

    let status = match outcome.final_state {
        mosaic_core::session::SessionState::Completed => TaskStatus::Success,
        mosaic_core::session::SessionState::Failed { .. } => TaskStatus::Failed,
        mosaic_core::session::SessionState::TimedOut => TaskStatus::TimedOut,
        mosaic_core::session::SessionState::Cancelled => TaskStatus::Cancelled,
        mosaic_core::session::SessionState::SpawnFailed { .. } => TaskStatus::SpawnFailed,
        _ => TaskStatus::Failed,
    };

    // Cleanup worktree.
    if let Some(wt) = worktree_handle {
        let succeeded = matches!(status, TaskStatus::Success);
        let _ = wt_mgr.cleanup(wt, cleanup, succeeded);
    }

    let worktree_path = if task.use_worktree { Some(cwd) } else { None };
    TaskRecord {
        task_id: task.id.clone(),
        status,
        exit_code: outcome.exit_code,
        started_at: outcome.started_at,
        ended_at: outcome.ended_at,
        duration_ms: outcome.duration_ms(),
        worktree_path,
        log_path,
        token_usage: outcome.token_usage,
        claude_session_id: outcome.claude_session_id,
        final_message_preview: outcome.final_message_preview,
    }
}

fn spawn_args(task: &ResolvedTask) -> Vec<String> {
    // claude CLI requires --verbose when combining -p (print mode) with
    // --output-format stream-json. Without it, claude rejects the invocation
    // with "When using --print, --output-format=stream-json requires --verbose".
    let mut args = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
    ];
    if !task.tools.is_empty() {
        args.push("--allowedTools".into());
        args.push(task.tools.join(","));
    }
    args.push("--model".into());
    args.push(task.model.clone());
    args.push("-p".into());
    args.push(task.prompt.clone());
    args
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use mosaic_core::process::fake::{FakeScript, FakeSpawner};
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo(root: &std::path::Path) {
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "t@t.x"])
            .current_dir(root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(root)
            .status()
            .unwrap();
        std::fs::write(root.join("r"), "").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-q", "-m", "i"])
            .current_dir(root)
            .status()
            .unwrap();
    }

    #[tokio::test]
    async fn executes_three_tasks_with_mixed_outcomes() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let run_dir = TempDir::new().unwrap();

        let resolved = crate::manifest::resolve::ResolvedManifest {
            max_parallel: 2,
            halt_on_failure: false,
            run_dir: run_dir.path().to_path_buf(),
            worktree_cleanup: crate::manifest::schema::WorktreeCleanup::Always,
            emit_event_stream: false,
            tasks: vec![
                ResolvedTask {
                    id: "ok".into(),
                    directory: dir.path().to_path_buf(),
                    prompt: "p".into(),
                    branch: None,
                    model: "m".into(),
                    effort: crate::manifest::schema::Effort::High,
                    tools: vec![],
                    timeout_secs: 30,
                    use_worktree: false,
                    env: Default::default(),
                },
                ResolvedTask {
                    id: "bad".into(),
                    directory: dir.path().to_path_buf(),
                    prompt: "p".into(),
                    branch: None,
                    model: "m".into(),
                    effort: crate::manifest::schema::Effort::High,
                    tools: vec![],
                    timeout_secs: 30,
                    use_worktree: false,
                    env: Default::default(),
                },
            ],
        };

        // Script: first call succeeds, second call fails. FakeSpawner is single-shot,
        // so we use a cycling spawner.
        let spawner = Arc::new(CyclingFake(
            vec![
                FakeScript::new()
                    .stdout_line(r#"{"type":"result","session_id":"s1","usage":{"input_tokens":1,"output_tokens":2}}"#)
                    .exit_code(0),
                FakeScript::new()
                    .stdout_line(r#"{"type":"result","session_id":"s2","usage":{"input_tokens":1,"output_tokens":2}}"#)
                    .exit_code(5),
            ],
            std::sync::Mutex::new(0),
        ));

        let store = Arc::new(JsonFileStore::new(run_dir.path().to_path_buf()));
        let rc = execute(
            resolved,
            String::new(),
            PathBuf::new(),
            PathBuf::from("claude"),
            None,
            spawner,
            store.clone(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(rc, 1, "one failure → exit 1");
    }

    struct CyclingFake(Vec<FakeScript>, std::sync::Mutex<usize>);

    #[async_trait::async_trait]
    impl ProcessSpawner for CyclingFake {
        async fn spawn(
            &self,
            cmd: SpawnCmd,
        ) -> Result<Box<dyn mosaic_core::process::ChildProcess>, mosaic_core::error::SpawnError>
        {
            let i = {
                let mut lock = self.1.lock().unwrap();
                let i = *lock;
                *lock += 1;
                i
            };
            let script = self.0[i % self.0.len()].clone();
            FakeSpawner::new(script).spawn(cmd).await
        }
    }

    #[tokio::test]
    async fn halt_on_failure_drains_after_first_failure() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let run_dir = TempDir::new().unwrap();

        let make_task = |id: &str| ResolvedTask {
            id: id.into(),
            directory: dir.path().to_path_buf(),
            prompt: "p".into(),
            branch: None,
            model: "m".into(),
            effort: crate::manifest::schema::Effort::High,
            tools: vec![],
            timeout_secs: 30,
            use_worktree: false,
            env: Default::default(),
        };

        let resolved = crate::manifest::resolve::ResolvedManifest {
            max_parallel: 1, // serialize so ordering is deterministic
            halt_on_failure: true,
            run_dir: run_dir.path().to_path_buf(),
            worktree_cleanup: crate::manifest::schema::WorktreeCleanup::Always,
            emit_event_stream: false,
            tasks: vec![make_task("a"), make_task("b"), make_task("c")],
        };

        let spawner = Arc::new(CyclingFake(
            vec![
                FakeScript::new()
                    .stdout_line(r#"{"type":"result","session_id":"s","usage":{"input_tokens":0,"output_tokens":0}}"#)
                    .exit_code(7),   // fails → cascade
                FakeScript::new().exit_code(0),
                FakeScript::new().exit_code(0),
            ],
            std::sync::Mutex::new(0),
        ));
        let store = Arc::new(JsonFileStore::new(run_dir.path().to_path_buf()));
        let rc = execute(
            resolved,
            String::new(),
            PathBuf::new(),
            PathBuf::from("claude"),
            None,
            spawner,
            store.clone(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(rc, 1);

        // Expect only task "a" recorded; others were skipped by the drain.
        // summary.json should exist with tasks.len() == 1.
        let summary_path = run_dir
            .path()
            .join(store_run_id_dir(run_dir.path()))
            .join("summary.json");
        let bytes = std::fs::read(&summary_path).unwrap();
        let s: RunSummary = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(s.tasks.len(), 1);
    }

    fn store_run_id_dir(root: &std::path::Path) -> String {
        // Finds the single UUID-named subdir just created.
        for entry in std::fs::read_dir(root).unwrap() {
            let e = entry.unwrap();
            if e.path().is_dir() {
                return e.file_name().to_string_lossy().to_string();
            }
        }
        panic!("no run dir")
    }
}
