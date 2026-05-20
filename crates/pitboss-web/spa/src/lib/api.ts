// API client for pitboss-web. The Rust server is mounted at the same origin
// in production (rust-embed serves the SPA bundle), and is reached via the
// Vite dev proxy under `/api` during local development.

import { browser } from '$app/environment';

// Mirror of `pitboss_cli::runs::RunStatus`. `'cancelled'` was added in
// the #365 fix: previously a `cancel_run`-finalized run rendered as
// `'complete'` (green), which conflated user-initiated stops with clean
// success. The status-badge component maps `'cancelled'` to red.
export type RunStatus = 'complete' | 'cancelled' | 'running' | 'stale' | 'aborted';

export interface RunDto {
  run_id: string;
  status: RunStatus;
  status_label: string;
  mtime_unix: number;
  tasks_total: number;
  tasks_failed: number;
  /** Peak memory utilization fraction (#553). Omitted on the wire
   *  when no sample was taken; the run-list renders `—` in that case. */
  peak_utilization_pct?: number;
}

export interface RunDetailDto extends RunDto {
  // Allow extra fields the backend might add (task list, manifest hash, etc.)
  // without forcing a type bump on every change.
  [k: string]: unknown;
}

export interface TaskLogOpts {
  /** Maximum number of lines to return. */
  limit?: number;
  /** When true, return the tail (last N lines) rather than the head. */
  tail?: boolean;
}

export class ApiError extends Error {
  status: number;
  body: string;
  constructor(status: number, body: string, message?: string) {
    super(message ?? `HTTP ${status}`);
    this.name = 'ApiError';
    this.status = status;
    this.body = body;
  }
}

const TOKEN_KEY = 'pitboss_token';

function authHeader(): Record<string, string> {
  if (!browser) return {};
  try {
    const t = window.localStorage.getItem(TOKEN_KEY);
    return t ? { Authorization: `Bearer ${t}` } : {};
  } catch {
    return {};
  }
}

export function setToken(token: string | null): void {
  if (!browser) return;
  try {
    if (token) window.localStorage.setItem(TOKEN_KEY, token);
    else window.localStorage.removeItem(TOKEN_KEY);
  } catch {
    /* localStorage disabled — silently ignore */
  }
}

export function getToken(): string | null {
  if (!browser) return null;
  try {
    return window.localStorage.getItem(TOKEN_KEY);
  } catch {
    return null;
  }
}

async function request<T>(
  path: string,
  init: RequestInit & { accept?: 'json' | 'text' } = {}
): Promise<T> {
  const accept = init.accept ?? 'json';
  const headers: Record<string, string> = {
    Accept: accept === 'json' ? 'application/json' : 'text/plain',
    ...authHeader(),
    ...((init.headers as Record<string, string>) ?? {})
  };

  const res = await fetch(path, { ...init, headers });
  if (!res.ok) {
    const body = await res.text().catch(() => '');
    throw new ApiError(res.status, body);
  }
  if (accept === 'text') return (await res.text()) as unknown as T;
  // Empty body guard (some endpoints might 204).
  const text = await res.text();
  if (!text) return undefined as unknown as T;
  return JSON.parse(text) as T;
}

const enc = encodeURIComponent;

// ---- Endpoints ------------------------------------------------------------

export function listRuns(): Promise<RunDto[]> {
  return request<RunDto[]>('/api/runs');
}

export function getRun(id: string): Promise<RunDetailDto> {
  return request<RunDetailDto>(`/api/runs/${enc(id)}`);
}

export function getResolvedManifest(id: string): Promise<unknown> {
  return request<unknown>(`/api/runs/${enc(id)}/resolved`);
}

export function getManifestToml(id: string): Promise<string> {
  return request<string>(`/api/runs/${enc(id)}/manifest`, { accept: 'text' });
}

export function getSummaryJsonl(id: string): Promise<string> {
  return request<string>(`/api/runs/${enc(id)}/summary-jsonl`, { accept: 'text' });
}

/**
 * GET /api/runs/:id/events-jsonl — persisted control-event stream
 * (#259). Parses NDJSON into `ControlEnvelope`s for the Replay tab.
 *
 * 404 (run never had `[run].emit_event_stream = true`, or the file
 * exists but no envelopes have been appended yet) returns an empty
 * list rather than throwing, so the Replay tab can render an empty-
 * state explaining the manifest flag instead of an error banner.
 * Other errors propagate to the caller.
 */
