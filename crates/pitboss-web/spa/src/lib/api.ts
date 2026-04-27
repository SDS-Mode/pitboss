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
