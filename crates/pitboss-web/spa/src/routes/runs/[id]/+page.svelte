<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/state';
  import {
    getRun,
    getResolvedManifest,
    getManifestToml,
    getSummaryJsonl,
    type RunDetailDto,
    ApiError
  } from '$lib/api';
  import { formatUnixSeconds, relativeFromUnix } from '$lib/utils';
  import StatusBadge from '$lib/components/status-badge.svelte';
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
  import { ArrowLeft, ChevronRight, RefreshCw, AlertTriangle } from 'lucide-svelte';
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

  <Tabs value="tasks" class="w-full">
    <TabsList>
      <TabsTrigger value="tasks">Tasks ({tasksToRender.length})</TabsTrigger>
      <TabsTrigger value="manifest">Manifest</TabsTrigger>
      <TabsTrigger value="resolved">Resolved</TabsTrigger>
      <TabsTrigger value="summary">Summary JSON</TabsTrigger>
    </TabsList>

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
{/if}