export async function getEventsJsonl(id: string): Promise<ControlEnvelope[]> {
  try {
    const text = await request<string>(`/api/runs/${enc(id)}/events-jsonl`, { accept: 'text' });
    return text
      .split('\n')
      .filter((l) => l.trim().length > 0)
      .map((l) => {
        try {
          return JSON.parse(l) as ControlEnvelope;
        } catch {
          // Partial-write tail caught mid-flush — skip silently.
          // Matches the dispatcher's canonical "drop incomplete
          // trailing line" semantics in `pitboss events` and the
          // summary.jsonl reader.
          return null;
        }
      })
      .filter((e): e is ControlEnvelope => e !== null);
  } catch (err) {
    if (err instanceof ApiError && err.status === 404) return [];
    throw err;
  }
}

export function getTaskLog(runId: string, taskId: string, opts: TaskLogOpts = {}): Promise<string> {
  const params = new URLSearchParams();
  if (opts.limit !== undefined) params.set('limit', String(opts.limit));
  if (opts.tail !== undefined) params.set('tail', String(opts.tail));
  const qs = params.toString();
  const path = `/api/runs/${enc(runId)}/tasks/${enc(taskId)}/log${qs ? `?${qs}` : ''}`;
  return request<string>(path, { accept: 'text' });
}

/**
 * Wire-format mirror of `pitboss_core::store::TaskRecord`. The SPA only
 * names the fields it renders; backend additions land here as needed.
 */
export interface TaskRecord {
  task_id: string;
  status: string;
  exit_code: number | null;
  started_at: string;
  ended_at: string;
  duration_ms: number;
  worktree_path: string | null;
  log_path: string;
  token_usage: {
    input: number;
    output: number;
    cache_read?: number;
    cache_creation?: number;
  };
  claude_session_id: string | null;
  final_message_preview: string | null;
  final_message: string | null;
  parent_task_id: string | null;
  pause_count: number;
  reprompt_count: number;
  approvals_requested: number;
  approvals_approved: number;
  approvals_rejected: number;
  model: string | null;
  failure_reason: unknown | null;
  /** Dispatcher-stamped USD cost (#258); falls back to client-side
   * `prices.ts` table when null on older runs. */
  cost_usd?: number | null;
  /** Resolved profile id (`worker_type` / `sublead_type`); v0.12+. */
  actor_type?: string | null;
}

/** NDJSON event row from `tasks/<task_id>/events.jsonl`. The kind tag is
 * snake-case to match `pitboss-cli/src/dispatch/events.rs::TaskEvent`. */
export type TaskEvent =
  | { kind: 'pause'; at: string; reason?: string }
  | { kind: 'continue'; at: string; new_session_id: string; prompt_preview: string }
  | { kind: 'reprompt'; at: string; prompt_preview: string; prior_session_id: string }
  | { kind: 'approval_request'; at: string; request_id: string; summary_preview: string }
  | {
      kind: 'approval_response';
      at: string;
      request_id: string;
      approved: boolean;
      edited: boolean;
    }
  | { kind: 'notification_failed'; at: string; sink_id: string; event_kind: string; error: string }
  | {
      kind: 'tool_denied';
      at: string;
      tool_name: string;
      actor_id: string;
      reason_kind: string;
      reason: string;
    }
  | {
      kind: 'tool_auto_approved';
      at: string;
      tool_name: string;
      actor_id: string;
      actor_type: string;
    };

/** GET /api/runs/:id/tasks/:task_id/events — NDJSON parsed into rows.
 * 404 (no events.jsonl yet — task never paused/repromp ted/got an
 * approval, etc.) returns an empty list rather than throwing so the
 * inspector can render "no lifecycle events yet". */
export async function getTaskEvents(runId: string, taskId: string): Promise<TaskEvent[]> {
  try {
    const text = await request<string>(`/api/runs/${enc(runId)}/tasks/${enc(taskId)}/events`, {
      accept: 'text'
    });
    return text
      .split('\n')
      .filter((l) => l.trim().length > 0)
      .map((l) => {
        try {
          return JSON.parse(l) as TaskEvent;
        } catch {
          return null;
        }
      })
      .filter((e): e is TaskEvent => e !== null);
  } catch (err) {
    if (err instanceof ApiError && err.status === 404) return [];
    throw err;
  }
}

export function getTaskDetail(runId: string, taskId: string): Promise<TaskRecord> {
  return request<TaskRecord>(`/api/runs/${enc(runId)}/tasks/${enc(taskId)}`);
}

// ---- Control writes (POST /api/runs/:id/control) ------------------------

