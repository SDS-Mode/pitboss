<script lang="ts">
  import { Handle, Position, type Node, type NodeProps } from '@xyflow/svelte';
  import type { WorkerEntry, ActorActivity, FailureReason, SubleadInfo } from '$lib/api';
  import { Badge } from '$lib/components/ui/badge';
  import { Layers, AlertTriangle } from 'lucide-svelte';

  // SvelteFlow passes the node `data` payload via standard NodeProps.
  // We attach our own typed payload to avoid `any` everywhere.
  type RunNodeData = {
    worker: WorkerEntry;
    activity?: ActorActivity;
    failure?: FailureReason;
    sublead?: SubleadInfo;
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
</script>

<div
  class="bg-background w-44 rounded-md border-2 px-3 py-2 shadow-sm {tileColor(
    d.worker.state,
    !!d.failure
  )}"
>
  <Handle type="target" position={Position.Top} class="!bg-muted-foreground/40" />
  <div class="flex items-center justify-between gap-1">
    <code class="truncate text-[10px] font-medium">{d.worker.task_id}</code>
    {#if d.sublead}
      <Layers class="text-muted-foreground size-3 shrink-0" />
    {/if}
  </div>
  <div class="mt-0.5 flex items-center gap-1">
    <Badge
      variant={d.worker.state === 'failed' ? 'destructive' : 'outline'}
      class="text-[9px] py-0"
    >
      {d.worker.state}
    </Badge>
    {#if d.failure}
      <AlertTriangle class="text-destructive size-3" />
    {/if}
  </div>
  {#if d.activity}
    <div class="text-muted-foreground mt-1 text-[9px] tabular-nums">
      kv:{d.activity.kv_ops} lease:{d.activity.lease_ops}
    </div>
  {/if}
  <Handle type="source" position={Position.Bottom} class="!bg-muted-foreground/40" />
</div>
