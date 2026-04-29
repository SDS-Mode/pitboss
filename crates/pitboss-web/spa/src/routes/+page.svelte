<script lang="ts">
  import { onMount } from 'svelte';
  import {
    listInsightsRuns,
    listInsightsManifests,
    type RunDigest,
    type ManifestSummary,
    ApiError
  } from '$lib/api';
  import { formatUnixSeconds, relativeFromUnix } from '$lib/utils';
  import StatusBadge from '$lib/components/status-badge.svelte';
  import {
    Table,
    TableBody,
    TableCell,
    TableHead,
    TableHeader,
    TableRow
  } from '$lib/components/ui/table';
  import { Card, CardContent } from '$lib/components/ui/card';
  import { Button } from '$lib/components/ui/button';
  import { Badge } from '$lib/components/ui/badge';
  import { RefreshCw, AlertTriangle, X, ChevronRight } from 'lucide-svelte';

  let runs = $state<RunDigest[]>([]);
  let manifests = $state<ManifestSummary[]>([]);
  let error = $state<string | null>(null);
  let loading = $state(false);

  let manifestFilter = $state<string | null>(null);
  let statusFilter = $state<string | null>(null);
  let groupByManifest = $state(false);

  async function load() {
    loading = true;
    error = null;
    try {
      const [r, m] = await Promise.all([
        listInsightsRuns({
          manifest: manifestFilter ?? undefined,
          status: statusFilter ?? undefined
        }),
        listInsightsManifests({})
      ]);
      runs = r.runs;
      manifests = m.manifests;
    } catch (e) {
      error = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
    } finally {
      loading = false;
    }
  }

  // Poll every 5 s, but only when the tab is visible — keeps a tab
  // left open all day from burning cycles. visibilitychange fires
  // an immediate refresh when the operator comes back to the tab so
  // they don't see stale data while the next interval lands.
  onMount(() => {
    void load();
    const POLL_MS = 5000;
    const handle = setInterval(() => {
      if (document.visibilityState === 'visible') void load();
    }, POLL_MS);
    const onVisibility = () => {
      if (document.visibilityState === 'visible') void load();
    };
    document.addEventListener('visibilitychange', onVisibility);
    return () => {
      clearInterval(handle);
      document.removeEventListener('visibilitychange', onVisibility);
    };
  });

  // Facet counts derived from the filtered run set so the operator
  // sees how their filter narrows things.
  const statusCounts = $derived.by(() => {
    const m: Record<string, number> = {};
    for (const r of runs) m[r.status] = (m[r.status] ?? 0) + 1;
    return m;
  });

  const failureKindCounts = $derived.by(() => {
    const m: Record<string, number> = {};
    for (const r of runs) for (const k of r.failure_kinds) m[k] = (m[k] ?? 0) + 1;
    return Object.entries(m).sort((a, b) => b[1] - a[1]);
  });

  // Group-by-manifest projection.
  type Group = {
    manifest_name: string;
    runs: RunDigest[];
    runs_total: number;
    runs_failed: number;
    last_run_at: number;
  };
  const grouped = $derived.by<Group[]>(() => {
    const map = new Map<string, Group>();
    for (const r of runs) {
      const g =
        map.get(r.manifest_name) ??
        ({
          manifest_name: r.manifest_name,
          runs: [],
          runs_total: 0,
          runs_failed: 0,
          last_run_at: 0
        } as Group);
      g.runs.push(r);
      g.runs_total++;
      if (r.tasks_failed > 0) g.runs_failed++;
      if ((r.started_at ?? 0) > g.last_run_at) g.last_run_at = r.started_at ?? 0;
      map.set(r.manifest_name, g);
    }
    return [...map.values()].sort((a, b) => b.last_run_at - a.last_run_at);
  });

  let expanded = $state<Record<string, boolean>>({});

  function toggleGroup(name: string) {
    expanded[name] = !expanded[name];
  }

  function setManifest(name: string | null) {
    manifestFilter = name;
    void load();
  }

  function setStatus(s: string | null) {
    statusFilter = s;
    void load();
  }
</script>

<svelte:head>
  <title>Runs — Pitboss</title>
</svelte:head>

