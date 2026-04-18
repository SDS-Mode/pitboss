//! Lifecycle of the pitboss MCP server (unix socket transport).

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo};
use rmcp::service::ServiceExt;
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler};

use crate::dispatch::state::DispatchState;
use crate::mcp::tools::{
    handle_cancel_worker, handle_continue_worker, handle_list_workers, handle_pause_worker,
    handle_reprompt_worker, handle_request_approval, handle_spawn_worker, handle_wait_for_any,
    handle_wait_for_worker, handle_worker_status, ContinueWorkerArgs, RepromptWorkerArgs,
    RequestApprovalArgs, SpawnWorkerArgs, TaskIdArgs, WaitForAnyArgs, WaitForWorkerArgs,
};

/// Compute the socket path for a given run. Falls back to the run_dir if
/// $XDG_RUNTIME_DIR is unset or non-writable.
pub fn socket_path_for_run(run_id: Uuid, run_dir: &Path) -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let p = PathBuf::from(xdg).join("pitboss");
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
    shutdown_tx: Option<oneshot::Sender<()>>,
    join_handle: Option<JoinHandle<()>>,
    tracker: TaskTracker,
    cancel: CancellationToken,
}

/// The rmcp `ServerHandler` that exposes the six pitboss tools to the lead
/// Hobbit via a per-connection MCP session.
#[derive(Clone)]
pub struct PitbossHandler {
    state: Arc<DispatchState>,
    tool_router: ToolRouter<Self>,
}

impl PitbossHandler {
    pub fn new(state: Arc<DispatchState>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl PitbossHandler {
    #[tool(description = "Spawn a worker Hobbit. Returns {task_id, worktree_path}.")]
    async fn spawn_worker(
        &self,
        Parameters(args): Parameters<SpawnWorkerArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_spawn_worker(&self.state, args).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(description = "Non-blocking status poll for a worker. Returns state + partial data.")]
    async fn worker_status(
        &self,
        Parameters(args): Parameters<TaskIdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_worker_status(&self.state, &args.task_id).await {
            Ok(status) => to_structured_result(&status),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(description = "Block until a specific worker exits (or timeout).")]
    async fn wait_for_worker(
        &self,
        Parameters(args): Parameters<WaitForWorkerArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_wait_for_worker(&self.state, &args.task_id, args.timeout_secs).await {
            Ok(rec) => to_structured_result(&rec),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(description = "Block until any of the listed workers exits.")]
    async fn wait_for_any(
        &self,
        Parameters(args): Parameters<WaitForAnyArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_wait_for_any(&self.state, &args.task_ids, args.timeout_secs).await {
            Ok((id, rec)) => {
                let value = serde_json::json!({
                    "task_id": id,
                    "record": rec,
                });
                Ok(CallToolResult::structured(value))
            }
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(description = "List all workers in the current run (excludes the lead).")]
    async fn list_workers(&self) -> Result<CallToolResult, ErrorData> {
        let summaries = handle_list_workers(&self.state).await;
        to_structured_result(&summaries)
    }

    #[tool(description = "Cancel a worker by task_id. Sends SIGTERM, grace, SIGKILL.")]
    async fn cancel_worker(
        &self,
        Parameters(args): Parameters<TaskIdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_cancel_worker(&self.state, &args.task_id).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(
        description = "Pause a running worker. Snapshots its session id so continue_worker can resume."
    )]
    async fn pause_worker(
        &self,
        Parameters(args): Parameters<TaskIdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_pause_worker(&self.state, &args.task_id).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(
        description = "Continue a previously-paused worker. Spawns claude --resume under the hood."
    )]
    async fn continue_worker(
        &self,
        Parameters(args): Parameters<ContinueWorkerArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_continue_worker(&self.state, args).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(
        description = "Reprompt a running or paused worker with a new prompt via claude --resume. Preserves the worker's claude session for context continuity."
    )]
    async fn reprompt_worker(
        &self,
        Parameters(args): Parameters<RepromptWorkerArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_reprompt_worker(&self.state, args).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }

    #[tool(
        description = "Request operator approval before proceeding. Blocks until operator responds or timeout."
    )]
    async fn request_approval(
        &self,
        Parameters(args): Parameters<RequestApprovalArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        match handle_request_approval(&self.state, args).await {
            Ok(res) => to_structured_result(&res),
            Err(e) => Err(ErrorData::invalid_request(e.to_string(), None)),
        }
    }
}

#[tool_handler]
impl ServerHandler for PitbossHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "pitboss".into(),
                title: None,
                version: env!("CARGO_PKG_VERSION").into(),
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "Pitboss MCP server: coordinate worker Hobbits via six structured tools.".into(),
            ),
            ..Default::default()
        }
    }
}

