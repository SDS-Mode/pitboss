<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import {
    getRun,
    getResolvedManifest,
    getManifestToml,
    getSummaryJsonl,
    getEventsJsonl,
    subscribeRunEvents,
    postControlOp,
    forkRun,
    type ControlEnvelope,
    type RunDetailDto,
    type PolicyRule,
    type WorkerEntry,
    type ActorActivity,
    type SubleadInfo,
    type FailureReason,
    type ResourceSampleEvent,
    type ResourcePressureEvent,
    type ResourceHighWater,
    ApiError
  } from '$lib/api';
  import { formatUnixSeconds, relativeFromUnix } from '$lib/utils';
  import { costUsd, fmtCost } from '$lib/prices';
  import StatusBadge from '$lib/components/status-badge.svelte';
  import ApprovalModal, { type ApprovalRequest } from '$lib/components/approval-modal.svelte';
  import PolicyEditor from '$lib/components/policy-editor.svelte';
  import RunTileGrid from '$lib/components/run-tile-grid.svelte';
  import RunGraph from '$lib/components/run-graph.svelte';
  import RunGraphInspector from '$lib/components/run-graph-inspector.svelte';
  import ResourcesTab from '$lib/components/resources-tab.svelte';
  import type { TaskRecord } from '$lib/api';
  import {
    Card,
    CardContent,
    CardDescription,
    CardHeader,
    CardTitle
  } from '$lib/components/ui/card';
  import { Tabs, TabsContent, TabsList, TabsTrigger } from '$lib/components/ui/tabs';
  import {
    Table,
    TableBody,
    TableCell,
    TableHead,
    TableHeader,
    TableRow
  } from '$lib/components/ui/table';
  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import { Switch } from '$lib/components/ui/switch';
  import { Label } from '$lib/components/ui/label';
  import {
    ArrowLeft,
    ChevronRight,
    RefreshCw,
    AlertTriangle,
    Octagon,
    GitFork,
    Filter
  } from 'lucide-svelte';
  import { browser } from '$app/environment';
  import type { RunStatus } from '$lib/api';

  const runId = $derived(page.params.id ?? '');

  let detail = $state<RunDetailDto | null>(null);
  let manifestToml = $state<string | null>(null);
  let resolved = $state<unknown>(null);
  let summaryJsonl = $state<string | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);

  // Derived view of the run record. summary.json shape is owned by pitboss-core
  // — we treat it loosely so a schema change doesn't break the UI catastrophically.
  const r = $derived(detail as Record<string, any> | null);
  const inProgress = $derived(Boolean(r?.in_progress));
  const summary = $derived(inProgress ? null : r);
  const stub = $derived(inProgress ? (r?.run as Record<string, any> | null) : null);
  // Run status derivation (#365):
  //   - in-progress: defer to `stub.status`, which the API returns from
  //     `pitboss_cli::runs::RunStatus` (the canonical classifier shared
  //     with the overview list — single source of truth).
  //   - finalized: read `was_interrupted` directly from `summary.json`.
  //     `was_interrupted = true` means the run was cancelled (cancel_run
  //     op or Ctrl-C / TUI quit); render as 'cancelled' so the badge
  //     goes red and matches the overview's RunStatus::Cancelled.
  //   - degenerate (no stub, no summary): 'aborted'. This fallback
  //     should be unreachable in practice — the API always returns one
  //     or the other — but keeps the type total.
  //
  // Replaces a pre-#365 derivation that read `summary.run_meta.outcome`,
  // a field that has never existed on `RunSummary`, so cancelled runs
  // silently rendered as 'complete' (green).
  const status = $derived<RunStatus>(
    (stub?.status as RunStatus | undefined) ??
      (summary ? (summary.was_interrupted === true ? 'cancelled' : 'complete') : 'aborted')
  );
  const taskList = $derived<Array<Record<string, any>>>(
    (summary?.tasks as Array<Record<string, any>> | undefined) ?? []
  );

  // Total tokens = sum of input + output across every actor we have
  // a TaskRecord for. Reads from `tasksToRender` (defined below) so it
  // works during in-progress runs (which only have liveTasks) AND
  // post-finalize runs (which have summary.tasks). The pre-fix code
  // read `usage.input_tokens` / `output_tokens` — those keys don't
  // exist on TaskRecord (which uses `input` / `output`), so the card
  // always rendered 0 regardless of run state.
  const totalTokens = $derived.by(() => {
    let sum = 0;
    for (const t of tasksToRender) {
      const usage = t.token_usage as Record<string, number> | undefined;
      if (!usage) continue;
      sum += (usage.input ?? 0) + (usage.output ?? 0);
    }
    return sum;
  });
  // Estimated USD cost summed across every task we have a model + usage
  // for. Computed locally via $lib/prices (mirrors pitboss_core::prices)
  // since neither summary.json nor TaskRecord carries a per-task
  // cost_usd field. Returns null when at least one task contributed but
  // the model wasn't in the price table — the operator should know we
  // couldn't price it rather than seeing a partial total. Returns 0
  // when no tasks have data yet (fresh run).
  const totalCost = $derived.by<number | null>(() => {
    let sum = 0;
    let priced = false;
    for (const t of tasksToRender) {
      const usage = t.token_usage as Record<string, number> | undefined;
      const model = t.model as string | undefined;
      if (!usage || !model) continue;
      const c = costUsd(model, usage);
      if (c === null) return null; // unknown model — refuse to fake a partial total
      sum += c;
      priced = true;
    }
    return priced ? sum : 0;
  });
  // Run wall-clock duration. Pre-fix the fourth card was "Total cost",
  // which read a `cost_usd` field that has never existed on TaskRecord
  // — so it always displayed $0.00. Replaced with elapsed runtime,
  // which is something we actually have data for. Live ticks against
  // `nowMs` (1 s cadence) while the run is in progress; freezes at
  // `total_duration_ms` once the summary lands.
  let nowMs = $state(Date.now());
  $effect(() => {
    if (!inProgress) return;
    const h = setInterval(() => (nowMs = Date.now()), 1000);
    return () => clearInterval(h);
  });
  const runtimeMs = $derived.by(() => {
    if (summary && typeof summary.total_duration_ms === 'number') {
      return summary.total_duration_ms;
    }
    const startStr = (summary?.started_at ?? stub?.started_at) as string | undefined;
    if (!startStr) return null;
    const startMs = Date.parse(startStr);
    if (!Number.isFinite(startMs)) return null;
    return Math.max(0, nowMs - startMs);
  });

  // Parsed JSONL of in-progress task records — lets the Tasks tab show
  // partial state while the run hasn't finalized.
  //
  // Dedupe by task_id, keeping the LAST entry per id. summary.jsonl is
  // append-only and certain failure recovery paths (reprompt → cancel →
  // respawn, hook-failure-on-cancel) can produce multiple rows for the
  // same task_id. Keyed `{#each}` blocks downstream require unique keys,
  // so collapse here at the parse boundary. (#425)
  const liveTasks = $derived<Array<Record<string, any>>>(
    summaryJsonl
      ? Array.from(
          summaryJsonl
            .split('\n')
            .filter((l) => l.trim().length > 0)
            .map((l) => {
              try {
                return JSON.parse(l) as Record<string, any>;
              } catch {
                return {};
              }
            })
            .filter((o) => Object.keys(o).length > 0)
            .reduce((m: Map<string | symbol, Record<string, any>>, o) => {
              // Records without a task_id keep their own slot via Symbol()
              // so we don't collapse unrelated parse-error rows together.
              const key = (o.task_id as string | undefined) ?? Symbol();
              return m.set(key, o);
            }, new Map())
            .values()
        )
      : []
  );

  const tasksToRender = $derived(taskList.length > 0 ? taskList : liveTasks);

  /**
   * Unified worker tile list. The dispatcher's `WorkersSnapshot` only
   * includes actors in active layers — once a sublead terminates,
   * `state.subleads.remove(sublead_id)` fires, taking the sublead AND
   * its sub-tree workers out of the snapshot. Late in a run with
   * short-lived sub-trees, the operator was left with just the root
   * lead in the Workers card.
   *
   * Fix: take the live `workers` list as authoritative for state
   * (running/paused/frozen — those need real-time wire data), then
   * union in entries from `liveTasks` (summary.jsonl) for any task_id
   * not in the live snapshot. summary.jsonl is append-only so it has
   * every actor that's ever been spawned in this run, with correct
   * `parent_task_id` (which the dispatcher's snapshot also gets wrong
   * for the sublead's own row — collect_layer_workers tags every
   * entry with `Some(sublead_id)`, so the sublead becomes its own
   * parent on the wire).
   */
  const allWorkers = $derived.by<WorkerEntry[]>(() => {
    const liveById = new Map(workers.map((w) => [w.task_id, w]));
    const merged: WorkerEntry[] = workers.map((w) => {
      // Patch the sublead-is-its-own-parent dispatcher bug: if a sublead
      // entry's parent_task_id equals its own task_id, drop it; the
      // JSONL fallback (added below for not-in-live ids) carries the
      // correct value, but for live entries we just zero it out so the
      // tile groups under "root" instead of nesting under itself.
      if (w.parent_task_id && w.parent_task_id === w.task_id) {
        return { ...w, parent_task_id: undefined };
      }
      return w;
    });
    for (const t of liveTasks) {
      const id = t.task_id as string | undefined;
      if (!id || liveById.has(id)) continue;
      const status = ((t.status as string | undefined) ?? 'unknown').toLowerCase();
      // Map TaskStatus → tile color buckets used by RunTileGrid.tileColor.
      // Anything not running/paused/frozen renders as terminal.
      const stateStr = status === 'success' ? 'completed' : status === 'failed' ? 'failed' : status;
      merged.push({
        task_id: id,
        state: stateStr,
        prompt_preview: (t.final_message_preview as string | undefined) ?? '',
        started_at: t.started_at as string | undefined,
        parent_task_id: (t.parent_task_id as string | null | undefined) ?? undefined,
        session_id: (t.claude_session_id as string | null | undefined) ?? undefined
      });
    }
    return merged;
  });

  // ---- Phase 2: live control events (SSE) -----------------------------
  // The dispatcher's per-run control socket is bridged to /api/runs/:id/events
  // by `pitboss-web` and fanned out to N browser tabs via tokio broadcast.
  // We only attach an EventSource for in-progress runs — completed runs
  // have nothing live to stream.
  let liveEvents = $state<ControlEnvelope[]>([]);
  let sseStatus = $state<'idle' | 'connecting' | 'open' | 'closed' | 'error'>('idle');
  const MAX_LIVE_EVENTS = 200;

  // ---- SSE event filter (per-tab, persisted) ----------------------------
  // Hide noise (default on): suppress `store_activity` events whose
  // `counters` array is empty. Those fire on a heartbeat from the
  // dispatcher and crowd out signal events when no shared-store ops
  // are happening — the most common noise the operator complained
  // about.
  //
  // disabledKinds tracks event kinds the operator has hidden via the
  // filter UI. Stored in localStorage so refreshes don't reset
  // operator preferences. We keep `disabled` rather than `enabled`
  // so future event kinds added by the dispatcher default to visible
  // (additive — the operator opts out when they get noisy).
  const FILTER_STORAGE_KEY = 'pitboss-sse-filters-v1';
  let hideNoise = $state(true);
  let disabledKinds = $state<Record<string, boolean>>({});
  let filterPanelOpen = $state(false);

  if (browser) {
    try {
      const raw = window.localStorage.getItem(FILTER_STORAGE_KEY);
      if (raw) {
        const parsed = JSON.parse(raw) as {
          hideNoise?: boolean;
          disabledKinds?: Record<string, boolean>;
        };
        if (typeof parsed.hideNoise === 'boolean') hideNoise = parsed.hideNoise;
        if (parsed.disabledKinds) disabledKinds = parsed.disabledKinds;
      }
    } catch {
      /* malformed storage — ignore and use defaults */
    }
  }

  $effect(() => {
    if (!browser) return;
    try {
      window.localStorage.setItem(FILTER_STORAGE_KEY, JSON.stringify({ hideNoise, disabledKinds }));
    } catch {
      /* quota / disabled — silently ignore */
    }
  });

  /** Determine if a given event should be hidden under the current filters. */
  function isHidden(e: ControlEnvelope): boolean {
    if (disabledKinds[e.event]) return true;
    if (
      hideNoise &&
      e.event === 'store_activity' &&
      Array.isArray((e as ControlEnvelope & { counters?: unknown[] }).counters) &&
      ((e as ControlEnvelope & { counters: unknown[] }).counters?.length ?? 0) === 0
    ) {
      return true;
    }
    return false;
  }

  /** Events visible under current filters. Re-derives on every event arrival. */
  const visibleEvents = $derived.by(() => liveEvents.filter((e) => !isHidden(e)));

  /** All event kinds the operator has seen this session, sorted alphabetically. */
  const seenKinds = $derived.by(() => {
    const s = new Set<string>();
    for (const e of liveEvents) s.add(e.event);
    return [...s].sort();
  });

  function toggleKind(kind: string) {
    disabledKinds = { ...disabledKinds, [kind]: !disabledKinds[kind] };
  }

  const hiddenCount = $derived(liveEvents.length - visibleEvents.length);

  // ---- Replay tab: persisted events.jsonl (#259) ----------------------
  // For finalized runs, the Live SSE bridge has closed but the
  // dispatcher persisted every envelope to `<run-dir>/events.jsonl`
  // (when `[run].emit_event_stream = true`). The Replay tab fetches
  // that file via GET /api/runs/:id/events-jsonl and renders the
  // same scrollable event list as Live, sharing the filter state so
  // an operator's "hide store_activity" preference carries between
  // live and historical inspection.
  //
  // Fetched lazily on first tab open (or via the Reload button) —
  // not at page mount — because a finalized run that didn't enable
  // emit_event_stream shouldn't pay the 404 round-trip until the
  // operator actually clicks Replay.
  let replayEvents = $state<ControlEnvelope[]>([]);
  let replayLoaded = $state(false);
  let replayLoading = $state(false);
  let replayError = $state<string | null>(null);

  async function loadReplay() {
    replayLoading = true;
    replayError = null;
    try {
      replayEvents = await getEventsJsonl(runId);
      replayLoaded = true;
    } catch (e) {
      replayError = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
    } finally {
      replayLoading = false;
    }
  }

  const visibleReplayEvents = $derived.by(() => replayEvents.filter((e) => !isHidden(e)));
  const replayHiddenCount = $derived(replayEvents.length - visibleReplayEvents.length);
  /** Replay-specific kind set, separate from `seenKinds` (which is
   * scoped to live events). Used to populate the Replay tab's
   * filter panel so the kind chips reflect what's actually in the
   * historical log, not what arrived on this session's SSE. */
  const replaySeenKinds = $derived.by(() => {
    const s = new Set<string>();
    for (const e of replayEvents) s.add(e.event);
    return [...s].sort();
  });

  // ---- Phase 3: control state derived from the live event stream ------
  // The dispatcher pushes typed events; we keep the latest snapshot of
  // each kind we render in the UI. None of this needs to round-trip to
  // disk — refreshing the page rebuilds it from the next Hello +
  // WorkersSnapshot pair.
  let workers = $state<WorkerEntry[]>([]);
  /** actor_id → store-op counters from the latest StoreActivity event. */
  let storeActivity = $state<Record<string, ActorActivity>>({});
  /** task_id → failure reason from the most recent WorkerFailed for that worker. */
  let failures = $state<Record<string, FailureReason>>({});
  /** sublead_id → snapshot built from SubleadSpawned (+ Terminated). */
  let subleads = $state<Record<string, SubleadInfo>>({});
  let policyRules = $state<PolicyRule[]>([]);
  let serverVersion = $state<string | null>(null);
  let pendingApprovals = $state<ApprovalRequest[]>([]);
  let activeApproval = $state<ApprovalRequest | null>(null);
  // #553: latest in-flight resource sample + the rolling buffer the
  // Resources tab uses for sparklines. `pressureBanner` is non-null
  // when the live banner should render on the Live tab.
  let latestResourceSample = $state<ResourceSampleEvent | null>(null);
  let resourceSamples = $state<Array<{ envelope: ResourceSampleEvent; at: number }>>([]);
  let pressureBanner = $state<ResourcePressureEvent | null>(null);
  // Finalize-time roll-up surfaced from `summary.json`. `RunDetailDto`
  // already passes through any extra fields the backend adds, so this
  // is just a typed view onto `detail.resource_high_water`.
  const resourceHighWater = $derived(
    (detail?.resource_high_water as ResourceHighWater | undefined) ?? null
  );
  // Banner shown when ANOTHER client takes over our slot (we get
  // `Superseded` from the dispatcher right before the socket closes).
  let superseded = $state(false);

  // #260: graph node inspector — selected actor id (null = closed).
  // Bound through to `RunGraph` (drives the ring on the matching node)
  // and `RunGraphInspector` (drives the side panel).
  let selectedTaskId = $state<string | null>(null);
  const selectedTask = $derived(
    selectedTaskId
      ? (tasksToRender.find((t) => t.task_id === selectedTaskId) as TaskRecord | undefined)
      : undefined
  );
  const selectedWorker = $derived(
    selectedTaskId ? allWorkers.find((w) => w.task_id === selectedTaskId) : undefined
  );
  let opFeedback = $state<{ kind: 'ok' | 'err'; text: string } | null>(null);
  let opFeedbackTimer: ReturnType<typeof setTimeout> | null = null;

  function flashOp(kind: 'ok' | 'err', text: string) {
    opFeedback = { kind, text };
    if (opFeedbackTimer) clearTimeout(opFeedbackTimer);
    opFeedbackTimer = setTimeout(() => (opFeedback = null), 4000);
  }

  $effect(() => {
    // Promote next pending approval into the modal slot whenever the
    // current one is dismissed. Keeps a queue if multiple workers fire
    // at once (rare, but legal).
    if (!activeApproval && pendingApprovals.length > 0) {
      activeApproval = pendingApprovals[0];
      pendingApprovals = pendingApprovals.slice(1);
    }
  });

  function ingest(e: ControlEnvelope) {
    switch (e.event) {
      case 'hello': {
        const ev = e as ControlEnvelope & { server_version?: string; policy_rules?: PolicyRule[] };
        serverVersion = ev.server_version ?? null;
        policyRules = Array.isArray(ev.policy_rules) ? ev.policy_rules : [];
        superseded = false;
        break;
      }
      case 'workers_snapshot': {
        const ev = e as ControlEnvelope & { workers?: WorkerEntry[] };
        workers = Array.isArray(ev.workers) ? ev.workers : [];
        break;
      }
      case 'store_activity': {
        const ev = e as ControlEnvelope & { counters?: ActorActivity[] };
        const next: Record<string, ActorActivity> = {};
        for (const c of ev.counters ?? []) next[c.actor_id] = c;
        storeActivity = next;
        break;
      }
      case 'worker_failed': {
        const ev = e as ControlEnvelope & {
          task_id?: string;
          reason?: FailureReason;
        };
        if (ev.task_id && ev.reason) {
          failures = { ...failures, [ev.task_id]: ev.reason };
        }
        break;
      }
      case 'sublead_spawned': {
        const ev = e as ControlEnvelope & SubleadInfo;
        if (ev.sublead_id) {
          subleads = {
            ...subleads,
            [ev.sublead_id]: {
              sublead_id: ev.sublead_id,
              budget_usd: ev.budget_usd ?? null,
              max_workers: ev.max_workers ?? null,
              read_down: ev.read_down ?? false
            }
          };
        }
        break;
      }
      case 'sublead_terminated': {
        const ev = e as ControlEnvelope & {
          sublead_id?: string;
          spent_usd?: number;
          unspent_usd?: number;
          outcome?: string;
        };
        if (ev.sublead_id && subleads[ev.sublead_id]) {
          subleads = {
            ...subleads,
            [ev.sublead_id]: {
              ...subleads[ev.sublead_id],
              outcome: ev.outcome,
              spent_usd: ev.spent_usd,
              unspent_usd: ev.unspent_usd
            }
          };
        }
        break;
      }
      case 'approval_request': {
        const req = e as unknown as ApprovalRequest;
        if (activeApproval || pendingApprovals.some((p) => p.request_id === req.request_id)) {
          // Don't double-queue.
          if (!activeApproval || activeApproval.request_id !== req.request_id) {
            pendingApprovals = [...pendingApprovals, req];
          }
        } else {
          activeApproval = req;
        }
        break;
      }
      case 'op_acked': {
        const ev = e as ControlEnvelope & { op?: string; task_id?: string };
        flashOp('ok', `${ev.op}${ev.task_id ? ` · ${ev.task_id}` : ''} acknowledged`);
        break;
      }
      case 'op_failed': {
        const ev = e as ControlEnvelope & { op?: string; task_id?: string; error?: string };
        flashOp('err', `${ev.op} failed: ${ev.error ?? 'unknown error'}`);
        break;
      }
      case 'op_unknown_state': {
        const ev = e as ControlEnvelope & { op?: string; current_state?: string };
        flashOp('err', `${ev.op} rejected — worker is ${ev.current_state}`);
        break;
      }
      case 'superseded':
        superseded = true;
        break;
      case 'resource_sample': {
        // #553: per-tick resource samples flow through the same wire
        // as everything else. Keep the latest sample for the Resources
        // tab + a bounded ring buffer for the per-actor sparklines so
        // a long-running session doesn't grow unboundedly. 240 entries
        // at the default 5 s cadence is 20 minutes of history — past
        // that the operator should switch to the events.jsonl replay.
        const ev = e as ControlEnvelope & ResourceSampleEvent;
        latestResourceSample = ev;
        if (resourceSamples.length >= 240) resourceSamples.shift();
        // Prefer the server-side wire timestamp (#580 FU-4) so the
        // chart's X-axis matches sample order, not receive order, and
        // events.jsonl replay plots at the original cadence. Fall
        // back to receive time for pre-v0.17 envelopes where the
        // field is absent or zero.
        const at =
          ev.sampled_at_unix_ms && ev.sampled_at_unix_ms > 0
            ? ev.sampled_at_unix_ms
            : Date.now();
        resourceSamples.push({ envelope: ev, at });
        resourceSamples = resourceSamples; // trigger reactivity
        break;
      }
      case 'resource_pressure': {
        const ev = e as ControlEnvelope & ResourcePressureEvent;
        // Banner reflects the most recent NON-clear event. A clear
        // event dismisses the banner; warn/error replace it.
        if (ev.level === 'clear') {
          pressureBanner = null;
        } else {
          pressureBanner = ev;
        }
        break;
      }
    }
  }

  $effect(() => {
    if (!runId || !inProgress) {
      sseStatus = 'idle';
      return;
    }
    sseStatus = 'connecting';
    liveEvents = [];
    workers = [];
    storeActivity = {};
    failures = {};
    subleads = {};
    policyRules = [];
    pendingApprovals = [];
    activeApproval = null;
    superseded = false;
    const teardown = subscribeRunEvents(runId, {
      onOpen: () => {
        sseStatus = 'open';
        // The dispatcher emits WorkersSnapshot only in response to
        // ListWorkers, not proactively. Without this the Workers card
        // sits at "Waiting for first snapshot…" for the whole run.
        // Same for store_activity — fire once on connect so the panel
        // has something before the first heartbeat.
        void postControlOp(runId, { op: 'list_workers' }).catch(() => {});
      },
      onError: () => (sseStatus = 'error'),
      onEvent: (envelope) => {
        ingest(envelope);
        liveEvents = [envelope, ...liveEvents].slice(0, MAX_LIVE_EVENTS);
      },
      onLagged: (skipped) => {
        liveEvents = [{ event: 'lagged', skipped } as ControlEnvelope, ...liveEvents].slice(
          0,
          MAX_LIVE_EVENTS
        );
      }
    });
    return () => {
      teardown();
      sseStatus = 'closed';
    };
  });

  // ---- In-progress polling ---------------------------------------------
  // Two pieces of UI state that DON'T derive from the SSE event stream:
  //
  //   1. summary.jsonl on disk — the per-task TaskRecord append log. Task
  //      counts, costs, tokens, exit codes, durations all read from here.
  //      The dispatcher appends as each actor finishes; without polling
  //      the page only ever sees what was on disk at mount time.
  //
  //   2. WorkersSnapshot — emitted on demand in response to a list_workers
  //      op, never proactively. New workers added/removed mid-run aren't
  //      visible until we ask.
  //
  // Poll every 3 s while the run is in-progress. 3 s is a tradeoff: fast
  // enough that the operator sees workers appearing within a refresh
  // tick, slow enough that a tab left open all day doesn't burn cycles.
  // The interval clears as soon as inProgress flips false (run finalized
  // and the page swaps over to the static summary.json view).
  const POLL_INTERVAL_MS = 3000;
  $effect(() => {
    if (!runId || !inProgress) return;
    const tick = async () => {
      try {
        summaryJsonl = await getSummaryJsonl(runId);
      } catch {
        /* run may have just finalized — next render uses summary.json */
      }
      // Re-fetch the top-level run record so the page transitions
      // gracefully when the dispatcher writes the finalised summary:
      // `r.in_progress` flips false, `inProgress` flips, and the UI
      // swaps from the Live view to the Tasks/Manifest/etc. view
      // without a manual reload. Without this, the page sits stuck
      // in the Live view with frozen data after finalization.
      try {
        detail = await getRun(runId);
      } catch {
        /* network blip — keep the last good record */
      }
      // Fire-and-forget; the WorkersSnapshot reply lands via SSE.
      void postControlOp(runId, { op: 'list_workers' }).catch(() => {});
    };
    const handle = setInterval(tick, POLL_INTERVAL_MS);
    return () => clearInterval(handle);
  });

  async function sendOp(opPromise: Promise<void>, label: string) {
    try {
      await opPromise;
      // Don't flash here — wait for OpAcked to land via SSE so the
      // operator sees the dispatcher actually accepted it. If the POST
      // failed, the catch path flashes.
    } catch (e) {
      const msg = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
      flashOp('err', `${label}: ${msg}`);
    }
  }

  function cancelRun() {
    if (!confirm('Cancel the entire run? All in-flight workers will be aborted.')) return;
    sendOp(postControlOp(runId, { op: 'cancel_run' }), 'cancel_run');
  }
  function cancelWorker(task_id: string) {
    sendOp(postControlOp(runId, { op: 'cancel_worker', task_id }), `cancel_worker ${task_id}`);
  }
  function pauseWorker(task_id: string) {
    sendOp(
      postControlOp(runId, { op: 'pause_worker', task_id, mode: 'freeze' }),
      `pause_worker ${task_id}`
    );
  }
  function continueWorker(task_id: string) {
    sendOp(postControlOp(runId, { op: 'continue_worker', task_id }), `continue_worker ${task_id}`);
  }
  function repromptWorker(task_id: string) {
    const prompt = window.prompt('New prompt for the worker?');
    if (!prompt || !prompt.trim()) return;
    sendOp(
      postControlOp(runId, { op: 'reprompt_worker', task_id, prompt: prompt.trim() }),
      `reprompt_worker ${task_id}`
    );
  }

  async function fork() {
    const suggested = `fork-of-${runId.slice(0, 8)}`;
    const newName = window.prompt(
      'Save this run’s manifest into the workspace as (without .toml)?',
      suggested
    );
    if (!newName || !newName.trim()) return;
    try {
      const res = await forkRun(runId, newName.trim());
      await goto(`/manifests/${encodeURIComponent(res.name)}`);
    } catch (e) {
      const msg = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
      window.alert(`Fork failed: ${msg}`);
    }
  }

  async function load() {
    loading = true;
    error = null;
    detail = null;
    manifestToml = null;
    resolved = null;
    summaryJsonl = null;

    try {
      detail = await getRun(runId);
    } catch (e) {
      error = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
      loading = false;
      return;
    }

    // Best-effort parallel fetches; missing artifacts are fine.
    const [m, rj, sj] = await Promise.allSettled([
      getManifestToml(runId),
      getResolvedManifest(runId),
      getSummaryJsonl(runId)
    ]);
    if (m.status === 'fulfilled') manifestToml = m.value;
    if (rj.status === 'fulfilled') resolved = rj.value;
    if (sj.status === 'fulfilled') summaryJsonl = sj.value;

    loading = false;
  }

  $effect(() => {
    if (runId) load();
  });

  function fmtDuration(ms?: number): string {
    if (typeof ms !== 'number' || ms <= 0) return '—';
    const s = Math.floor(ms / 1000);
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m}m ${s % 60}s`;
    return `${Math.floor(m / 60)}h ${m % 60}m`;
  }

  function taskState(t: Record<string, any>): string {
    return (t.status as string | undefined) ?? (t.state as string | undefined) ?? 'unknown';
  }
</script>

<svelte:head>
  <title>Run {runId.slice(0, 8)}… — Pitboss</title>
</svelte:head>

<div class="mb-4 flex items-center gap-3 text-sm">
  <Button variant="ghost" size="sm" href="/">
    <ArrowLeft class="mr-1.5 size-4" /> All runs
  </Button>
  <ChevronRight class="text-muted-foreground size-4" />
  <code class="text-xs">{runId}</code>
</div>

{#if error}
  <Card class="border-destructive/50">
    <CardContent class="flex items-start gap-3 pt-6">
      <AlertTriangle class="text-destructive mt-0.5 size-5 shrink-0" />
      <div>
        <p class="text-destructive font-medium">Failed to load run</p>
        <p class="text-muted-foreground mt-1 text-sm">{error}</p>
      </div>
    </CardContent>
  </Card>
{:else if loading && !detail}
  <Card>
    <CardContent class="text-muted-foreground py-12 text-center text-sm">Loading run…</CardContent>
  </Card>
{:else if detail}
  <div class="mb-6 flex items-start justify-between gap-4">
    <div>
      <div class="mb-2 flex items-center gap-3">
        <h1 class="text-xl font-semibold tracking-tight">Run detail</h1>
        <StatusBadge {status} />
        {#if inProgress}
          <Badge variant="outline" class="text-xs">in progress</Badge>
        {/if}
      </div>
      <p class="text-muted-foreground text-xs">
        {#if summary?.started_at}
          Started {summary.started_at}
        {:else if stub}
          Last activity {relativeFromUnix(stub.mtime_unix)}
        {/if}
        {#if summary?.ended_at}
          · Ended {summary.ended_at}
        {/if}
        {#if runtimeMs !== null}
          · Runtime <span class="tabular-nums">{fmtDuration(runtimeMs)}</span>
        {/if}
      </p>
    </div>
    <div class="flex items-center gap-2">
      <Button variant="outline" size="sm" onclick={fork} disabled={!manifestToml}>
        <GitFork class="mr-2 size-4" />
        Fork manifest
      </Button>
      <Button variant="outline" size="sm" onclick={load} disabled={loading}>
        <RefreshCw class="mr-2 size-4 {loading ? 'animate-spin' : ''}" />
        Refresh
      </Button>
    </div>
  </div>

  <div class="mb-6 grid grid-cols-2 gap-3 sm:grid-cols-4">
    <Card>
      <CardHeader class="pb-2">
        <CardDescription>Tasks</CardDescription>
        <CardTitle class="text-2xl">{tasksToRender.length}</CardTitle>
      </CardHeader>
    </Card>
    <Card>
      <CardHeader class="pb-2">
        <CardDescription>Failed</CardDescription>
        <CardTitle class="text-2xl">
          {tasksToRender.filter((t) => taskState(t) === 'failed').length}
        </CardTitle>
      </CardHeader>
    </Card>
    <Card>
      <CardHeader class="pb-2">
        <CardDescription>Cost (est.)</CardDescription>
        <CardTitle class="text-2xl tabular-nums">{fmtCost(totalCost)}</CardTitle>
      </CardHeader>
    </Card>
    <Card>
      <CardHeader class="pb-2">
        <CardDescription>Tokens</CardDescription>
        <CardTitle class="text-2xl tabular-nums">{totalTokens.toLocaleString()}</CardTitle>
      </CardHeader>
    </Card>
  </div>

  <Tabs value={inProgress ? 'live' : 'tasks'} class="w-full">
    <TabsList>
      {#if inProgress}
        <TabsTrigger value="live">
          Live
          <span
            class="ml-2 inline-block size-2 rounded-full {sseStatus === 'open'
              ? 'bg-emerald-500 animate-pulse'
              : sseStatus === 'connecting'
                ? 'bg-amber-500'
                : sseStatus === 'error'
                  ? 'bg-red-500'
                  : 'bg-muted-foreground'}"
            aria-hidden="true"
          ></span>
        </TabsTrigger>
      {/if}
      <TabsTrigger value="graph">Graph</TabsTrigger>
      <TabsTrigger value="tasks">Tasks ({tasksToRender.length})</TabsTrigger>
      <TabsTrigger value="resources">
        Resources
        {#if pressureBanner && pressureBanner.level !== 'clear'}
          <span class="ml-1 text-amber-600" aria-hidden="true">⚠</span>
        {:else if resourceHighWater && (resourceHighWater.peak_utilization_pct ?? 0) >= 0.8}
          <span class="ml-1 text-amber-600" aria-hidden="true">⚠</span>
        {/if}
      </TabsTrigger>
      {#if !inProgress}
        <TabsTrigger value="replay">Replay</TabsTrigger>
      {/if}
      <TabsTrigger value="manifest">Manifest</TabsTrigger>
      <TabsTrigger value="resolved">Resolved</TabsTrigger>
      <TabsTrigger value="summary">Summary JSON</TabsTrigger>
    </TabsList>

    <!--
      The Live tab (event stream + Filter UI + Workers card) is
      gated on inProgress on purpose: SSE closes when the
      dispatcher exits, so a finalised run has nothing live to
      stream. The 3-s polling tick re-fetches the run record
      (see effect above), so this gate flips at finalize without
      a manual reload. The historical equivalent is the Replay
      tab (#259) below, which reads the persisted events.jsonl.
    -->
    {#if inProgress}
      <TabsContent value="live" class="mt-4 space-y-4">
        {#if superseded}
          <Card class="border-amber-500/50 bg-amber-500/5">
            <CardContent class="flex items-start gap-3 pt-6">
              <AlertTriangle class="mt-0.5 size-5 shrink-0 text-amber-600" />
              <div>
                <p class="font-medium text-amber-700 dark:text-amber-300">Control taken</p>
                <p class="text-muted-foreground mt-1 text-sm">
                  Another client (TUI or another browser) connected to this run's control socket and
                  superseded ours. Read-only views still work.
                  <Button
                    variant="link"
                    class="ml-1 h-auto p-0 text-sm"
                    onclick={() => (superseded = false)}
                  >
                    Reconnect
                  </Button>
                </p>
              </div>
            </CardContent>
          </Card>
        {/if}

        <Card>
          <CardHeader class="pb-3">
            <div class="flex items-center justify-between gap-3">
              <div>
                <CardTitle class="text-base">Run controls</CardTitle>
                <CardDescription class="text-xs">
                  Dispatcher: <span class="font-mono">{serverVersion ?? '—'}</span>
                </CardDescription>
              </div>
              <Button
                variant="destructive"
                size="sm"
                onclick={cancelRun}
                disabled={superseded || sseStatus !== 'open'}
              >
                <Octagon class="mr-1.5 size-4" />
                Cancel run
              </Button>
            </div>
          </CardHeader>
          {#if opFeedback}
            <CardContent class="pt-0">
              <div
                class="rounded border px-3 py-2 text-xs {opFeedback.kind === 'ok'
                  ? 'border-emerald-500/40 bg-emerald-500/5 text-emerald-700 dark:text-emerald-300'
                  : 'border-destructive/50 bg-destructive/5 text-destructive'}"
              >
                {opFeedback.text}
              </div>
            </CardContent>
          {/if}
        </Card>

        {#if pressureBanner && pressureBanner.level !== 'clear'}
          <!--
            #553 live memory-pressure banner. Mounted above Workers
            so it doesn't push the run controls off-screen. Cleared
            automatically when the watcher emits a `clear`
            transition; clicking the Resources tab gives the
            operator the full chart + per-actor breakdown.
          -->
          <Card
            class={pressureBanner.level === 'error'
              ? 'border-destructive/50 bg-destructive/5'
              : 'border-amber-500/50 bg-amber-500/5'}
          >
            <CardContent class="flex items-start gap-3 pt-6">
              <AlertTriangle
                class={pressureBanner.level === 'error'
                  ? 'mt-0.5 size-5 shrink-0 text-destructive'
                  : 'mt-0.5 size-5 shrink-0 text-amber-600'}
              />
              <div class="text-sm">
                <p class="font-medium">
                  Memory pressure: {pressureBanner.level}
                </p>
                <p class="text-muted-foreground mt-1">
                  {pressureBanner.message ?? ''}
                </p>
              </div>
            </CardContent>
          </Card>
        {/if}

        <Card>
          <CardHeader class="pb-3">
            <CardTitle class="text-base">
              Workers
              <Badge variant="outline" class="ml-2 text-xs">{allWorkers.length}</Badge>
              {#if Object.keys(subleads).length > 0}
                <Badge variant="outline" class="ml-1 text-xs">
                  {Object.keys(subleads).length} sublead{Object.keys(subleads).length === 1
                    ? ''
                    : 's'}
                </Badge>
              {/if}
            </CardTitle>
            <CardDescription class="text-xs">
              Live state from `WorkersSnapshot`; terminated actors filled in from `summary.jsonl` so
              sub-tree workers stay visible after their sublead exits.
            </CardDescription>
          </CardHeader>
          <CardContent class="pt-0">
            {#if allWorkers.length === 0}
              <p class="text-muted-foreground py-4 text-center text-xs">
                {sseStatus === 'open' ? 'No workers reported yet.' : 'Waiting for first snapshot…'}
              </p>
            {:else}
              <RunTileGrid
                workers={allWorkers}
                {storeActivity}
                {failures}
                {subleads}
                disabled={superseded}
                onPause={pauseWorker}
                onContinue={continueWorker}
                onReprompt={repromptWorker}
                onCancel={cancelWorker}
              />
            {/if}
          </CardContent>
        </Card>

        <PolicyEditor {runId} initialRules={policyRules} />

        <Card>
          <CardHeader class="flex-row items-start justify-between gap-2 pb-2 space-y-0">
            <div>
              <CardTitle class="text-base">Event stream</CardTitle>
              <CardDescription class="text-xs">
                SSE bridge: <span class="font-mono">{sseStatus}</span>
                · {visibleEvents.length} of {liveEvents.length} event{liveEvents.length === 1
                  ? ''
                  : 's'}{#if liveEvents.length === MAX_LIVE_EVENTS}
                  (latest only){/if}{#if hiddenCount > 0}
                  <span class="text-muted-foreground/70"> · {hiddenCount} hidden</span>{/if}
              </CardDescription>
            </div>
            <Button
              variant={filterPanelOpen ? 'default' : 'outline'}
              size="sm"
              onclick={() => (filterPanelOpen = !filterPanelOpen)}
            >
              <Filter class="mr-1.5 size-3.5" /> Filters
            </Button>
          </CardHeader>
          {#if filterPanelOpen}
            <div class="border-border/60 mx-6 mb-2 rounded-md border p-3 text-xs">
              <div class="mb-2 flex items-center gap-2">
                <Switch id="sse-hide-noise" bind:checked={hideNoise} />
                <Label for="sse-hide-noise" class="cursor-pointer text-xs">
                  Hide noise (empty <code>store_activity</code> heartbeats)
                </Label>
              </div>
              {#if seenKinds.length > 0}
                <div class="text-muted-foreground mb-1.5 text-[11px]">Event kinds</div>
                <div class="flex flex-wrap gap-1.5">
                  {#each seenKinds as k (k)}
                    <button
                      onclick={() => toggleKind(k)}
                      class="rounded border px-2 py-0.5 font-mono text-[11px] {disabledKinds[k]
                        ? 'border-muted-foreground/20 text-muted-foreground/50 line-through'
                        : 'border-sky-500/40 text-sky-700 dark:text-sky-400'}"
                    >
                      {k}
                    </button>
                  {/each}
                </div>
              {:else}
                <p class="text-muted-foreground text-[11px]">
                  No events seen yet — kinds appear here as they arrive.
                </p>
              {/if}
            </div>
          {/if}
          <CardContent class="pt-0">
            {#if liveEvents.length === 0}
              <p class="text-muted-foreground py-4 text-center text-sm">
                {sseStatus === 'open'
                  ? 'Waiting for first event…'
                  : sseStatus === 'error'
                    ? 'Connection failed. Run may have ended or dispatcher is unreachable.'
                    : 'Connecting…'}
              </p>
            {:else if visibleEvents.length === 0}
              <p class="text-muted-foreground py-4 text-center text-sm">
                All {liveEvents.length} event{liveEvents.length === 1 ? ' is' : 's are'} hidden by current
                filters.
              </p>
            {:else}
              <div class="max-h-[40vh] space-y-1 overflow-auto font-mono text-xs">
                {#each visibleEvents as e, idx (idx)}
                  <div class="bg-muted/30 rounded border-l-2 border-sky-500/40 px-2 py-1">
                    <span class="text-sky-700 dark:text-sky-400">{e.event}</span>
                    {#if e.actor_path && Array.isArray(e.actor_path) && e.actor_path.length > 0}
                      <span class="text-muted-foreground ml-2">{e.actor_path.join('/')}</span>
                    {/if}
                    <pre
                      class="text-muted-foreground mt-0.5 overflow-x-auto whitespace-pre-wrap text-[11px]">{JSON.stringify(
                        e,
                        null,
                        2
                      )}</pre>
                  </div>
                {/each}
              </div>
            {/if}
          </CardContent>
        </Card>
      </TabsContent>
    {/if}

    <!--
      Graph tab is intentionally OUTSIDE the inProgress gate (#260):
      the actor topology + per-actor inspector should remain useful for
      post-mortems on finalized runs, not just live ones. Live overlays
      (animated edges, store-activity counters) gracefully degrade to
      empty maps when SSE is dormant; per-actor TaskRecord data comes
      from `summary.jsonl` which is live-appended and survives finalize.
    -->
    <TabsContent value="graph" class="mt-4">
      <Card>
        <CardHeader class="pb-3">
          <CardTitle class="text-base">Run hierarchy</CardTitle>
          <CardDescription class="text-xs">
            Actor tree laid out via Dagre. Click a node to open the inspector.
            {#if inProgress}
              Animated edges trace `running` workers; sublead nodes carry the layers icon.
            {:else}
              Showing every actor recorded for this run, including cancelled / failed states.
            {/if}
          </CardDescription>
        </CardHeader>
        <CardContent class="pt-0">
          <RunGraph
            workers={allWorkers}
            {storeActivity}
            {failures}
            {subleads}
            tasks={tasksToRender as TaskRecord[]}
            {selectedTaskId}
            onSelect={(id) => (selectedTaskId = id)}
          />
        </CardContent>
      </Card>
    </TabsContent>

    <TabsContent value="tasks" class="mt-4">
      <Card>
        {#if tasksToRender.length === 0}
          <CardContent class="text-muted-foreground py-12 text-center text-sm">
            No task records yet.
          </CardContent>
        {:else}
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Task</TableHead>
                <TableHead class="w-[10ch]">Status</TableHead>
                <TableHead>Model</TableHead>
                <TableHead class="w-[12ch] text-right">Tokens</TableHead>
                <TableHead class="w-[10ch] text-right">Cost (est.)</TableHead>
                <TableHead class="w-[10ch] text-right">Duration</TableHead>
                <TableHead class="w-[8ch]">Log</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {#each tasksToRender as t, idx ((t.task_id ?? '') + '#' + idx)}
                <TableRow>
                  <TableCell>
                    <code class="text-xs">{t.task_id ?? '—'}</code>
                    {#if t.parent_task_id}
                      <span class="text-muted-foreground ml-1 text-xs">
                        ← {t.parent_task_id}
                      </span>
                    {/if}
                  </TableCell>
                  <TableCell>
                    <Badge
                      variant={taskState(t) === 'completed'
                        ? 'secondary'
                        : taskState(t) === 'failed'
                          ? 'destructive'
                          : 'outline'}
                    >
                      {taskState(t)}
                    </Badge>
                  </TableCell>
                  <TableCell class="text-muted-foreground text-xs">{t.model ?? '—'}</TableCell>
                  <TableCell class="text-right tabular-nums text-xs">
                    {#if t.token_usage}
                      {(
                        ((t.token_usage as Record<string, number>).input ?? 0) +
                        ((t.token_usage as Record<string, number>).output ?? 0)
                      ).toLocaleString()}
                    {:else}—{/if}
                  </TableCell>
                  <TableCell class="text-right tabular-nums text-xs">
                    {fmtCost(
                      costUsd(
                        t.model as string | undefined,
                        t.token_usage as Record<string, number> | undefined
                      )
                    )}
                  </TableCell>
                  <TableCell class="text-right tabular-nums">{fmtDuration(t.duration_ms)}</TableCell
                  >
                  <TableCell>
                    {#if t.task_id}
                      <a
                        href="/runs/{runId}/tasks/{t.task_id}"
                        class="text-primary text-xs hover:underline">View</a
                      >
                    {/if}
                  </TableCell>
                </TableRow>
              {/each}
            </TableBody>
          </Table>
        {/if}
      </Card>
    </TabsContent>

    <TabsContent value="resources" class="mt-4 space-y-4">
      <!--
        Resources tab (#553): live sample stream (when inProgress)
        plus the finalize-time `resource_high_water` roll-up. The
        sub-component owns chart, table, and headline banner; this
        page only feeds it the wire-derived state.
      -->
      <ResourcesTab
        samples={resourceSamples}
        latest={latestResourceSample}
        pressure={pressureBanner}
        highWater={resourceHighWater}
        {inProgress}
      />
    </TabsContent>

    <!--
      Replay tab (#259 PR-K): renders the persisted
      `events.jsonl` written by the dispatcher when
      `[run].emit_event_stream = true`. Only available on
      finalized runs — for in-progress runs the Live tab is the
      authoritative event surface. The same filter UI (hide noise,
      kind chips) applies, sharing state with Live so an
      operator's preferences carry across.
    -->
    {#if !inProgress}
      <TabsContent value="replay" class="mt-4 space-y-4">
        {#if !replayLoaded && !replayLoading}
          <Card>
            <CardContent class="flex items-start justify-between gap-3 pt-6">
              <div class="space-y-1">
                <p class="text-sm font-medium">Persisted control-event stream</p>
                <p class="text-muted-foreground text-xs">
                  Loads <code>events.jsonl</code> from the run directory. Available when the
                  manifest enabled <code>[run].emit_event_stream</code>.
                </p>
              </div>
              <Button size="sm" onclick={() => void loadReplay()}>Load events</Button>
            </CardContent>
          </Card>
        {:else}
          <Card>
            <CardHeader class="flex-row items-start justify-between gap-2 pb-2 space-y-0">
              <div>
                <CardTitle class="text-base">Replay</CardTitle>
                <CardDescription class="text-xs">
                  {#if replayLoading}
                    Loading…
                  {:else if replayError}
                    <span class="text-destructive">{replayError}</span>
                  {:else if replayEvents.length === 0}
                    No persisted envelopes — the run was dispatched with
                    <code>emit_event_stream</code> disabled, or no events were ever emitted.
                  {:else}
                    {visibleReplayEvents.length} of {replayEvents.length} envelope{replayEvents.length ===
                    1
                      ? ''
                      : 's'}{#if replayHiddenCount > 0}
                      <span class="text-muted-foreground/70">
                        · {replayHiddenCount} hidden</span
                      >{/if}
                  {/if}
                </CardDescription>
              </div>
              <div class="flex gap-2">
                <Button
                  variant={filterPanelOpen ? 'default' : 'outline'}
                  size="sm"
                  onclick={() => (filterPanelOpen = !filterPanelOpen)}
                  disabled={replayEvents.length === 0}
                >
                  <Filter class="mr-1.5 size-3.5" /> Filters
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onclick={() => void loadReplay()}
                  disabled={replayLoading}
                >
                  <RefreshCw class="mr-1.5 size-3.5" /> Reload
                </Button>
              </div>
            </CardHeader>
            {#if filterPanelOpen && replayEvents.length > 0}
              <div class="border-border/60 mx-6 mb-2 rounded-md border p-3 text-xs">
                <div class="mb-2 flex items-center gap-2">
                  <Switch id="replay-hide-noise" bind:checked={hideNoise} />
                  <Label for="replay-hide-noise" class="cursor-pointer text-xs">
                    Hide noise (empty <code>store_activity</code> heartbeats)
                  </Label>
                </div>
                {#if replaySeenKinds.length > 0}
                  <div class="text-muted-foreground mb-1.5 text-[11px]">Event kinds</div>
                  <div class="flex flex-wrap gap-1.5">
                    {#each replaySeenKinds as k (k)}
                      <button
                        onclick={() => toggleKind(k)}
                        class="rounded border px-2 py-0.5 font-mono text-[11px] {disabledKinds[k]
                          ? 'border-muted-foreground/20 text-muted-foreground/50 line-through'
                          : 'border-sky-500/40 text-sky-700 dark:text-sky-400'}"
                      >
                        {k}
                      </button>
                    {/each}
                  </div>
                {/if}
              </div>
            {/if}
            <CardContent class="pt-0">
              {#if replayLoading}
                <p class="text-muted-foreground py-4 text-center text-sm">Loading events…</p>
              {:else if replayEvents.length === 0 && !replayError}
                <p class="text-muted-foreground py-4 text-center text-sm">
                  No persisted envelopes found.
                </p>
              {:else if visibleReplayEvents.length === 0}
                <p class="text-muted-foreground py-4 text-center text-sm">
                  All {replayEvents.length} envelope{replayEvents.length === 1 ? ' is' : 's are'} hidden
                  by current filters.
                </p>
              {:else}
                <div class="max-h-[60vh] space-y-1 overflow-auto font-mono text-xs">
                  {#each visibleReplayEvents as e, idx (idx)}
                    <div class="bg-muted/30 rounded border-l-2 border-sky-500/40 px-2 py-1">
                      {#if typeof e.seq === 'number'}
                        <span class="text-muted-foreground mr-2 tabular-nums">#{e.seq}</span>
                      {/if}
                      <span class="text-sky-700 dark:text-sky-400">{e.event}</span>
                      {#if e.actor_path && Array.isArray(e.actor_path) && e.actor_path.length > 0}
                        <span class="text-muted-foreground ml-2">{e.actor_path.join('/')}</span>
                      {/if}
                      <pre
                        class="text-muted-foreground mt-0.5 overflow-x-auto whitespace-pre-wrap text-[11px]">{JSON.stringify(
                          e,
                          null,
                          2
                        )}</pre>
                    </div>
                  {/each}
                </div>
              {/if}
            </CardContent>
          </Card>
        {/if}
      </TabsContent>
    {/if}

    <TabsContent value="manifest" class="mt-4">
      <Card>
        <CardContent class="pt-6">
          {#if manifestToml}
            <pre
              class="bg-muted/40 max-h-[60vh] overflow-auto rounded-md p-4 text-xs leading-relaxed"><code
                >{manifestToml}</code
              ></pre>
          {:else}
            <p class="text-muted-foreground py-6 text-center text-sm">
              No <code>manifest.snapshot.toml</code> found for this run.
            </p>
          {/if}
        </CardContent>
      </Card>
    </TabsContent>

    <TabsContent value="resolved" class="mt-4">
      <Card>
        <CardContent class="pt-6">
          {#if resolved}
            <pre
              class="bg-muted/40 max-h-[60vh] overflow-auto rounded-md p-4 text-xs leading-relaxed"><code
                >{JSON.stringify(resolved, null, 2)}</code
              ></pre>
          {:else}
            <p class="text-muted-foreground py-6 text-center text-sm">
              No <code>resolved.json</code> found for this run.
            </p>
          {/if}
        </CardContent>
      </Card>
    </TabsContent>

    <TabsContent value="summary" class="mt-4">
      <Card>
        <CardContent class="pt-6">
          <pre
            class="bg-muted/40 max-h-[60vh] overflow-auto rounded-md p-4 text-xs leading-relaxed"><code
              >{JSON.stringify(detail, null, 2)}</code
            ></pre>
        </CardContent>
      </Card>
    </TabsContent>
  </Tabs>

  <ApprovalModal {runId} bind:request={activeApproval} />

  <!-- Graph node inspector (#260). Bound to `selectedTaskId`; opens
       whenever the user clicks a graph node. Lazy-fetches the per-actor
       events.jsonl on first open. -->
  <RunGraphInspector
    {runId}
    open={selectedTaskId !== null}
    {selectedTaskId}
    task={selectedTask}
    worker={selectedWorker}
    failure={selectedTaskId ? failures[selectedTaskId] : undefined}
    sublead={selectedTaskId ? subleads[selectedTaskId] : undefined}
    activity={selectedTaskId ? storeActivity[selectedTaskId] : undefined}
    allActors={allWorkers}
    {inProgress}
    onJumpTo={(id) => (selectedTaskId = id === '' ? null : id)}
  />
{/if}
