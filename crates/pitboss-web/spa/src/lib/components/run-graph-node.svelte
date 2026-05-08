<script lang="ts">
  import { Handle, Position, type Node, type NodeProps } from '@xyflow/svelte';
  import type {
    WorkerEntry,
    ActorActivity,
    FailureReason,
    SubleadInfo,
    TaskRecord
  } from '$lib/api';
  import { Badge } from '$lib/components/ui/badge';
  import { Layers, AlertTriangle } from 'lucide-svelte';
  import { costUsd, fmtCost } from '$lib/prices';

  // SvelteFlow passes the node `data` payload via standard NodeProps.
  // We attach our own typed payload to avoid `any` everywhere.
  type RunNodeData = {
    worker: WorkerEntry;
    activity?: ActorActivity;
    failure?: FailureReason;
    sublead?: SubleadInfo;
    /** Full per-task record from `summary.jsonl` (or `summary.json`).
     * Undefined for live-only actors that haven't written a record yet. */
    task?: TaskRecord;
    /** True when this node matches the inspector's current selection. */
    selected?: boolean;
    [k: string]: unknown;
  };

  type RunNode = Node<RunNodeData, 'runNode'>;

  let { data }: NodeProps<RunNode> = $props();
  const d = $derived(data);

  function tileColor(state: string, hasFailure: boolean): string {
    if (hasFailure) return 'border-red-500/70 bg-red-500/10';
    switch (state) {
      case 'running':
        return 'border-sky-500/70 bg-sky-500/10';
      case 'paused':
      case 'frozen':
        return 'border-amber-500/70 bg-amber-500/10';
      case 'completed':
      case 'success':
        return 'border-emerald-500/70 bg-emerald-500/10';
      case 'failed':
      case 'aborted':
        return 'border-red-500/70 bg-red-500/10';
      default:
        return 'border-border bg-background';
    }
  }

  function fmtDuration(ms: number | undefined): string {
    if (typeof ms !== 'number' || !Number.isFinite(ms) || ms < 0) return '—';
    const s = Math.floor(ms / 1000);
    if (s < 60) return `${s}s`;
    if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`;
    return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  }

  function fmtTokens(n: number | undefined): string {
    if (typeof n !== 'number' || !Number.isFinite(n)) return '0';
    if (n < 1000) return String(n);
    if (n < 1_000_000) return `${(n / 1000).toFixed(1)}k`;
    return `${(n / 1_000_000).toFixed(2)}m`;
  }

  function fmtRel(iso: string): string {
    const t = Date.parse(iso);
    if (!Number.isFinite(t)) return '';
    const ago = Math.max(0, Math.floor((Date.now() - t) / 1000));
    if (ago < 60) return `${ago}s ago`;
    if (ago < 3600) return `${Math.floor(ago / 60)}m ago`;
    return `${Math.floor(ago / 3600)}h ago`;
  }

  /**
   * Failure-reason chip text. `failure_reason` from TaskRecord is the
   * post-run authoritative source; the live `failure` map (SSE) wins
   * during an active run. Renders the variant tag plus a short
   * substring for tagged-with-message variants.
   */
  function failureChip(
    reason: unknown
  ): { label: string; detail?: string } | null {
    if (!reason || typeof reason !== 'object') return null;
    const r = reason as { kind?: string; message?: string };
    if (!r.kind) return null;
    return { label: r.kind, detail: r.message };
  }

  // The node's headline counters. Pull from TaskRecord if present, else
  // zero. (Live-only entries don't have these — they're accumulated by
  // the dispatcher and only flushed at task finalize.)
  const counters = $derived({
    pause: d.task?.pause_count ?? 0,
    reprompt: d.task?.reprompt_count ?? 0,
    aprq: d.task?.approvals_requested ?? 0,
    aprn: d.task?.approvals_rejected ?? 0
  });

  // Cost: prefer the dispatcher-stamped value (#258); fall back to the
  // SPA's per-1M-token table when older binaries leave it null.
  const cost = $derived(
    typeof d.task?.cost_usd === 'number'
      ? d.task.cost_usd
      : costUsd(d.task?.model, d.task?.token_usage)
  );

  const failureInfo = $derived(failureChip(d.task?.failure_reason ?? d.failure));

  const totalTokens = $derived(
    (d.task?.token_usage?.input ?? 0) +
      (d.task?.token_usage?.output ?? 0) +
      (d.task?.token_usage?.cache_read ?? 0)
  );

  const startedIso = $derived(d.task?.started_at ?? d.worker.started_at);
  const isTerminal = $derived(
    d.task?.status &&
      d.task.status !== 'Running' &&
      d.task.status !== 'Paused' &&
      d.task.status !== 'Frozen'
  );
</script>

<div
  class="bg-background w-52 rounded-md border-2 px-3 py-2 shadow-sm transition-shadow {tileColor(
    d.worker.state,
    !!d.failure || !!failureInfo
  )} {d.selected
    ? 'ring-2 ring-sky-500/70 ring-offset-1 ring-offset-background'
    : ''}"
>
  <Handle type="target" position={Position.Top} class="!bg-muted-foreground/40" />

  <!-- Row 1: id + role + sublead glyph -->
  <div class="flex items-center justify-between gap-1">
    <code class="truncate text-[10px] font-medium" title={d.worker.task_id}>
      {d.worker.task_id}
    </code>
    {#if d.sublead}
      <Layers class="text-muted-foreground size-3 shrink-0" />
    {/if}
  </div>

  <!-- Row 2: status badge + failure indicator -->
  <div class="mt-0.5 flex items-center gap-1">
    <Badge
      variant={d.worker.state === 'failed' ? 'destructive' : 'outline'}
      class="text-[9px] py-0"
    >
      {d.worker.state}
    </Badge>
    {#if failureInfo}
      <AlertTriangle class="text-destructive size-3 shrink-0" />
    {/if}
  </div>

  <!-- Row 3: timing -->
  <div class="text-muted-foreground mt-1 flex items-center justify-between gap-1 text-[9px] tabular-nums">
    {#if isTerminal && d.task?.duration_ms !== undefined}
      <span title={`Started ${startedIso ?? ''}`}>ran {fmtDuration(d.task.duration_ms)}</span>
    {:else if startedIso}
      <span title={`Started ${startedIso}`}>started {fmtRel(startedIso)}</span>
    {:else}
      <span>—</span>
    {/if}
    {#if d.task?.model}
      <span class="truncate" title={d.task.model}>
        {d.task.model.replace(/^claude-/, '')}
      </span>
    {/if}
  </div>

  <!-- Row 4: tokens + cost -->
  {#if totalTokens > 0 || cost !== null}
    <div class="text-muted-foreground mt-0.5 flex items-center justify-between gap-1 text-[9px] tabular-nums">
      <span title={`in ${d.task?.token_usage?.input ?? 0} / out ${d.task?.token_usage?.output ?? 0} / cr ${d.task?.token_usage?.cache_read ?? 0}`}>
        {fmtTokens(totalTokens)} tok
      </span>
      <span class="font-medium">{fmtCost(cost)}</span>
    </div>
  {/if}

  <!-- Row 5: counters (only when non-zero, to keep tile dense for typical runs) -->
  {#if counters.pause + counters.reprompt + counters.aprq + counters.aprn > 0}
    <div class="text-muted-foreground mt-0.5 flex flex-wrap gap-x-1.5 text-[9px] tabular-nums">
      {#if counters.pause > 0}<span>p:{counters.pause}</span>{/if}
      {#if counters.reprompt > 0}<span>rp:{counters.reprompt}</span>{/if}
      {#if counters.aprq > 0}<span title="approvals requested">aq:{counters.aprq}</span>{/if}
      {#if counters.aprn > 0}<span title="approvals rejected" class="text-destructive">an:{counters.aprn}</span>{/if}
    </div>
  {/if}

  <!-- Row 6: live store activity (SSE-only; absent post-run) -->
  {#if d.activity}
    {@const msgOps = d.activity.message_ops ?? 0}
    {@const artOps = d.activity.artifact_ops ?? 0}
    <div class="text-muted-foreground/80 mt-0.5 text-[9px] tabular-nums">
      kv:{d.activity.kv_ops} lease:{d.activity.lease_ops}
      {#if msgOps > 0 || artOps > 0}msg:{msgOps} art:{artOps}{/if}
    </div>
  {/if}

  <!-- Row 7: failure-reason chip -->
  {#if failureInfo}
    <div
      class="bg-destructive/10 text-destructive mt-1 rounded border border-destructive/30 px-1.5 py-0.5 text-[9px]"
      title={failureInfo.detail ?? ''}
    >
      <span class="font-medium">{failureInfo.label}</span>
      {#if failureInfo.detail}
        <span class="text-destructive/80 ml-1 truncate">{failureInfo.detail}</span>
      {/if}
    </div>
  {/if}

  <Handle type="source" position={Position.Bottom} class="!bg-muted-foreground/40" />
</div>