/// Serialize a value to `CallToolResult::structured(json)`. Used for the
/// structured JSON payloads our tools return. Serialization failures are
/// reported as internal errors.
fn to_structured_result<T: serde::Serialize>(value: &T) -> Result<CallToolResult, ErrorData> {
    let v = serde_json::to_value(value)
        .map_err(|e| ErrorData::internal_error(format!("serialize: {e}"), None))?;
    Ok(CallToolResult::structured(v))
}

impl McpServer {
    /// Start serving on the given socket path. Binds to the unix socket,
    /// spawns an accept loop in a dedicated tokio task, returns a handle.
    ///
    /// Each accepted connection gets its own rmcp `ServiceExt::serve` session
    /// backed by a cloned `PitbossHandler`. The shared `DispatchState` is held
    /// behind `Arc` so all sessions see the same run.
    pub async fn start(socket_path: PathBuf, state: Arc<DispatchState>) -> Result<Self> {
        // If the socket file already exists (stale), remove it.
        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
        let listener = tokio::net::UnixListener::bind(&socket_path)?;
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
        let handler = PitbossHandler::new(state);

        let tracker = TaskTracker::new();
        let cancel = CancellationToken::new();

        let tracker_outer = tracker.clone();
        let cancel_outer = cancel.clone();

        let join_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => break,
                    _ = cancel_outer.cancelled() => break,
                    accept = listener.accept() => {
                        match accept {
                            Ok((stream, _addr)) => {
                                let h = handler.clone();
                                let cancel_inner = cancel_outer.clone();
                                // Track the spawned session task so Drop can signal cancellation
                                // to per-connection tasks without waiting for MCP session timeouts.
                                tracker_outer.spawn(async move {
                                    tokio::select! {
                                        _ = cancel_inner.cancelled() => {}
                                        _ = async {
                                            match h.serve(stream).await {
                                                Ok(running) => {
                                                    if let Err(e) = running.waiting().await {
                                                        tracing::debug!("mcp session join error: {e}");
                                                    }
                                                }
                                                Err(e) => {
                                                    tracing::debug!("mcp session init error: {e}");
                                                }
                                            }
                                        } => {}
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::debug!("mcp accept error: {e}");
                            }
                        }
                    }
                }
            }
        });

