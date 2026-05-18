// Shared test scaffolding. The module-level doc lives on the two
// wrapper sites (`tests/support/mod.rs` and `src/test_support.rs`)
// because this file is reached both via `mod state_builder;` and
// via `include!()`, and `include!()` rejects inner attributes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pitboss_cli::dispatch::layer::LayerState;
use pitboss_cli::dispatch::state::{ApprovalPolicy, DispatchState};
use pitboss_cli::manifest::resolve::{ResolvedLead, ResolvedManifest};
use pitboss_cli::manifest::schema::{
    CommunicationConfig, Effort, UntypedActorPolicy, WorkerType, WorktreeCleanup,
};
use pitboss_cli::shared_store::SharedStore;
use pitboss_core::process::fake::{FakeScript, FakeSpawner};
use pitboss_core::process::{ProcessSpawner, TokioSpawner};
use pitboss_core::session::CancelToken;
use pitboss_core::store::{JsonFileStore, SessionStore};
use pitboss_core::worktree::{CleanupPolicy, WorktreeManager};
use tempfile::TempDir;
use uuid::Uuid;

/// Builder for `DispatchState` (and `LayerState`) test fixtures.
///
/// Collapses the ~14 hand-rolled `mk_state` / `test_state` / `mk_layer`
/// factories that previously lived in `#[cfg(test)]` modules across
/// `dispatch/`, `mcp/`, `control/`, `communication.rs`, and the
/// `tests/*_flows.rs` integration suite. See issue #488.
///
/// Defaults match the most-common shape across the factories this
/// replaces: no lead (flat mode), `FakeSpawner` with
/// `hold_until_signal()`, `$5` budget cap, 4 max workers, 4 max
/// parallel tasks, `ApprovalPolicy::Block`, no communication. Opt in
/// to variations via the fluent setters.
///
/// ## Examples
///
/// Flat-mode state, defaults (matches `dispatch/state.rs::mk_state`):
/// ```ignore
/// let (_dir, state) = TestStateBuilder::new().build();
/// ```
///
/// Hierarchical lead, sub-leads enabled (matches
/// `tests/sublead_flows.rs::mk_state_with_subleads`):
/// ```ignore
/// let (_dir, state) = TestStateBuilder::new()
///     .with_lead()
///     .lead_id("root")
///     .allow_subleads()
///     .max_parallel_tasks(8)
///     .max_workers(20)
///     .budget(20.0)
///     .build();
/// ```
///
/// Approval-policy varying (matches `mcp/approval.rs::mk_state`):
/// ```ignore
/// let (dir, state) = TestStateBuilder::new()
///     .tokio_spawner()
///     .approval_policy(ApprovalPolicy::AutoApprove)
///     .default_approval_policy(ApprovalPolicy::AutoApprove)
///     .budget(1.0)
///     .build();
/// ```
#[allow(dead_code)]
pub struct TestStateBuilder {
    budget_usd: Option<f64>,
    lead_budget_usd: Option<f64>,
    max_workers: Option<u32>,
    max_parallel_tasks: Option<u32>,
    approval_policy: ApprovalPolicy,
    default_approval_policy: Option<ApprovalPolicy>,
    communication: CommunicationConfig,
    lead: Option<ResolvedLead>,
    spawner: Option<Arc<dyn ProcessSpawner>>,
    run_id: Option<Uuid>,
    run_subdir: Option<PathBuf>,
    claude_binary: PathBuf,
    lead_id: String,
    worker_types: Vec<WorkerType>,
    require_actor_type: bool,
    untyped_actor_policy: UntypedActorPolicy,
}

#[allow(dead_code)]
impl TestStateBuilder {
    pub fn new() -> Self {
        Self {
            budget_usd: Some(5.0),
            lead_budget_usd: None,
            max_workers: Some(4),
            max_parallel_tasks: Some(4),
            approval_policy: ApprovalPolicy::Block,
            default_approval_policy: None,
            communication: CommunicationConfig::default(),
            lead: None,
            spawner: None,
            run_id: None,
            run_subdir: None,
            claude_binary: PathBuf::from("claude"),
            lead_id: "lead".into(),
            worker_types: vec![],
            require_actor_type: false,
            untyped_actor_policy: UntypedActorPolicy::default(),
        }
    }

    pub fn worker_types(mut self, types: Vec<WorkerType>) -> Self {
        self.worker_types = types;
        self
    }

