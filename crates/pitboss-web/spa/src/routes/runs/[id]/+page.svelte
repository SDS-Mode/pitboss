<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import {
    getRun,
    getResolvedManifest,
    getManifestToml,
    getSummaryJsonl,
    subscribeRunEvents,
    postControlOp,
    type ControlEnvelope,
    type RunDetailDto,
    type PolicyRule,
    ApiError
  } from '$lib/api';
  import { formatUnixSeconds, relativeFromUnix } from '$lib/utils';
  import StatusBadge from '$lib/components/status-badge.svelte';
  import ApprovalModal, { type ApprovalRequest } from '$lib/components/approval-modal.svelte';
  import PolicyEditor from '$lib/components/policy-editor.svelte';
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
  import {
    ArrowLeft,
    ChevronRight,
    RefreshCw,
    AlertTriangle,
    Octagon,
    Pause,
    Play,
    MessageSquare,
    Ban
  } from 'lucide-svelte';
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
  const status = $derived<RunStatus>(
    (stub?.status as RunStatus | undefined) ??
      (summary?.run_meta?.outcome === 'success' ? 'complete' : summary ? 'complete' : 'aborted')
  );
  const taskList = $derived<Array<Record<string, any>>>(
    (summary?.tasks as Array<Record<string, any>> | undefined) ?? []
  );

  const totalCost = $derived(
    taskList.reduce((sum, t) => sum + (typeof t.cost_usd === 'number' ? t.cost_usd : 0), 0)
  );
  const totalTokens = $derived(
    taskList.reduce((sum, t) => {
      const usage = t.token_usage as Record<string, number> | undefined;
      if (!usage) return sum;
      return sum + (usage.input_tokens ?? 0) + (usage.output_tokens ?? 0);
    }, 0)
  );

  // Parsed JSONL of in-progress task records — lets the Tasks tab show
  // partial state while the run hasn't finalized.
  const liveTasks = $derived<Array<Record<string, any>>>(
    summaryJsonl
      ? summaryJsonl
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
      : []
  );

  const tasksToRender = $derived(taskList.length > 0 ? taskList : liveTasks);

  // ---- Phase 2: live control events (SSE) -----------------------------
  // The dispatcher's per-run control socket is bridged to /api/runs/:id/events
  // by `pitboss-web` and fanned out to N browser tabs via tokio broadcast.
  // We only attach an EventSource for in-progress runs — completed runs
  // have nothing live to stream.
  let liveEvents = $state<ControlEnvelope[]>([]);
  let sseStatus = $state<'idle' | 'connecting' | 'open' | 'closed' | 'error'>('idle');
  const MAX_LIVE_EVENTS = 200;

  // ---- Phase 3: control state derived from the live event stream ------
  // The dispatcher pushes typed events; we keep the latest snapshot of
  // each kind we render in the UI. None of this needs to round-trip to
  // disk — refreshing the page rebuilds it from the next Hello +
  // WorkersSnapshot pair.
  type WorkerEntry = {
    task_id: string;
    state: string;
    prompt_preview: string;
    parent_task_id?: string;
  };
  let workers = $state<WorkerEntry[]>([]);
  let policyRules = $state<PolicyRule[]>([]);
  let serverVersion = $state<string | null>(null);
  let pendingApprovals = $state<ApprovalRequest[]>([]);
  let activeApproval = $state<ApprovalRequest | null>(null);
  // Banner shown when ANOTHER client takes over our slot (we get
  // `Superseded` from the dispatcher right before the socket closes).
  let superseded = $state(false);
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
    policyRules = [];
    pendingApprovals = [];
    activeApproval = null;
    superseded = false;
    const teardown = subscribeRunEvents(runId, {
      onOpen: () => (sseStatus = 'open'),
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
    sendOp(
      postControlOp(runId, { op: 'continue_worker', task_id }),
      `continue_worker ${task_id}`
    );
  }
  function repromptWorker(task_id: string) {
    const prompt = window.prompt('New prompt for the worker?');
    if (!prompt || !prompt.trim()) return;
    sendOp(
      postControlOp(runId, { op: 'reprompt_worker', task_id, prompt: prompt.trim() }),
      `reprompt_worker ${task_id}`
    );
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

  function fmtCost(v?: number): string {
    if (typeof v !== 'number') return '—';
    return v < 0.01 ? `$${v.toFixed(4)}` : `$${v.toFixed(2)}`;
  }

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
      </p>
    </div>
    <Button variant="outline" size="sm" onclick={load} disabled={loading}>
      <RefreshCw class="mr-2 size-4 {loading ? 'animate-spin' : ''}" />
      Refresh
    </Button>
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
        <CardDescription>Total cost</CardDescription>
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
      <TabsTrigger value="tasks">Tasks ({tasksToRender.length})</TabsTrigger>
      <TabsTrigger value="manifest">Manifest</TabsTrigger>
      <TabsTrigger value="resolved">Resolved</TabsTrigger>
      <TabsTrigger value="summary">Summary JSON</TabsTrigger>
    </TabsList>

    {#if inProgress}
      <TabsContent value="live" class="mt-4 space-y-4">
        {#if superseded}
          <Card class="border-amber-500/50 bg-amber-500/5">
            <CardContent class="flex items-start gap-3 pt-6">
              <AlertTriangle class="mt-0.5 size-5 shrink-0 text-amber-600" />
              <div>
                <p class="font-medium text-amber-700 dark:text-amber-300">Control taken</p>
                <p class="text-muted-foreground mt-1 text-sm">
                  Another client (TUI or another browser) connected to this run's control
                  socket and superseded ours. Read-only views still work.
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

        <Card>
          <CardHeader class="pb-3">
            <CardTitle class="text-base">
              Workers
              <Badge variant="outline" class="ml-2 text-xs">{workers.length}</Badge>
            </CardTitle>
            <CardDescription class="text-xs">
              Live snapshot from `WorkersSnapshot` events.
            </CardDescription>
          </CardHeader>
          <CardContent class="pt-0">
            {#if workers.length === 0}
              <p class="text-muted-foreground py-4 text-center text-xs">
                {sseStatus === 'open'
                  ? 'No workers reported yet.'
                  : 'Waiting for first snapshot…'}
              </p>
            {:else}
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Task</TableHead>
                    <TableHead>State</TableHead>
                    <TableHead>Prompt</TableHead>
                    <TableHead class="w-[18ch] text-right">Actions</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {#each workers as w (w.task_id)}
                    <TableRow>
                      <TableCell>
                        <code class="text-xs">{w.task_id}</code>
                        {#if w.parent_task_id}
                          <span class="text-muted-foreground ml-1 text-xs">← {w.parent_task_id}</span>
                        {/if}
                      </TableCell>
                      <TableCell>
                        <Badge
                          variant={w.state === 'running'
                            ? 'outline'
                            : w.state === 'failed'
                              ? 'destructive'
                              : 'secondary'}
                        >
                          {w.state}
                        </Badge>
                      </TableCell>
                      <TableCell class="text-muted-foreground max-w-[40ch] truncate text-xs">
                        {w.prompt_preview}
                      </TableCell>
                      <TableCell class="text-right">
                        <div class="flex justify-end gap-1">
                          <Button
                            variant="ghost"
                            size="sm"
                            class="h-7 px-2"
                            title="Pause (freeze)"
                            disabled={superseded}
                            onclick={() => pauseWorker(w.task_id)}
                          >
                            <Pause class="size-3.5" />
                          </Button>
                          <Button
                            variant="ghost"
                            size="sm"
                            class="h-7 px-2"
                            title="Continue"
                            disabled={superseded}
                            onclick={() => continueWorker(w.task_id)}
                          >
                            <Play class="size-3.5" />
                          </Button>
                          <Button
                            variant="ghost"
                            size="sm"
                            class="h-7 px-2"
                            title="Reprompt"
                            disabled={superseded}
                            onclick={() => repromptWorker(w.task_id)}
                          >
                            <MessageSquare class="size-3.5" />
                          </Button>
                          <Button
                            variant="ghost"
                            size="sm"
                            class="text-destructive hover:text-destructive h-7 px-2"
                            title="Cancel worker"
                            disabled={superseded}
                            onclick={() => cancelWorker(w.task_id)}
                          >
                            <Ban class="size-3.5" />
                          </Button>
                        </div>
                      </TableCell>
                    </TableRow>
                  {/each}
                </TableBody>
              </Table>
            {/if}
          </CardContent>
        </Card>

        <PolicyEditor {runId} initialRules={policyRules} />

        <Card>
          <CardHeader class="pb-2">
            <CardTitle class="text-base">Event stream</CardTitle>
            <CardDescription class="text-xs">
              SSE bridge: <span class="font-mono">{sseStatus}</span>
              · {liveEvents.length} event{liveEvents.length === 1 ? '' : 's'}{#if liveEvents.length === MAX_LIVE_EVENTS}
                (latest only){/if}
            </CardDescription>
          </CardHeader>
          <CardContent class="pt-0">
            {#if liveEvents.length === 0}
              <p class="text-muted-foreground py-4 text-center text-sm">
                {sseStatus === 'open'
                  ? 'Waiting for first event…'
                  : sseStatus === 'error'
                    ? 'Connection failed. Run may have ended or dispatcher is unreachable.'
                    : 'Connecting…'}
              </p>
            {:else}
              <div class="max-h-[40vh] space-y-1 overflow-auto font-mono text-xs">
                {#each liveEvents as e, idx (idx)}
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
                <TableHead class="w-[10ch] text-right">Cost</TableHead>
                <TableHead class="w-[10ch] text-right">Duration</TableHead>
                <TableHead class="w-[8ch]">Log</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {#each tasksToRender as t (t.task_id ?? Math.random())}
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
                  <TableCell class="text-right tabular-nums">{fmtCost(t.cost_usd)}</TableCell>
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
{/if}