/**
 * Wire-format mirror of the Rust `ControlOp` enum
 * (`pitboss_cli::control::protocol::ControlOp`). The dispatcher decodes
 * by the `op` discriminator; all field names match the Rust serde
 * representation (`snake_case`).
 *
 * Hello is intentionally omitted — the bridge sends the client Hello
 * automatically on first connect and rejects any client that tries to
 * impersonate it.
 */
export type ControlOp =
  | { op: 'cancel_worker'; task_id: string }
  | { op: 'cancel_run' }
  | { op: 'pause_worker'; task_id: string; mode?: 'cancel' | 'freeze' }
  | { op: 'continue_worker'; task_id: string; prompt?: string }
  | { op: 'reprompt_worker'; task_id: string; prompt: string }
  | {
      op: 'approve';
      request_id: string;
      approved: boolean;
      comment?: string;
      edited_summary?: string;
      reason?: string;
    }
  | { op: 'list_workers' }
  | { op: 'update_policy'; rules: PolicyRule[] };

/**
 * Mirror of `ApprovalRule`. The shape is intentionally loose — the
 * editor renders unknown match fields as raw JSON so server-side
 * additions don't break the UI.
 */
export interface PolicyRule {
  match: Record<string, unknown>;
  action: ApprovalAction;
}

export type ApprovalAction =
  | { action: 'auto_approve' }
  | { action: 'auto_deny'; reason?: string }
  | { action: 'require_operator' };

/**
 * Send a single ControlOp to the dispatcher. Returns void on `202`. Any
 * dispatcher-side ack/failure is delivered out-of-band on the SSE event
 * stream (`OpAcked` / `OpFailed`); subscribe first if you need to
 * observe it.
 */
export async function postControlOp(runId: string, op: ControlOp): Promise<void> {
  await request<void>(`/api/runs/${enc(runId)}/control`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(op),
    accept: 'json'
  });
}

// ---- Live actor state (mirrors control::protocol on the wire) ------------

/**
 * One row of `WorkersSnapshot.workers`. Loose typing for fields the SPA
 * doesn't render today so dispatcher additions don't break the UI.
 */
export interface WorkerEntry {
  task_id: string;
  state: string;
  prompt_preview: string;
  started_at?: string;
  parent_task_id?: string;
  session_id?: string;
}

/** Per-actor row in `StoreActivity.counters`. */
export interface ActorActivity {
  actor_id: string;
  kv_ops: number;
  lease_ops: number;
  message_ops?: number;
  artifact_ops?: number;
}

/** Snapshot of a sublead derived from `SubleadSpawned` (+ Terminated). */
export interface SubleadInfo {
  sublead_id: string;
  budget_usd?: number | null;
  max_workers?: number | null;
  read_down: boolean;
  /** Set once the sub-tree exits — `success` | `cancel` | `timeout` | `error`. */
  outcome?: string;
  spent_usd?: number;
  unspent_usd?: number;
}

/** `WorkerFailed.reason` payload. Treated loosely; renderer reads `kind`. */
export type FailureReason = { kind: string; [k: string]: unknown };

// ---- Resource sampling (#553) -------------------------------------------

/** Mirror of `pitboss_cli::control::protocol::ResourceSampleEntry`. */
export interface ResourceSampleEntry {
  actor_id: string;
  /** Omitted on the wire when 0 (subprocess hadn't published yet). */
  pid?: number;
  rss_bytes?: number;
  vsz_bytes?: number;
  /** Cumulative `utime + stime` in clock-ticks. Consumers diff across
   *  samples for CPU%. */
  cpu_jiffies?: number;
}

/** Mirror of `pitboss_cli::control::protocol::PressureLevel`. */
export type PressureLevel = 'clear' | 'warn' | 'error';

/** Mirror of the `resource_sample` `ControlEvent` variant. */
export interface ResourceSampleEvent {
  event: 'resource_sample';
  samples: ResourceSampleEntry[];
  cgroup_memory_current_bytes?: number;
  cgroup_memory_max_bytes?: number;
  host_mem_total_bytes?: number;
}

/** Mirror of the `resource_pressure` `ControlEvent` variant. */
export interface ResourcePressureEvent {
  event: 'resource_pressure';
  level: PressureLevel;
  total_rss_bytes?: number;
  available_bytes?: number;
  message?: string;
}

/** Mirror of `pitboss_core::store::record::ResourceHighWater` (finalize-
 *  time roll-up persisted to `summary.json`). */
export interface ResourceHighWater {
  total_rss_bytes_max?: number;
  cgroup_memory_max_bytes?: number;
  /** Fractional value in [0, 1+]. The "Mem peak" column on the runs
   *  list multiplies by 100 for display. */
  peak_utilization_pct?: number;
  rss_bytes_max_by_actor?: Record<string, number>;
  sample_count?: number;
  sample_cadence_secs?: number;
}