    pub fn require_actor_type(mut self, require: bool) -> Self {
        self.require_actor_type = require;
        self
    }

    pub fn untyped_actor_policy(mut self, policy: UntypedActorPolicy) -> Self {
        self.untyped_actor_policy = policy;
        self
    }

    // ── manifest knobs ──────────────────────────────────────────────────────

    pub fn budget(mut self, usd: f64) -> Self {
        self.budget_usd = Some(usd);
        self
    }

    pub fn no_budget(mut self) -> Self {
        self.budget_usd = None;
        self
    }

    pub fn lead_budget(mut self, usd: f64) -> Self {
        self.lead_budget_usd = Some(usd);
        self
    }

    pub fn max_workers(mut self, n: u32) -> Self {
        self.max_workers = Some(n);
        self
    }

    pub fn no_max_workers(mut self) -> Self {
        self.max_workers = None;
        self
    }

    pub fn max_parallel_tasks(mut self, n: u32) -> Self {
        self.max_parallel_tasks = Some(n);
        self
    }

    pub fn approval_policy(mut self, p: ApprovalPolicy) -> Self {
        self.approval_policy = p;
        self
    }

    pub fn default_approval_policy(mut self, p: ApprovalPolicy) -> Self {
        self.default_approval_policy = Some(p);
        self
    }

    pub fn communication(mut self, c: CommunicationConfig) -> Self {
        self.communication = c;
        self
    }

    // ── lead knobs ──────────────────────────────────────────────────────────

    /// Install a default `ResolvedLead`, enabling hierarchical-mode
    /// tests. Idempotent: a second call leaves any prior customizations
    /// in place.
    pub fn with_lead(mut self) -> Self {
        if self.lead.is_none() {
            self.lead = Some(default_lead());
        }
        self
    }

    /// Override both the `lead.id` (if a lead is installed) and the
    /// `DispatchState::new` `lead_id` argument used for token minting.
    pub fn lead_id(mut self, id: impl Into<String>) -> Self {
        let id = id.into();
        if let Some(ref mut lead) = self.lead {
            lead.id = id.clone();
        }
        self.lead_id = id;
        self
    }

    pub fn lead_model(mut self, model: impl Into<String>) -> Self {
        let mut lead = self.lead.unwrap_or_else(default_lead);
        lead.model = model.into();
        self.lead = Some(lead);
        self
    }

    pub fn allow_subleads(mut self) -> Self {
        let mut lead = self.lead.unwrap_or_else(default_lead);
        lead.allow_subleads = true;
        self.lead = Some(lead);
        self
    }

    pub fn max_sublead_budget(mut self, usd: f64) -> Self {
        let mut lead = self.lead.unwrap_or_else(default_lead);
        lead.max_sublead_budget_usd = Some(usd);
        self.lead = Some(lead);
        self
    }

    pub fn max_subleads(mut self, n: u32) -> Self {
        let mut lead = self.lead.unwrap_or_else(default_lead);
        lead.max_subleads = Some(n);
        self.lead = Some(lead);
        self
    }

    // ── runtime knobs ───────────────────────────────────────────────────────

    pub fn spawner(mut self, s: Arc<dyn ProcessSpawner>) -> Self {
        self.spawner = Some(s);
        self
    }

    /// Use a real `TokioSpawner`. Tests that don't actually exercise
    /// the spawn path can use this safely — it just isn't invoked.
    pub fn tokio_spawner(mut self) -> Self {
        self.spawner = Some(Arc::new(TokioSpawner::new()));
        self
    }

    /// Use a `FakeSpawner` whose script terminates immediately
    /// (no `hold_until_signal`). Useful for handler-shape tests that
    /// don't care about worker liveness.
    pub fn fake_spawner_no_hold(mut self) -> Self {
        self.spawner = Some(Arc::new(FakeSpawner::new(FakeScript::new())));
        self
    }

    pub fn run_id(mut self, id: Uuid) -> Self {
        self.run_id = Some(id);
        self
    }

    pub fn run_subdir(mut self, p: PathBuf) -> Self {
        self.run_subdir = Some(p);
        self
    }

    pub fn claude_binary(mut self, p: PathBuf) -> Self {
        self.claude_binary = p;
        self
    }

    // ── build ───────────────────────────────────────────────────────────────