        Ok(Self {
            socket_path,
            shutdown_tx: Some(shutdown_tx),
            join_handle: Some(join_handle),
            tracker,
            cancel,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        // Signal shutdown to the accept loop.
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        // Signal cancel to all per-connection tasks; they exit their select!
        // arms immediately rather than waiting for MCP session close / timeout.
        self.cancel.cancel();
        self.tracker.close();
        if let Some(h) = self.join_handle.take() {
            h.abort();
        }
        // Note: we can't `.await` tracker.wait() from a sync Drop. The
        // CancellationToken fires above let per-connection tasks exit quickly
        // without us blocking here. If a future async shutdown() method is
        // added, that would be the place to await the tracker.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::TempDir;
    use uuid::Uuid;

    // Serializes tests that mutate XDG_RUNTIME_DIR, since env vars are
    // process-global and cargo runs tests in parallel by default.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn socket_path_uses_xdg_runtime_dir_when_set() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("XDG_RUNTIME_DIR");
        let dir = TempDir::new().unwrap();
        let run_id = Uuid::now_v7();
        let p = socket_path_for_run(run_id, dir.path());
        assert!(p.starts_with(dir.path()));
    }

    #[tokio::test]
    async fn server_starts_and_accepts_connection() {
        use crate::dispatch::state::{ApprovalPolicy, DispatchState};
        use crate::manifest::resolve::ResolvedManifest;
        use crate::manifest::schema::WorktreeCleanup;
        use pitboss_core::process::{ProcessSpawner, TokioSpawner};
        use pitboss_core::session::CancelToken;
        use pitboss_core::store::{JsonFileStore, SessionStore};
        use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let run_id = Uuid::now_v7();
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
        let wt_mgr = Arc::new(WorktreeManager::new());
        let run_subdir = dir.path().join(run_id.to_string());
        let state = Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("/bin/true"),
            wt_mgr,
            CleanupPolicy::Never,
            run_subdir,
            ApprovalPolicy::Block,
            None,
        ));

        let sock = dir.path().join("test.sock");
        let server = McpServer::start(sock.clone(), state).await.unwrap();
        assert!(sock.exists(), "socket file should exist after start");
        assert_eq!(server.socket_path(), sock.as_path());

        // Connect a raw unix stream to verify the server is listening.
        let stream = tokio::net::UnixStream::connect(&sock).await;
        assert!(stream.is_ok(), "server should accept connections");

        drop(server);
        // Socket is cleaned up on drop.
    }

    #[tokio::test]
    async fn server_drops_cleanly_even_with_active_connection() {
        use crate::dispatch::state::{ApprovalPolicy, DispatchState};
        use crate::manifest::resolve::ResolvedManifest;
        use crate::manifest::schema::WorktreeCleanup;
        use pitboss_core::process::{ProcessSpawner, TokioSpawner};
        use pitboss_core::session::CancelToken;
        use pitboss_core::store::{JsonFileStore, SessionStore};
        use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
        use std::path::PathBuf;
        use std::sync::Arc;
        use tokio::time::Duration;

        let dir = TempDir::new().unwrap();
        let manifest = ResolvedManifest {
            max_parallel: 4,
            halt_on_failure: false,
            run_dir: dir.path().to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            tasks: vec![],
            lead: None,
            max_workers: Some(4),
            budget_usd: Some(5.0),
            lead_timeout_secs: None,
            approval_policy: None,
            notifications: vec![],
        };
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        let run_id = Uuid::now_v7();
        let spawner: Arc<dyn ProcessSpawner> = Arc::new(TokioSpawner::new());
        let wt_mgr = Arc::new(WorktreeManager::new());
        let run_subdir = dir.path().join(run_id.to_string());
        let state = Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            "lead".into(),
            spawner,
            PathBuf::from("/bin/true"),
            wt_mgr,
            CleanupPolicy::Never,
            run_subdir,
            ApprovalPolicy::Block,
            None,
        ));

        let sock = dir.path().join("drop-test.sock");
        let server = McpServer::start(sock.clone(), state).await.unwrap();

        // Open a raw connection and hold it; the accept task will spawn a
        // tracked per-connection task to serve it.
        let _stream = tokio::net::UnixStream::connect(&sock).await.unwrap();

        // Give the server a moment to accept and spawn the session task so the
        // tracker is non-empty before we drop.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drop the server while the connection is still open. Should complete
        // near-instantly via the cancellation token, not wait for MCP session
        // timeout (which can be up to an hour for wait_for_worker).
        let dropped_at = std::time::Instant::now();
        drop(server);
        let elapsed = dropped_at.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "Drop took too long: {:?}",
            elapsed
        );
        assert!(!sock.exists(), "socket file should be removed on drop");
    }
}
