# Compatibility

Pitboss makes specific backward-compatibility guarantees at each version boundary.

## v0.8.0 — correctness hardening and new capabilities

### Backward compatible with v0.7

v0.8 is backward-compatible with v0.7 manifests and tooling with one caveat:

- **Manifests**: All v0.7 manifests run unchanged. The new `[container]` and `permission_routing` fields are optional; their absence preserves v0.7 behavior. `permission_routing = "path_b"` is explicitly rejected with an error until the follow-on stabilization lands (see issues #92–#94).
- **Wire format**: `ApprovalResponse` gains `from_ttl: bool` (default `false`). Existing consumers parsing approval responses see no change. `summary.json` and `summary.jsonl` gain `ApprovalTimedOut` as a `status` string value alongside existing `ApprovalRejected` and `Success`.
- **Control protocol**: `ControlEvent::Hello` now includes `policy_rules` (skipped when empty). `ControlOp::UpdatePolicy` is a new op; v0.7 TUI clients that don't send it work unchanged.
- **`approval_bridge` internal type change**: `BridgeEntry` replaces the bare `Sender<ApprovalResponse>` in the bridge map. Entirely internal; no wire or on-disk format change.
- **`DispatchState` Deref removed**: This is an internal Rust type. Operators writing custom pitboss forks or extensions that compiled against pitboss-cli as a library will need to update call sites from `state.<field>` to `state.root.<field>`.

### Nothing removed in v0.8

No tools, manifest fields, CLI subcommands, or TUI behaviors were removed in v0.8. `pitboss container-dispatch` and `pitboss status` are additive.

---

## v0.6.0 — depth-2 sub-leads

### Backward compatible with v0.5

v0.6 is fully backward-compatible with v0.5 manifests and tooling:

- **Manifests**: v0.5 manifests (flat mode, hierarchical without `allow_subleads`) run unchanged. `allow_subleads` defaults to `false`; no new fields are required.
- **MCP callers**: v0.5 leads that only call `spawn_worker`, `wait_for_worker`, `list_workers`, etc. work identically. New tools (`spawn_sublead`, `wait_actor`, `run_lease_acquire`, `run_lease_release`) are additive and not required.
- **Control-plane clients**: TUI sessions connected to a v0.6 dispatcher behave identically when no sub-leads are spawned. New TUI elements (grouped grid, approval list pane) appear only when depth-2 features are used.
- **Wire format**: `EventEnvelope` adds `actor_path` (e.g., `"root→S1→W3"`) with `serde(skip_serializing_if = "ActorPath::is_empty")`, so v0.5 consumers parsing event streams see no change for flat or depth-1 runs.
- **On-disk run artifacts**: `summary.json` schema is backward-compatible. New fields added with `#[serde(default)]`; pre-v0.6 records parse cleanly.
- **SQLite**: All schema migrations are idempotent. Opening a v0.5 database under v0.6 auto-migrates.

### Nothing removed in v0.6

No tools, manifest fields, CLI subcommands, or TUI behaviors were removed in v0.6. `wait_for_worker` is retained as a back-compat alias for `wait_actor`.

## v0.5.0

### Backward compatible with v0.4

- v0.4.x manifests run unchanged. `require_plan_approval` defaults to `false`.
- `pause_worker` gains a `mode` field; the default (`"cancel"`) matches v0.4 behavior.
- `approval_policy` defaults to `"block"`, matching v0.4.
- v0.4.x run directories deserialize with new counter fields defaulting to 0.

## v0.4.0

### Backward compatible with v0.3

- v0.3.x manifests run unchanged. `approval_policy` defaults to `"block"`.
- v0.3.x on-disk runs: `control.sock` absent → TUI enters observe-only mode.
- `parent_task_id` on `TaskRecord` uses `#[serde(default)]`; v0.3 records parse as `null`.

## Forward-looking guarantees

Pitboss follows [Semantic Versioning](https://semver.org/):

- **Patch versions** (0.6.x) — bug fixes only; no schema or API changes.
- **Minor versions** (0.7+) — additive features; existing manifests and callers continue to work.
- **Major version** (1.0) — reserved for breaking changes. None currently planned.

The authoritative guide to what changed in each version is [`CHANGELOG.md`](./changelog.md) in this book (sourced directly from the repository's `CHANGELOG.md`).

## Checking compatibility

```bash
pitboss validate pitboss.toml
```

`pitboss validate` is the runtime source of truth. If a manifest field doesn't parse, validate will report it. The binary always wins over documentation — file a PR if something here is wrong.
