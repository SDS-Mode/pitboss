<script lang="ts">
  import {
    Sheet,
    SheetContent,
    SheetDescription,
    SheetHeader,
    SheetTitle
  } from '$lib/components/ui/sheet';
  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import { Separator } from '$lib/components/ui/separator';
  import {
    getTaskEvents,
    getTaskLog,
    ApiError,
    type ActorActivity,
    type FailureReason,
    type SubleadInfo,
    type TaskEvent,
    type TaskRecord,
    type WorkerEntry
  } from '$lib/api';
  import { costUsd, fmtCost } from '$lib/prices';
  import LogPretty from '$lib/components/log-pretty.svelte';
  import { ExternalLink, ArrowLeftRight, AlertTriangle, Pause, Play } from 'lucide-svelte';

  let {
    runId,
    open = $bindable(false),
    selectedTaskId,
    task,
    worker,
    failure,
    sublead,
    activity,
    allTasks,
    inProgress,
    onJumpTo
  }: {
    runId: string;
    open?: boolean;
    selectedTaskId: string | null;
    /** Matching TaskRecord from `summary.jsonl` (post-finalize) or
     *  `summary.json` (post-finalize). Undefined when the actor only
     *  exists in live SSE state. */
    task: TaskRecord | undefined;
    /** Live `WorkersSnapshot` row if the actor is still active. */
    worker: WorkerEntry | undefined;
    failure: FailureReason | undefined;
    sublead: SubleadInfo | undefined;
    activity: ActorActivity | undefined;
    /** Full task list — used to compute parent/children for the
     *  hierarchy section. Same source the graph already consumes. */
    allTasks: TaskRecord[];
    /** Whether the run is still in progress. Drives whether the log
     *  pane auto-tails. Post-run inspectors render a frozen tail. */
    inProgress: boolean;
    /** Click-jump on parent / child rows to swap the selection. */
    onJumpTo: (taskId: string) => void;
  } = $props();

  // Lazy-fetch lifecycle events when the inspector opens for a new id.
  // The fetch is bounded to the currently-selected task; if the user
  // switches selection mid-fetch, the previous request's result is
  // dropped via the `lastFetchedFor` token check.
  let events = $state<TaskEvent[]>([]);
  let eventsLoading = $state(false);
  let eventsError = $state<string | null>(null);
  let lastFetchedFor = $state<string | null>(null);

  $effect(() => {
    const id = selectedTaskId;
    if (!open || !id) {
      events = [];
      eventsError = null;
      lastFetchedFor = null;
      return;
    }
    if (id === lastFetchedFor) return;
    lastFetchedFor = id;
    eventsLoading = true;
    eventsError = null;
    getTaskEvents(runId, id)
      .then((rows) => {
        // Drop the result if the selection has moved on.
        if (lastFetchedFor !== id) return;
        events = rows;
      })
      .catch((err) => {
        if (lastFetchedFor !== id) return;
        eventsError = err instanceof Error ? err.message : String(err);
      })
      .finally(() => {
        if (lastFetchedFor === id) eventsLoading = false;
      });
  });

  // ---- Log tail (inline, like the TUI's per-tile log view) ---------
  // The fetch hits the existing `/log` endpoint with `tail=true`, so
  // "tailing" is implemented as polling the last 64 KiB every 2 s
  // while the actor is still running. Keeps the implementation in
  // line with the existing standalone task log page (no new SSE
  // endpoint needed).
  let logText = $state<string>('');
  let logError = $state<string | null>(null);
  let logLoading = $state(false);
  let logLastFetchedFor = $state<string | null>(null);
  /** Operator override: pause auto-tail. Resets when the inspector
   *  switches to a different actor. Useful for inspecting a stable
   *  window without the view jumping on every poll. */
  let tailPaused = $state(false);
  /** Scroll container bound for auto-scroll-to-bottom on content change.
   *  Wraps either the raw `<pre>` or the `LogPretty` component so the
   *  scroll logic is identical across modes. */
  let logPaneEl = $state<HTMLDivElement | null>(null);
  /** Renderer mode. Pretty parses each NDJSON line via stream-json.ts
   *  and renders kind-styled rows; Raw just dumps the bytes. */
  let logFormat = $state<'pretty' | 'raw'>('pretty');
  /** Hide hook noise + thinking blocks in pretty mode. Off by default
   *  (hide hooks; show thinking). */
  let hideHooks = $state(true);
  let hideThinking = $state(false);
  /** Whether the most recent scroll position is at the bottom. If the
   *  user scrolled up to read history, polling continues but the view
   *  stays put. */
  let stickToBottom = $state(true);

  const LOG_TAIL_BYTES = 64 * 1024;
  const LOG_POLL_MS = 2000;

  /** True when this actor still has a live writer: run is in progress
   *  AND the actor's last-known status is non-terminal. */
  const isLogTailing = $derived.by(() => {
    if (!inProgress || tailPaused) return false;
    const s = task?.status;
    if (s && s !== 'Running' && s !== 'Paused' && s !== 'Frozen') return false;
    if (!worker && !task) return false;
    return true;
  });

  async function fetchLogOnce(id: string) {
    logLoading = true;
    try {
      const text = await getTaskLog(runId, id, { limit: LOG_TAIL_BYTES, tail: true });
      // Drop late results if the user navigated to another node.
      if (logLastFetchedFor !== id) return;
      logText = text;
      logError = null;
    } catch (err) {
      if (logLastFetchedFor !== id) return;
      // 404 is normal: stdout.log doesn't exist until the subprocess
      // writes its first byte. Render an empty pane rather than a banner.
      if (err instanceof ApiError && err.status === 404) {
        logText = '';
        logError = null;
      } else {
        logError = err instanceof Error ? err.message : String(err);
      }
    } finally {
      if (logLastFetchedFor === id) logLoading = false;
    }
  }

  $effect(() => {
    const id = selectedTaskId;
    if (!open || !id) {
      logText = '';
      logError = null;
      logLastFetchedFor = null;
      tailPaused = false;
      stickToBottom = true;
      return;
    }
    if (id !== logLastFetchedFor) {
      logLastFetchedFor = id;
      tailPaused = false;
      stickToBottom = true;
      // Initial fetch fires regardless of inProgress so terminal actors
      // still show their final log tail.
      fetchLogOnce(id);
    }
    if (!isLogTailing) return;
    const h = setInterval(() => {
      if (logLastFetchedFor === id) fetchLogOnce(id);
    }, LOG_POLL_MS);
    return () => clearInterval(h);
  });

  // Auto-scroll to bottom whenever new content lands AND the operator
  // hasn't scrolled up. Triggered by `logText` change.
  $effect(() => {
    // Touch logText so the effect re-runs on update.
    void logText;
    if (!stickToBottom || !logPaneEl) return;
    queueMicrotask(() => {
      if (logPaneEl) logPaneEl.scrollTop = logPaneEl.scrollHeight;
    });
  });

  function onLogScroll() {
    if (!logPaneEl) return;
    const dist = logPaneEl.scrollHeight - logPaneEl.scrollTop - logPaneEl.clientHeight;
    // 8 px tolerance for browsers that don't quite hit 0 on a flush.
    stickToBottom = dist <= 8;
  }

  const status = $derived(task?.status ?? worker?.state ?? 'unknown');
  const startedIso = $derived(task?.started_at ?? worker?.started_at);
  const endedIso = $derived(task?.ended_at);
  const duration = $derived(task?.duration_ms);

  const parentId = $derived(task?.parent_task_id ?? worker?.parent_task_id ?? null);
  const childrenIds = $derived(
    allTasks
      .filter((t) => t.parent_task_id === selectedTaskId)
      .map((t) => t.task_id)
  );

  const totalTokens = $derived(
    (task?.token_usage?.input ?? 0) +
      (task?.token_usage?.output ?? 0) +
      (task?.token_usage?.cache_read ?? 0) +
      (task?.token_usage?.cache_creation ?? 0)
  );

  const cost = $derived(
    typeof task?.cost_usd === 'number' ? task.cost_usd : costUsd(task?.model, task?.token_usage)
  );

  const failureKind = $derived.by(() => {
    const r = (task?.failure_reason ?? failure) as { kind?: string } | null | undefined;
    return r && typeof r === 'object' ? (r.kind ?? null) : null;
  });
  const failureMessage = $derived.by(() => {
    const r = (task?.failure_reason ?? failure) as { message?: string } | null | undefined;
    return r && typeof r === 'object' ? (r.message ?? null) : null;
  });

  function fmtDuration(ms?: number): string {
    if (typeof ms !== 'number' || !Number.isFinite(ms) || ms < 0) return '—';
    const s = Math.floor(ms / 1000);
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
    return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  }

  function fmtTime(iso?: string | null): string {
    if (!iso) return '—';
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString();
  }

  function statusBadgeVariant(s: string): 'destructive' | 'secondary' | 'outline' {
    const lc = s.toLowerCase();
    if (lc === 'failed' || lc === 'aborted' || lc === 'cancelled') return 'destructive';
    if (lc === 'success' || lc === 'completed') return 'secondary';
    return 'outline';
  }

  function eventLabel(e: TaskEvent): string {
    switch (e.kind) {
      case 'pause':
        return 'paused' + (e.reason ? ` (${e.reason})` : '');
      case 'continue':
        return `continued → ${e.new_session_id}`;
      case 'reprompt':
        return 'reprompted';
      case 'approval_request':
        return `approval requested (${e.request_id.slice(0, 8)}…)`;
      case 'approval_response':
        return `approval ${e.approved ? 'granted' : 'denied'}${e.edited ? ' (edited)' : ''}`;
      case 'notification_failed':
        return `notify_failed: ${e.sink_id}`;
      case 'tool_denied':
        return `denied: ${e.tool_name} (${e.reason_kind})`;
      case 'tool_auto_approved':
        return `auto-approved: ${e.tool_name}`;
    }
  }
