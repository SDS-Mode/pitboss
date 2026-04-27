<script lang="ts">
  import type { WorkerEntry, ActorActivity, SubleadInfo, FailureReason } from '$lib/api';
  import { Badge } from '$lib/components/ui/badge';
  import { Button } from '$lib/components/ui/button';
  import { Pause, Play, MessageSquare, Ban, Layers, AlertTriangle } from 'lucide-svelte';

  let {
    workers,
    storeActivity,
    failures,
    subleads,
    disabled = false,
    onPause,
    onContinue,
    onReprompt,
    onCancel
  }: {
    workers: WorkerEntry[];
    /** actor_id → counters */
    storeActivity: Record<string, ActorActivity>;
    /** task_id → failure reason payload */
    failures: Record<string, FailureReason>;
    /** sublead_id → snapshot */
    subleads: Record<string, SubleadInfo>;
    disabled?: boolean;
    onPause: (task_id: string) => void;
    onContinue: (task_id: string) => void;
    onReprompt: (task_id: string) => void;
    onCancel: (task_id: string) => void;
  } = $props();

  // Group workers by parent. Top-level entries (no parent_task_id) sort
  // first; their direct children render in a nested grid underneath.
  const grouped = $derived(() => {
    const childrenOf = new Map<string, WorkerEntry[]>();
    const roots: WorkerEntry[] = [];
    for (const w of workers) {
      if (w.parent_task_id) {
        const list = childrenOf.get(w.parent_task_id) ?? [];
        list.push(w);
        childrenOf.set(w.parent_task_id, list);
      } else {
        roots.push(w);
      }
    }
    // Workers whose parent isn't itself in the snapshot land at top level
    // so they don't disappear (parent might have terminated and been
    // pruned from WorkersSnapshot).
    const knownIds = new Set(workers.map((w) => w.task_id));
    for (const w of workers) {
      if (w.parent_task_id && !knownIds.has(w.parent_task_id) && !roots.includes(w)) {
        roots.push(w);
      }
    }
    return { roots, childrenOf };
  });

  function tileColor(state: string, hasFailure: boolean): string {
    if (hasFailure) return 'border-red-500/60 bg-red-500/5';
    switch (state) {
      case 'running':
        return 'border-sky-500/60 bg-sky-500/5';
      case 'paused':
      case 'frozen':
        return 'border-amber-500/60 bg-amber-500/5';
      case 'completed':
      case 'success':
        return 'border-emerald-500/60 bg-emerald-500/5';
      case 'failed':
      case 'aborted':
        return 'border-red-500/60 bg-red-500/5';
      default:
        return 'border-border/60';
    }
  }

  function badgeVariant(state: string): 'outline' | 'destructive' | 'secondary' {
    if (state === 'failed' || state === 'aborted') return 'destructive';
    if (state === 'completed' || state === 'success') return 'secondary';
    return 'outline';
  }

  function relativeStarted(iso?: string): string {
    if (!iso) return '';
    const t = Date.parse(iso);
    if (!Number.isFinite(t)) return '';
    const ago = Math.max(0, Math.floor((Date.now() - t) / 1000));
    if (ago < 60) return `${ago}s ago`;
    if (ago < 3600) return `${Math.floor(ago / 60)}m ago`;
    return `${Math.floor(ago / 3600)}h ${Math.floor((ago % 3600) / 60)}m ago`;
  }
</script>

{#snippet tile(w: WorkerEntry, depth: number)}
  {@const failure = failures[w.task_id]}
  {@const activity = storeActivity[w.task_id]}
  {@const sublead = subleads[w.task_id]}
  <div
    class="rounded-md border-l-4 p-3 transition {tileColor(w.state, !!failure)}"
    style:margin-left="{depth * 12}px"
  >
    <div class="mb-1 flex items-center justify-between gap-2">
      <div class="flex min-w-0 items-center gap-2">
        <code class="truncate text-xs font-medium">{w.task_id}</code>
        {#if sublead}
          <Layers class="text-muted-foreground size-3 shrink-0" />
        {/if}
      </div>
      <Badge variant={badgeVariant(w.state)} class="shrink-0 text-[10px]">{w.state}</Badge>
    </div>

    {#if w.prompt_preview}
      <p class="text-muted-foreground mb-2 line-clamp-2 text-xs">{w.prompt_preview}</p>
    {/if}

    {#if failure}
      <div class="bg-destructive/10 text-destructive mb-2 flex items-start gap-1.5 rounded px-2 py-1 text-[11px]">
        <AlertTriangle class="mt-0.5 size-3 shrink-0" />
        <span class="font-mono">{failure.kind}</span>
      </div>
    {/if}

    {#if sublead}
      <div class="text-muted-foreground mb-2 grid grid-cols-3 gap-1 text-[11px]">
        {#if typeof sublead.budget_usd === 'number'}
          <span title="budget">${sublead.budget_usd.toFixed(2)}</span>
        {:else}
          <span title="shared pool">shared</span>
        {/if}
        {#if typeof sublead.max_workers === 'number'}
          <span title="max workers">≤{sublead.max_workers}w</span>
        {/if}
        {#if sublead.read_down}<span>read↓</span>{/if}
      </div>
    {/if}

    <div class="text-muted-foreground flex items-center justify-between gap-2 text-[11px]">
      <div class="flex items-center gap-2 tabular-nums">
        {#if activity}
          <span title="store ops">kv:{activity.kv_ops} lease:{activity.lease_ops}</span>
        {/if}
        {#if w.started_at}
          <span>{relativeStarted(w.started_at)}</span>
        {/if}
      </div>
      <div class="flex gap-0.5">
        <Button
          variant="ghost"
          size="sm"
          class="h-6 w-6 p-0"
          title="Pause"
          {disabled}
          onclick={() => onPause(w.task_id)}
        >
          <Pause class="size-3" />
        </Button>
        <Button
          variant="ghost"
          size="sm"
          class="h-6 w-6 p-0"
          title="Continue"
          {disabled}
          onclick={() => onContinue(w.task_id)}
        >
          <Play class="size-3" />
        </Button>
        <Button
          variant="ghost"
          size="sm"
          class="h-6 w-6 p-0"
          title="Reprompt"
          {disabled}
          onclick={() => onReprompt(w.task_id)}
        >
          <MessageSquare class="size-3" />
        </Button>
        <Button
          variant="ghost"
          size="sm"
          class="text-destructive hover:text-destructive h-6 w-6 p-0"
          title="Cancel"
          {disabled}
          onclick={() => onCancel(w.task_id)}
        >
          <Ban class="size-3" />
        </Button>
      </div>
    </div>
  </div>
{/snippet}

{#if workers.length === 0}
  <p class="text-muted-foreground py-6 text-center text-xs">No workers reported yet.</p>
{:else}
  {@const g = grouped()}
  <div class="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
    {#each g.roots as root (root.task_id)}
      <div class="space-y-2">
        {@render tile(root, 0)}
        {#each g.childrenOf.get(root.task_id) ?? [] as child (child.task_id)}
          {@render tile(child, 1)}
        {/each}
      </div>
    {/each}
  </div>
{/if}