<div class="mb-6 flex items-center justify-between">
  <div>
    <h1 class="text-2xl font-semibold tracking-tight">Runs</h1>
    <p class="text-muted-foreground text-sm">All dispatched runs visible to this console.</p>
  </div>
  <div class="flex items-center gap-2">
    <Button
      variant={groupByManifest ? 'default' : 'outline'}
      size="sm"
      onclick={() => (groupByManifest = !groupByManifest)}
    >
      Group by manifest
    </Button>
    <Button variant="outline" size="sm" onclick={load} disabled={loading}>
      <RefreshCw class="mr-2 size-4 {loading ? 'animate-spin' : ''}" />
      Refresh
    </Button>
  </div>
</div>

<!-- Facet pill row -->
<div class="mb-4 flex flex-wrap items-center gap-2 text-xs">
  {#if manifestFilter}
    <Badge variant="secondary" class="gap-1">
      manifest: {manifestFilter}
      <button onclick={() => setManifest(null)} aria-label="Clear manifest filter">
        <X class="size-3" />
      </button>
    </Badge>
  {/if}
  {#if statusFilter}
    <Badge variant="secondary" class="gap-1">
      status: {statusFilter}
      <button onclick={() => setStatus(null)} aria-label="Clear status filter">
        <X class="size-3" />
      </button>
    </Badge>
  {/if}
  {#each Object.entries(statusCounts) as [s, n] (s)}
    <button
      class="text-muted-foreground hover:text-foreground hover:bg-muted rounded-md px-2 py-1"
      onclick={() => setStatus(statusFilter === s ? null : s)}
    >
      {s} <span class="tabular-nums opacity-70">({n})</span>
    </button>
  {/each}
  {#if failureKindCounts.length > 0}
    <span class="text-muted-foreground/60 mx-1">·</span>
    {#each failureKindCounts as [k, n] (k)}
      <a
        href="/insights/failures?kind={encodeURIComponent(k)}"
        class="text-destructive/80 hover:text-destructive rounded-md px-2 py-1"
      >
        {k} <span class="tabular-nums opacity-70">({n})</span>
      </a>
    {/each}
  {/if}
</div>

{#if error}
  <Card class="border-destructive/50 mb-6">
    <CardContent class="flex items-start gap-3 pt-6">
      <AlertTriangle class="text-destructive mt-0.5 size-5 shrink-0" />
      <div>
        <p class="text-destructive font-medium">Failed to load runs</p>
        <p class="text-muted-foreground mt-1 text-sm">{error}</p>
      </div>
    </CardContent>
  </Card>
{/if}

{#if groupByManifest}
  <Card>
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Manifest</TableHead>
          <TableHead class="w-[8ch] text-right">Runs</TableHead>
          <TableHead class="w-[8ch] text-right">Failed</TableHead>
          <TableHead class="w-[10ch]">Last run</TableHead>
          <TableHead class="w-[6ch]"></TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {#if runs.length === 0}
          <TableRow>
            <TableCell colspan={5} class="text-muted-foreground py-12 text-center text-sm">
              No runs.
            </TableCell>
          </TableRow>
        {:else}
          {#each grouped as g (g.manifest_name)}
            <TableRow class="hover:bg-muted/50 cursor-pointer">
              <TableCell onclick={() => toggleGroup(g.manifest_name)}>
                <div class="flex items-center gap-1">
                  <ChevronRight
                    class="size-4 transition-transform {expanded[g.manifest_name]
                      ? 'rotate-90'
                      : ''}"
                  />
                  <button
                    class="text-foreground hover:underline"
                    onclick={(e) => {
                      e.stopPropagation();
                      setManifest(g.manifest_name);
                    }}
                  >
                    {g.manifest_name}
                  </button>
                </div>
              </TableCell>
              <TableCell class="text-right tabular-nums">{g.runs_total}</TableCell>
              <TableCell class="text-right tabular-nums">
                {#if g.runs_failed > 0}
                  <span class="text-destructive font-medium">{g.runs_failed}</span>
                {:else}
                  <span class="text-muted-foreground">0</span>
                {/if}
              </TableCell>
              <TableCell class="text-muted-foreground text-xs">
                {g.last_run_at ? relativeFromUnix(g.last_run_at) : '—'}
              </TableCell>
              <TableCell class="text-right text-xs"
                >{((1 - g.runs_failed / g.runs_total) * 100).toFixed(0)}%</TableCell
              >
            </TableRow>
            {#if expanded[g.manifest_name]}
              {#each g.runs as r (r.run_id)}
                <TableRow
                  class="bg-muted/20 {r.status === 'aborted' ? 'opacity-60 italic' : ''}"
                >
                  <TableCell class="pl-10">
                    <a href="/runs/{r.run_id}" class="block">
                      <code class="text-xs">{r.run_id.slice(0, 18)}…</code>
                    </a>
                  </TableCell>
                  <TableCell class="text-right tabular-nums text-xs">{r.tasks_total}</TableCell>
                  <TableCell class="text-right tabular-nums">
                    {#if r.tasks_failed > 0}
                      <span class="text-destructive font-medium">{r.tasks_failed}</span>
                    {:else}
                      <span class="text-muted-foreground">0</span>
                    {/if}
                  </TableCell>
                  <TableCell class="text-muted-foreground text-xs">
                    {r.started_at ? relativeFromUnix(r.started_at) : '—'}
                  </TableCell>
                  <TableCell><StatusBadge status={r.status} label={r.status} /></TableCell>
                </TableRow>
              {/each}
            {/if}
          {/each}
        {/if}
      </TableBody>
    </Table>
  </Card>
{:else}
  <Card>
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead class="w-[26ch]">Run ID</TableHead>
          <TableHead class="w-[14ch]">Manifest</TableHead>
          <TableHead class="w-[10ch]">Status</TableHead>
          <TableHead class="w-[8ch] text-right">Tasks</TableHead>
          <TableHead class="w-[8ch] text-right">Failed</TableHead>
          <TableHead>Updated</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {#if runs.length === 0 && !loading && !error}
          <TableRow>
            <TableCell colspan={6} class="text-muted-foreground py-12 text-center text-sm">
              No runs found. Dispatch one with <code class="bg-muted rounded px-1.5 py-0.5"
                >pitboss dispatch &lt;manifest.toml&gt;</code
              >.
            </TableCell>
          </TableRow>
        {:else}
          {#each runs as r (r.run_id)}
            <TableRow
              class="hover:bg-muted/50 cursor-pointer {r.status === 'aborted'
                ? 'opacity-60 italic'
                : ''}"
            >
              <TableCell>
                <a href="/runs/{r.run_id}" class="block">
                  <code class="text-xs font-medium tracking-tight">{r.run_id.slice(0, 18)}…</code>
                </a>
              </TableCell>
              <TableCell>
                <button
                  class="text-foreground/80 hover:underline text-xs"
                  onclick={() => setManifest(r.manifest_name)}
                >
                  {r.manifest_name}
                </button>
              </TableCell>
              <TableCell>
                <StatusBadge status={r.status} label={r.status} />
              </TableCell>
              <TableCell class="text-right tabular-nums">{r.tasks_total}</TableCell>
              <TableCell class="text-right tabular-nums">
                {#if r.tasks_failed > 0}
                  <span class="text-destructive font-medium">{r.tasks_failed}</span>
                  {#if r.failure_kinds.length > 0}
                    <a
                      href="/insights/failures?manifest={encodeURIComponent(
                        r.manifest_name
                      )}&kind={encodeURIComponent(r.failure_kinds[0])}"
                      class="text-destructive/80 ml-1 text-[10px] hover:underline"
                      title="View this failure kind across runs"
                    >
                      {r.failure_kinds[0]}
                    </a>
                  {/if}
                {:else}
                  <span class="text-muted-foreground">0</span>
                {/if}
              </TableCell>
              <TableCell>
                <span
                  title={r.started_at ? formatUnixSeconds(r.started_at) : ''}
                  class="text-muted-foreground text-sm"
                >
                  {r.started_at ? relativeFromUnix(r.started_at) : '—'}
                </span>
              </TableCell>
            </TableRow>
          {/each}
        {/if}
      </TableBody>
    </Table>
  </Card>
{/if}

{#if manifests.length > 0 && !groupByManifest}
  <p class="text-muted-foreground mt-4 text-xs">
    {manifests.length} manifest{manifests.length === 1 ? '' : 's'} tracked. Toggle <span
      class="text-foreground">Group by manifest</span
    > to roll up.
  </p>
{/if}