</script>

<Sheet
  bind:open
  onOpenChange={(o: boolean) => {
    // Bubble close-via-overlay/ESC up to the parent so it can clear
    // `selectedTaskId`. Without this, hitting ESC closes the sheet but
    // the graph keeps highlighting the node.
    if (!o && selectedTaskId !== null) onJumpTo('');
  }}
>
  <SheetContent class="w-full overflow-y-auto sm:max-w-md">
    {#if !selectedTaskId}
      <SheetHeader>
        <SheetTitle>No selection</SheetTitle>
      </SheetHeader>
    {:else}
      <SheetHeader class="space-y-1">
        <SheetTitle class="flex items-center gap-2">
          <code class="text-base font-mono">{selectedTaskId}</code>
          <Badge variant={statusBadgeVariant(status)} class="text-[10px]">
            {status}
          </Badge>
          {#if sublead}
            <Badge variant="outline" class="text-[10px]">sublead</Badge>
          {/if}
        </SheetTitle>
        <SheetDescription class="text-xs">
          {task?.model ?? worker?.session_id ?? 'unknown model'}
          {#if task?.actor_type}
            · profile <code class="font-mono">{task.actor_type}</code>
          {/if}
        </SheetDescription>
      </SheetHeader>

      <div class="space-y-4 px-4 pb-6">
        <!-- Timing -->
        <section class="space-y-1 text-xs">
          <h3 class="text-muted-foreground text-[11px] font-medium uppercase tracking-wide">Timing</h3>
          <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
            <dt class="text-muted-foreground">Started</dt>
            <dd class="font-mono">{fmtTime(startedIso)}</dd>
            <dt class="text-muted-foreground">Ended</dt>
            <dd class="font-mono">{endedIso ? fmtTime(endedIso) : '—'}</dd>
            <dt class="text-muted-foreground">Duration</dt>
            <dd class="font-mono">{fmtDuration(duration)}</dd>
            {#if task?.exit_code !== undefined && task.exit_code !== null}
              <dt class="text-muted-foreground">Exit code</dt>
              <dd class="font-mono">{task.exit_code}</dd>
            {/if}
          </dl>
        </section>

        <Separator />

        <!-- Tokens / cost -->
        <section class="space-y-1 text-xs">
          <h3 class="text-muted-foreground text-[11px] font-medium uppercase tracking-wide">Tokens &amp; cost</h3>
          {#if totalTokens === 0 && cost === null}
            <p class="text-muted-foreground italic">No usage data yet.</p>
          {:else}
            <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 tabular-nums">
              <dt class="text-muted-foreground">Input</dt>
              <dd class="font-mono">{(task?.token_usage?.input ?? 0).toLocaleString()}</dd>
              <dt class="text-muted-foreground">Output</dt>
              <dd class="font-mono">{(task?.token_usage?.output ?? 0).toLocaleString()}</dd>
              <dt class="text-muted-foreground">Cache read</dt>
              <dd class="font-mono">{(task?.token_usage?.cache_read ?? 0).toLocaleString()}</dd>
              <dt class="text-muted-foreground">Cache create</dt>
              <dd class="font-mono">{(task?.token_usage?.cache_creation ?? 0).toLocaleString()}</dd>
              <dt class="text-muted-foreground">Cost</dt>
              <dd class="font-mono">{fmtCost(cost)}</dd>
            </dl>
          {/if}
        </section>

        <Separator />

        <!-- Counters -->
        <section class="space-y-1 text-xs">
          <h3 class="text-muted-foreground text-[11px] font-medium uppercase tracking-wide">Counters</h3>
          <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 tabular-nums">
            <dt class="text-muted-foreground">Pauses</dt>
            <dd class="font-mono">{task?.pause_count ?? 0}</dd>
            <dt class="text-muted-foreground">Reprompts</dt>
            <dd class="font-mono">{task?.reprompt_count ?? 0}</dd>
            <dt class="text-muted-foreground">Approvals (req / ok / denied)</dt>
            <dd class="font-mono">
              {task?.approvals_requested ?? 0} / {task?.approvals_approved ?? 0} / {task?.approvals_rejected ?? 0}
            </dd>
            {#if activity}
              <dt class="text-muted-foreground">Store ops (kv / lease / msg / art)</dt>
              <dd class="font-mono">
                {activity.kv_ops} / {activity.lease_ops} / {activity.message_ops ?? 0} / {activity.artifact_ops ?? 0}
              </dd>
            {/if}
          </dl>
        </section>

        <!-- Failure detail -->
        {#if failureKind}
          <Separator />
          <section class="space-y-1 text-xs">
            <h3 class="text-destructive flex items-center gap-1 text-[11px] font-medium uppercase tracking-wide">
              <AlertTriangle class="size-3" /> Failure
            </h3>
            <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
              <dt class="text-muted-foreground">Kind</dt>
              <dd class="font-mono">{failureKind}</dd>
              {#if failureMessage}
                <dt class="text-muted-foreground">Message</dt>
                <dd class="font-mono break-words">{failureMessage}</dd>
              {/if}
            </dl>
          </section>
        {/if}

        <Separator />

        <!-- Hierarchy -->
        <section class="space-y-1 text-xs">
          <h3 class="text-muted-foreground text-[11px] font-medium uppercase tracking-wide">Hierarchy</h3>
          <div class="flex flex-col gap-1.5">
            <div class="flex items-center gap-2">
              <span class="text-muted-foreground w-12">Parent</span>
              {#if parentId}
                <Button
                  variant="link"
                  size="sm"
                  class="h-auto p-0 font-mono"
                  onclick={() => onJumpTo(parentId)}
                >
                  <ArrowLeftRight class="mr-1 size-3" />
                  {parentId}
                </Button>
              {:else}
                <span class="text-muted-foreground italic">root</span>
              {/if}
            </div>
            <div class="flex items-start gap-2">
              <span class="text-muted-foreground w-12 pt-0.5">Children</span>
              {#if childrenIds.length === 0}
                <span class="text-muted-foreground italic">none</span>
              {:else}
                <div class="flex flex-wrap gap-1">
                  {#each childrenIds as id (id)}
                    <Button
                      variant="link"
                      size="sm"
                      class="h-auto p-0 font-mono"
                      onclick={() => onJumpTo(id)}
                    >
                      {id}
                    </Button>
                  {/each}
                </div>
              {/if}
            </div>
          </div>
        </section>

        <!-- Final message -->
        {#if task?.final_message ?? task?.final_message_preview}
          <Separator />
          <section class="space-y-1 text-xs">
            <h3 class="text-muted-foreground text-[11px] font-medium uppercase tracking-wide">Final message</h3>
            <pre class="bg-muted/30 max-h-48 overflow-auto whitespace-pre-wrap rounded p-2 font-mono text-[11px]">{task.final_message ?? task.final_message_preview}</pre>
          </section>
        {/if}

        <!-- Lifecycle events -->
        <Separator />
        <section class="space-y-1 text-xs">
          <h3 class="text-muted-foreground text-[11px] font-medium uppercase tracking-wide">Lifecycle events</h3>
          {#if eventsLoading}
            <p class="text-muted-foreground italic">Loading…</p>
          {:else if eventsError}
            <p class="text-destructive">Failed to load events: {eventsError}</p>
          {:else if events.length === 0}
            <p class="text-muted-foreground italic">No lifecycle events recorded.</p>
          {:else}
            <ul class="space-y-1">
              {#each events as e, i (i)}
                <li class="flex items-start gap-2 text-[11px]">
                  <span class="text-muted-foreground/80 w-32 shrink-0 font-mono">
                    {fmtTime(e.at)}
                  </span>
                  <span class="font-mono">{eventLabel(e)}</span>
                </li>
              {/each}
            </ul>
          {/if}
        </section>

        <!-- Live log tail -->
        <Separator />
        <section class="space-y-1 text-xs">
          <div class="flex items-center justify-between gap-2">
            <h3 class="text-muted-foreground text-[11px] font-medium uppercase tracking-wide">
              stdout.log
              <span class="ml-1 inline-flex items-center gap-1 normal-case">
                {#if isLogTailing}
                  <span
                    class="inline-block size-1.5 rounded-full bg-emerald-500 animate-pulse"
                    aria-hidden="true"
                  ></span>
                  <span class="text-emerald-700 dark:text-emerald-400">live</span>
                {:else if inProgress && tailPaused}
                  <span class="text-amber-700 dark:text-amber-400">paused</span>
                {:else}
                  <span class="text-muted-foreground/70">final</span>
                {/if}
              </span>
            </h3>
            <div class="flex items-center gap-1">
              <!-- Pretty / Raw toggle (#260 follow-up). Default Pretty:
                   the raw stream-json is dense and noisy for log
                   review; the pretty renderer parses each line and
                   collapses tool_use / tool_result / assistant text /
                   thinking into one styled row each. Raw stays a click
                   away for debugging the wire format. -->
              <div class="bg-muted/30 mr-1 inline-flex items-center rounded text-[10px]">
                <button
                  class="px-1.5 py-0.5 rounded {logFormat === 'pretty'
                    ? 'bg-background border border-border shadow-sm font-medium'
                    : 'text-muted-foreground hover:text-foreground'}"
                  onclick={() => (logFormat = 'pretty')}
                  title="Human-readable rows (parsed stream-json)"
                >
                  Pretty
                </button>
                <button
                  class="px-1.5 py-0.5 rounded {logFormat === 'raw'
                    ? 'bg-background border border-border shadow-sm font-medium'
                    : 'text-muted-foreground hover:text-foreground'}"
                  onclick={() => (logFormat = 'raw')}
                  title="Raw NDJSON as written by the claude subprocess"
                >
                  Raw
                </button>
              </div>
              {#if inProgress && (worker || task?.status === 'Running' || task?.status === 'Paused' || task?.status === 'Frozen')}
                <Button
                  variant="ghost"
                  size="sm"
                  class="h-6 px-2"
                  onclick={() => (tailPaused = !tailPaused)}
                  title={tailPaused ? 'Resume tail' : 'Pause tail'}
                >
                  {#if tailPaused}
                    <Play class="size-3" />
                  {:else}
                    <Pause class="size-3" />
                  {/if}
                </Button>
              {/if}
              <Button
                variant="ghost"
                size="sm"
                class="h-6 px-2"
                onclick={() => {
                  // Open the standalone task page (head/tail toggle,
                  // download-full button, status card) so an operator
                  // who needs more than a 64 KiB tail can get it.
                  window.open(
                    `/runs/${encodeURIComponent(runId)}/tasks/${encodeURIComponent(selectedTaskId!)}`,
                    '_blank',
                    'noopener'
                  );
                }}
                title="Open in standalone log view"
              >
                <ExternalLink class="size-3" />
              </Button>
            </div>
          </div>
          {#if logFormat === 'pretty'}
            <div class="text-muted-foreground flex items-center gap-3 text-[10px]">
              <label class="inline-flex cursor-pointer items-center gap-1">
                <input type="checkbox" class="size-3" bind:checked={hideHooks} />
                hide hooks
              </label>
              <label class="inline-flex cursor-pointer items-center gap-1">
                <input type="checkbox" class="size-3" bind:checked={hideThinking} />
                hide thinking
              </label>
            </div>
          {/if}
          {#if logError}
            <p class="text-destructive text-[11px]">{logError}</p>
          {:else if logLoading && logText.length === 0}
            <p class="text-muted-foreground italic">Loading…</p>
          {:else if logText.length === 0}
            <p class="text-muted-foreground italic">
              {isLogTailing ? 'Waiting for first output…' : 'Log is empty.'}
            </p>
          {:else}
            <div
              bind:this={logPaneEl}
              onscroll={onLogScroll}
              class="bg-muted/40 max-h-72 overflow-auto rounded p-2"
            >
              {#if logFormat === 'pretty'}
                <LogPretty text={logText} {hideHooks} {hideThinking} />
              {:else}
                <pre
                  class="font-mono text-[10px] leading-relaxed whitespace-pre-wrap m-0">{logText}</pre>
              {/if}
            </div>
            {#if !stickToBottom}
              <button
                class="text-sky-700 dark:text-sky-400 mt-1 text-[10px] underline hover:no-underline"
                onclick={() => {
                  stickToBottom = true;
                  if (logPaneEl) logPaneEl.scrollTop = logPaneEl.scrollHeight;
                }}
              >
                ↓ Jump to bottom
              </button>
            {/if}
          {/if}
        </section>
      </div>
    {/if}
  </SheetContent>
</Sheet>
