<script lang="ts">
  import { SvelteFlow, Background, Controls, MiniMap, type Node, type Edge } from '@xyflow/svelte';
  import dagre from '@dagrejs/dagre';
  import '@xyflow/svelte/dist/style.css';
  import RunGraphNode from './run-graph-node.svelte';
  import type { WorkerEntry, ActorActivity, FailureReason, SubleadInfo } from '$lib/api';

  let {
    workers,
    storeActivity,
    failures,
    subleads
  }: {
    workers: WorkerEntry[];
    storeActivity: Record<string, ActorActivity>;
    failures: Record<string, FailureReason>;
    subleads: Record<string, SubleadInfo>;
  } = $props();

  const NODE_WIDTH = 180;
  const NODE_HEIGHT = 90;

  /** Build {nodes, edges} for the current snapshot, then run dagre for layout. */
  function layout(
    workers: WorkerEntry[],
    storeActivity: Record<string, ActorActivity>,
    failures: Record<string, FailureReason>,
    subleads: Record<string, SubleadInfo>
  ): { nodes: Node[]; edges: Edge[] } {
    const g = new dagre.graphlib.Graph();
    g.setGraph({ rankdir: 'TB', nodesep: 32, ranksep: 64 });
    g.setDefaultEdgeLabel(() => ({}));

    const knownIds = new Set(workers.map((w) => w.task_id));

    for (const w of workers) {
      g.setNode(w.task_id, { width: NODE_WIDTH, height: NODE_HEIGHT });
    }
    for (const w of workers) {
      if (w.parent_task_id && knownIds.has(w.parent_task_id)) {
        g.setEdge(w.parent_task_id, w.task_id);
      }
    }
    dagre.layout(g);

    const nodes: Node[] = workers.map((w) => {
      const pos = g.node(w.task_id);
      return {
        id: w.task_id,
        type: 'runNode',
        position: {
          x: (pos?.x ?? 0) - NODE_WIDTH / 2,
          y: (pos?.y ?? 0) - NODE_HEIGHT / 2
        },
        data: {
          worker: w,
          activity: storeActivity[w.task_id],
          failure: failures[w.task_id],
          sublead: subleads[w.task_id]
        }
      };
    });

    const edges: Edge[] = [];
    for (const w of workers) {
      if (w.parent_task_id && knownIds.has(w.parent_task_id)) {
        edges.push({
          id: `${w.parent_task_id}->${w.task_id}`,
          source: w.parent_task_id,
          target: w.task_id,
          animated: w.state === 'running',
          style: w.state === 'running' ? 'stroke: rgb(14 165 233 / 0.6)' : undefined
        });
      }
    }
    return { nodes, edges };
  }

  // Recompute on every prop change. SvelteFlow's `nodes` / `edges` props
  // expect $state stores under the hood; reassigning derived arrays
  // re-renders cleanly because the component does shallow identity diff.
  let layoutResult = $derived(layout(workers, storeActivity, failures, subleads));
  let nodes = $state<Node[]>([]);
  let edges = $state<Edge[]>([]);
  $effect(() => {
    nodes = layoutResult.nodes;
    edges = layoutResult.edges;
  });

  const nodeTypes = { runNode: RunGraphNode };
</script>

<div class="bg-muted/20 dark:bg-muted/5 h-[60vh] w-full rounded-md border">
  {#if workers.length === 0}
    <div class="text-muted-foreground flex h-full items-center justify-center text-xs">
      No workers reported yet.
    </div>
  {:else}
    <SvelteFlow
      bind:nodes
      bind:edges
      {nodeTypes}
      fitView
      nodesDraggable={false}
      nodesConnectable={false}
      proOptions={{ hideAttribution: true }}
    >
      <Background />
      <Controls />
      <MiniMap pannable zoomable />
    </SvelteFlow>
  {/if}
</div>