    /// Build the `DispatchState`, allocating a fresh `TempDir` whose
    /// lifetime the caller owns. Drop the `TempDir` to clean up;
    /// `std::mem::forget(dir)` to leak it (matches the pre-refactor
    /// pattern used by tests that hold paths into the dir).
    pub fn build(self) -> (TempDir, Arc<DispatchState>) {
        let dir = TempDir::new().unwrap();
        let state = self.build_in(dir.path());
        (dir, state)
    }

    /// Build into an externally-owned directory. Used for tests
    /// (e.g. `control/server.rs`) that mint their own `dir` + `run_id`.
    pub fn build_in(self, dir_path: &Path) -> Arc<DispatchState> {
        let run_id = self.run_id.unwrap_or_else(Uuid::now_v7);
        let run_subdir = self
            .run_subdir
            .clone()
            .unwrap_or_else(|| dir_path.join(run_id.to_string()));
        let manifest = self.build_manifest(dir_path);
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir_path.to_path_buf()));
        let spawner = self.spawner.unwrap_or_else(default_fake_spawner);
        let wt_mgr = Arc::new(WorktreeManager::new());
        Arc::new(DispatchState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            self.lead_id,
            spawner,
            self.claude_binary,
            wt_mgr,
            CleanupPolicy::Never,
            run_subdir,
            self.approval_policy,
            None,
            Arc::new(SharedStore::new()),
        ))
    }

    /// Build a standalone `LayerState`. Only `dispatch/layer.rs` tests
    /// need this — every other site builds a `DispatchState`.
    pub fn build_layer(self) -> (TempDir, LayerState) {
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().to_path_buf();
        let run_id = self.run_id.unwrap_or_else(Uuid::now_v7);
        let run_subdir = self.run_subdir.clone().unwrap_or_else(|| dir_path.clone());
        let manifest = self.build_manifest(&dir_path);
        let store: Arc<dyn SessionStore> = Arc::new(JsonFileStore::new(dir_path.clone()));
        let spawner = self.spawner.unwrap_or_else(default_fake_spawner);
        let layer = LayerState::new(
            run_id,
            manifest,
            store,
            CancelToken::new(),
            self.lead_id,
            spawner,
            self.claude_binary,
            Arc::new(WorktreeManager::new()),
            CleanupPolicy::Never,
            run_subdir,
            self.approval_policy,
            None,
            Arc::new(SharedStore::new()),
            None,
        );
        (dir, layer)
    }

    fn build_manifest(&self, dir_path: &Path) -> ResolvedManifest {
        ResolvedManifest {
            manifest_schema_version: 0,
            name: None,
            max_parallel_tasks: self.max_parallel_tasks,
            halt_on_failure: false,
            run_dir: dir_path.to_path_buf(),
            worktree_cleanup: WorktreeCleanup::OnSuccess,
            emit_event_stream: false,
            claude_setting_sources: None,
            tasks: vec![],
            lead: self.lead.clone(),
            max_workers: self.max_workers,
            budget_usd: self.budget_usd,
            lead_budget_usd: self.lead_budget_usd,
            lead_timeout_secs: None,
            default_approval_policy: self.default_approval_policy,
            denial_termination_policy: None,
            notifications: vec![],
            dump_shared_store: false,
            require_plan_approval: false,
            approval_rules: vec![],
            container: None,
            mcp_servers: vec![],
            communication: self.communication.clone(),
            lifecycle: None,
            worker_types: self.worker_types.clone(),
            sublead_types: vec![],
            require_actor_type: self.require_actor_type,
            untyped_actor_policy: self.untyped_actor_policy,
            agent_profiles: std::collections::HashMap::new(),
        }
    }
}

impl Default for TestStateBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Default `ResolvedLead` used by `with_lead()`. Matches the
/// most-common lead shape across the existing factories: haiku model,
/// no worktree, no subleads.
fn default_lead() -> ResolvedLead {
    ResolvedLead {
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
        allow_subleads: false,
        max_subleads: None,
        max_sublead_budget_usd: None,
        max_total_workers: None,
        sublead_defaults: None,
    }
}

/// Default spawner used when none is explicitly set. Mirrors the
/// pre-refactor default: a `FakeSpawner` whose children hold open
/// until the dispatcher signals them, so `active_worker_count()`
/// stays deterministic across the test.
fn default_fake_spawner() -> Arc<dyn ProcessSpawner> {
    Arc::new(FakeSpawner::new(FakeScript::new().hold_until_signal()))
}
