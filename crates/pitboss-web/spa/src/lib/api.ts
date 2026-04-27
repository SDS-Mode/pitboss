// API client for pitboss-web. The Rust server is mounted at the same origin
// in production (rust-embed serves the SPA bundle), and is reached via the
// Vite dev proxy under `/api` during local development.

import { browser } from '$app/environment';

export type RunStatus = 'complete' | 'running' | 'stale' | 'aborted';

export interface RunDto {
  run_id: string;
  status: RunStatus;
  status_label: string;
  mtime_unix: number;
  tasks_total: number;
  tasks_failed: number;
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

export function getTaskLog(runId: string, taskId: string, opts: TaskLogOpts = {}): Promise<string> {
  const params = new URLSearchParams();
  if (opts.limit !== undefined) params.set('limit', String(opts.limit));
  if (opts.tail !== undefined) params.set('tail', String(opts.tail));
  const qs = params.toString();
  const path = `/api/runs/${enc(runId)}/tasks/${enc(taskId)}/log${qs ? `?${qs}` : ''}`;
  return request<string>(path, { accept: 'text' });
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
