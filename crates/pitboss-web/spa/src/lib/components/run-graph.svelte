<script lang="ts">
  import { SvelteFlow, Background, Controls, MiniMap, type Node, type Edge } from '@xyflow/svelte';
  import dagre from '@dagrejs/dagre';
  import '@xyflow/svelte/dist/style.css';
  import RunGraphNode from './run-graph-node.svelte';
  import type {
    WorkerEntry,
    ActorActivity,
    FailureReason,
    SubleadInfo,
    TaskRecord
  } from '$lib/api';

  let {
    workers,
    storeActivity,
    failures,
    subleads,
    tasks,
    selectedTaskId,
    onSelect
  }: {
    workers: WorkerEntry[];
    storeActivity: Record<string, ActorActivity>;
    failures: Record<string, FailureReason>;
    subleads: Record<string, SubleadInfo>;
    /** Per-task records from `summary.jsonl` / `summary.json`. Looked up
     *  by `task_id` to densify each tile (timestamps, tokens, cost,
     *  counters, failure reason) and to power the inspector. */
    tasks: TaskRecord[];
    /** Currently-inspected task_id. The matching node renders with a
     *  ring; the inspector panel reads from this same value. Bindable so
     *  ESC / overlay click in the inspector clears the selection too. */
    selectedTaskId?: string | null;
    /** Fires on node click. Parent updates `selectedTaskId` to open the
     *  inspector. Toggling on the same node clears the selection. */
    onSelect?: (taskId: string | null) => void;
  } = $props();

  const NODE_WIDTH = 208; // matches w-52 in run-graph-node.svelte
  const NODE_HEIGHT = 130; // grew from 90 to fit the new dense rows

  /** Build {nodes, edges} for the current snapshot, then run dagre for layout. */
  function layout(
    workers: WorkerEntry[],
    storeActivity: Record<string, ActorActivity>,
    failures: Record<string, FailureReason>,
    subleads: Record<string, SubleadInfo>,
    tasks: TaskRecord[],
    selectedTaskId: string | null | undefined
  ): { nodes: Node[]; edges: Edge[] } {
    const g = new dagre.graphlib.Graph();
    g.setGraph({ rankdir: 'TB', nodesep: 32, ranksep: 64 });
    g.setDefaultEdgeLabel(() => ({}));

    const knownIds = new Set(workers.map((w) => w.task_id));
    const taskById = new Map(tasks.map((t) => [t.task_id, t]));

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
          sublead: subleads[w.task_id],
          task: taskById.get(w.task_id),
          selected: selectedTaskId === w.task_id
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
  let layoutResult = $derived(
    layout(workers, storeActivity, failures, subleads, tasks, selectedTaskId)
  );
  let nodes = $state<Node[]>([]);
  let edges = $state<Edge[]>([]);
  $effect(() => {
    nodes = layoutResult.nodes;
    edges = layoutResult.edges;
  });

  const nodeTypes = { runNode: RunGraphNode };

  // SvelteFlow's nodeclick handler. Clicking the already-selected node
  // clears the selection (matches the issue's "click again to dismiss"
  // muscle memory).
  function handleNodeClick({ node }: { node: Node }): void {
    if (!onSelect) return;
    onSelect(selectedTaskId === node.id ? null : node.id);
  }
</script>

<div class="bg-muted/20 dark:bg-muted/5 h-[60vh] w-full rounded-md border">
  {#if workers.length === 0}
    <div class="text-muted-foreground flex h-full items-center justify-center text-xs">
      No actors recorded for this run yet.
    </div>
  {:else}
    <SvelteFlow
      bind:nodes
      bind:edges
      {nodeTypes}
      fitView
      nodesDraggable={false}
      nodesConnectable={false}
      onnodeclick={handleNodeClick}
      proOptions={{ hideAttribution: true }}
    >
      <Background />
      <Controls />
      <MiniMap pannable zoomable />
    </SvelteFlow>
  {/if}
</div>