// ---- Manifest workspace (Phase 4) ----------------------------------------

export interface ManifestEntry {
  name: string;
  size: number;
  mtime_unix: number;
}

/**
 * Mirror of `pitboss_cli::capability_matrix::RowKind`. Snake-case wire
 * shape; the Rust `#[serde(rename_all = "snake_case")]` is the contract.
 */
export type RowKind = 'untyped' | 'worker_type' | 'sublead_type';

/**
 * Mirror of `pitboss_cli::capability_matrix::MatrixRow`. One row of the
 * actor-type × MCP-server matrix returned by `validateManifest` when
 * validation succeeds. Matches the wire shape pinned by
 * `matrix_row_serialises_with_stable_field_and_kind_names` (#391 slice 3).
 */
/**
 * Mirror of `pitboss_cli::capability_matrix::MatrixServerEntry`.
 * One server admitted on a `MatrixRow`, plus the per-server
 * `[[mcp_server]].tools` allowlist as it applies on that row.
 *
 * `tools` is omitted (not `null`) when the server has no allowlist —
 * the Rust struct uses `#[serde(skip_serializing_if = "Option::is_none")]`
 * so the SPA branches on key-presence, not on a sentinel. (#391)
 */
export interface MatrixServerEntry {
  server_id: string;
  /** Absent => unrestricted. Present => explicit allowlist. */
  tools?: string[];
}

export interface MatrixRow {
  /** Display label as the CLI text formatter emits it. */
  label: string;
  /** `null` for the untyped row, the type id for declared profiles. */
  actor_type: string | null;
  kind: RowKind;
  /**
   * MCP servers (with their per-server tool allowlists) that scope-admit,
   * in manifest declaration order.
   */
  servers: MatrixServerEntry[];
}

export interface ValidateResult {
  ok: boolean;
  errors: string[];
  /**
   * Capability matrix rows when validation succeeds; absent on failure.
   * Always populated on success — even for manifests with no
   * `[[mcp_server]]` declarations, so the UI can render the untyped
   * row's `(none)` cell rather than guess from a missing field.
   */
  capability_matrix?: MatrixRow[];
}

export interface DispatchDescriptor {
  run_id: string;
  manifest_path: string;
  started_at: string;
  child_pid?: number;
  [k: string]: unknown;
}

/**
 * Mirror of `pitboss_schema::SchemaSection`. Treated loosely so future
 * additions on the Rust side don't require a TS bump on every change.
 */
export interface SchemaSection {
  toml_path: string;
  type_name: string;
  fields: SchemaField[];
}

export interface SchemaField {
  name: string;
  label: string;
  help: string;
  form_type: string;
  required: boolean;
  enum_values: string[];
}

export function listManifests(): Promise<ManifestEntry[]> {
  return request<ManifestEntry[]>('/api/manifests');
}

export function readManifest(name: string): Promise<string> {
  return request<string>(`/api/manifests/${enc(name)}`, { accept: 'text' });
}

/**
 * URL for downloading a manifest as a `Content-Disposition: attachment`
 * file. Browsers cannot set Authorization headers on `<a download>` or
 * `window.location` navigations, so when a token is configured we
 * fall back to the `?token=` query param the auth middleware accepts
 * for routes that can't carry headers (the same path the SSE event
 * stream uses).
 */
export function exportManifestUrl(name: string): string {
  const tok = getToken();
  const params = new URLSearchParams({ download: '1' });
  if (tok) params.set('token', tok);
  return `/api/manifests/${enc(name)}?${params.toString()}`;
}

export function saveManifest(
  name: string,
  contents: string
): Promise<{ name: string; bytes: number }> {
  return request<{ name: string; bytes: number }>('/api/manifests', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name, contents }),
    accept: 'json'
  });
}

export function validateManifest(contents: string): Promise<ValidateResult> {
  return request<ValidateResult>('/api/manifests/validate', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ contents }),
    accept: 'json'
  });
}

export function dispatchManifest(
  manifest_name: string
): Promise<{ descriptor: DispatchDescriptor }> {
  return request<{ descriptor: DispatchDescriptor }>('/api/runs', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ manifest_name }),
    accept: 'json'
  });
}

export function forkRun(runId: string, new_name: string): Promise<{ name: string }> {
  return request<{ name: string }>(`/api/runs/${enc(runId)}/fork`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ new_name }),
    accept: 'json'
  });
}

export function getSchema(): Promise<SchemaSection[]> {
  return request<SchemaSection[]>('/api/schema');
}

// ---- Insights (cross-run aggregator) -------------------------------------

export interface RunDigest {
  run_id: string;
  manifest_name: string;
  manifest_path: string | null;
  status: RunStatus;
  outcome: 'success' | 'failed' | 'partial' | 'running' | 'stale' | 'aborted';
  started_at: number | null;
  ended_at: number | null;
  duration_ms: number | null;
  tasks_total: number;
  tasks_failed: number;
  failure_kinds: string[];
  /** Peak memory utilization fraction (#553). Omitted when sampling
   *  was disabled / unsupported / pre-#553. Rendered as the "Mem peak"
   *  column on the run-list. */
  peak_utilization_pct?: number;
}

export interface TaskFailureDigest {
  run_id: string;
  manifest_name: string;
  task_id: string;
  parent_task_id: string | null;
  failure_kind: string;
  error_message: string | null;
  error_template: string | null;
  model: string | null;
  duration_ms: number | null;
  occurred_at: number | null;
}

export interface Cluster {
  kind: string;
  template: string | null;
  count: number;
  first_seen: number | null;
  last_seen: number | null;
  manifests: string[];
  task_ids: string[];
  run_ids: string[];
  exemplar_message: string | null;
}

export interface ManifestSummary {
  manifest_name: string;
  runs_total: number;
  runs_failed: number;
  success_rate: number;
  last_run_at: number | null;
  avg_duration_ms: number | null;
  failure_kinds: string[];
}

export interface InsightsFilter {
  manifest?: string;
  since?: number;
  until?: number;
  status?: string;
  kind?: string;
  limit?: number;
  offset?: number;
  min_count?: number;
}

function insightsQuery(f: InsightsFilter): string {
  const params = new URLSearchParams();
  for (const [k, v] of Object.entries(f)) {
    if (v !== undefined && v !== null && v !== '') params.set(k, String(v));
  }
  const qs = params.toString();
  return qs ? `?${qs}` : '';
}

export function listInsightsRuns(
  filter: InsightsFilter = {}
): Promise<{ runs: RunDigest[]; total: number }> {
  return request(`/api/insights/runs${insightsQuery(filter)}`);
}

export function listInsightsFailures(
  filter: InsightsFilter = {}
): Promise<{ failures: TaskFailureDigest[]; total: number }> {
  return request(`/api/insights/failures${insightsQuery(filter)}`);
}

export function listInsightsClusters(
  filter: InsightsFilter = {}
): Promise<{ clusters: Cluster[]; total: number }> {
  return request(`/api/insights/clusters${insightsQuery(filter)}`);
}

export function listInsightsManifests(
  filter: InsightsFilter = {}
): Promise<{ manifests: ManifestSummary[]; total: number }> {
  return request(`/api/insights/manifests${insightsQuery(filter)}`);
}

// ---- SSE: live control events --------------------------------------------

/** Per-event payload from the dispatcher's control socket, JSON-decoded. */
export type ControlEnvelope = Record<string, unknown> & {
  event: string;
  actor_path?: string[];
};

export interface SubscribeHandlers {
  onEvent: (envelope: ControlEnvelope) => void;
  onLagged?: (skipped: number) => void;
  onError?: (err: Event) => void;
  onOpen?: () => void;
}

/**
 * Subscribe to a run's live control events. Returns a teardown function;
 * call it to close the EventSource. EventSource does NOT support custom
 * headers, so when auth is enabled the token is appended as `?token=`.
 * Lower security profile than the header (token may surface in logs /
 * referrer / browser history) so the SPA only sends it on this route.
 */
export function subscribeRunEvents(runId: string, handlers: SubscribeHandlers): () => void {
  const tok = getToken();
  const qs = tok ? `?token=${enc(tok)}` : '';
  const url = `/api/runs/${enc(runId)}/events${qs}`;
  const es = new EventSource(url);
  if (handlers.onOpen) es.addEventListener('open', handlers.onOpen);
  if (handlers.onError) es.addEventListener('error', handlers.onError);
  es.addEventListener('control', (ev) => {
    try {
      const data = JSON.parse((ev as MessageEvent).data) as ControlEnvelope;
      handlers.onEvent(data);
    } catch {
      /* skip malformed event */
    }
  });
  if (handlers.onLagged) {
    es.addEventListener('lagged', (ev) => {
      const n = Number((ev as MessageEvent).data);
      if (handlers.onLagged) handlers.onLagged(Number.isFinite(n) ? n : 0);
    });
  }
  return () => es.close();
}
