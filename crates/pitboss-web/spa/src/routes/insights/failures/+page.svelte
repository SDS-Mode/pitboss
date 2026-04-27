<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import {
    listInsightsFailures,
    listInsightsClusters,
    listInsightsManifests,
    type Cluster,
    type TaskFailureDigest,
    type ManifestSummary,
    type InsightsFilter,
    ApiError
  } from '$lib/api';
  import { formatUnixSeconds, relativeFromUnix } from '$lib/utils';
  import {
    Table,
    TableBody,
    TableCell,
    TableHead,
    TableHeader,
    TableRow
  } from '$lib/components/ui/table';
  import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card';
  import { Button } from '$lib/components/ui/button';
  import { Badge } from '$lib/components/ui/badge';
  import { RefreshCw, AlertTriangle, X } from 'lucide-svelte';
  import EChart from '$lib/components/charts/echart.svelte';
  import TemplatePills from '$lib/components/insights/template-pills.svelte';

  type Window = '1h' | '24h' | '7d' | '30d' | 'all';
  const WINDOW_SECS: Record<Window, number | null> = {
    '1h': 3600,
    '24h': 86400,
    '7d': 7 * 86400,
    '30d': 30 * 86400,
    all: null
  };

  let win = $state<Window>('24h');
  let manifest = $state<string | null>(null);
  let kind = $state<string | null>(null);

  let failures = $state<TaskFailureDigest[]>([]);
  let clusters = $state<Cluster[]>([]);
  let manifests = $state<ManifestSummary[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);

  // Hydrate state from URL on first mount.
  onMount(() => {
    const url = $page.url;
    const w = url.searchParams.get('win') as Window | null;
    if (w && w in WINDOW_SECS) win = w;
    manifest = url.searchParams.get('manifest');
    kind = url.searchParams.get('kind');
    void load();
  });

  function syncUrl() {
    const params = new URLSearchParams();
    if (win !== '24h') params.set('win', win);
    if (manifest) params.set('manifest', manifest);
    if (kind) params.set('kind', kind);
    const qs = params.toString();
    void goto(`/insights/failures${qs ? `?${qs}` : ''}`, { replaceState: true, noScroll: true, keepFocus: true });
  }

  function currentFilter(): InsightsFilter {
    const secs = WINDOW_SECS[win];
    const since = secs ? Math.floor(Date.now() / 1000) - secs : undefined;
    return {
      since,
      manifest: manifest ?? undefined,
      kind: kind ?? undefined
    };
  }

  async function load() {
    loading = true;
    error = null;
    syncUrl();
    try {
      const f = currentFilter();
      const [failuresRes, clustersRes, manifestsRes] = await Promise.all([
        listInsightsFailures({ ...f, limit: 50 }),
        listInsightsClusters(f),
        listInsightsManifests({ since: f.since })
      ]);
      failures = failuresRes.failures;
      clusters = clustersRes.clusters;
      manifests = manifestsRes.manifests;
    } catch (e) {
      error = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
    } finally {
      loading = false;
    }
  }

  // KPI strip.
  const distinctManifests = $derived(new Set(failures.map((f) => f.manifest_name)).size);
  const distinctKinds = $derived(new Set(failures.map((f) => f.failure_kind)).size);
  const topKind = $derived.by(() => {
    if (clusters.length === 0) return '—';
    const counts: Record<string, number> = {};
    for (const c of clusters) counts[c.kind] = (counts[c.kind] ?? 0) + c.count;
    let top = '—';
    let max = 0;
    for (const [k, n] of Object.entries(counts)) {
      if (n > max) {
        max = n;
        top = k;
      }
    }
    return top;
  });

  // Failure-kind bar chart option (clickable).
  const kindBarOption = $derived.by(() => {
    const counts: Record<string, number> = {};
    for (const c of clusters) counts[c.kind] = (counts[c.kind] ?? 0) + c.count;
    const entries = Object.entries(counts).sort((a, b) => b[1] - a[1]);
    return {
      grid: { left: 100, right: 16, top: 16, bottom: 24 },
      tooltip: { trigger: 'axis', axisPointer: { type: 'shadow' } },
      xAxis: { type: 'value' },
      yAxis: { type: 'category', data: entries.map((e) => e[0]).reverse() },
      series: [
        {
          type: 'bar',
          data: entries.map((e) => e[1]).reverse(),
          itemStyle: { color: '#dc2626' }
        }
      ]
    };
  });

  function onKindBarClick(params: unknown) {
    const p = params as { name?: string };
    if (p?.name) {
      kind = p.name;
      void load();
    }
  }

  // Time-of-day heatmap: day-of-week × hour-of-day grid.
  const heatmapOption = $derived.by(() => {
    const grid: number[][] = Array.from({ length: 7 }, () => Array(24).fill(0));
    for (const f of failures) {
      if (f.occurred_at == null) continue;
      const d = new Date(f.occurred_at * 1000);
      const dow = d.getDay();
      const hod = d.getHours();
      grid[dow][hod]++;
    }
    const data: [number, number, number][] = [];
    let max = 0;
    for (let dow = 0; dow < 7; dow++) {
      for (let hod = 0; hod < 24; hod++) {
        const n = grid[dow][hod];
        data.push([hod, dow, n]);
        if (n > max) max = n;
      }
    }
    return {
      grid: { left: 36, right: 16, top: 24, bottom: 24 },
      tooltip: { position: 'top' },
      xAxis: {
        type: 'category',
        data: Array.from({ length: 24 }, (_, i) => `${i}`),
        splitArea: { show: true }
      },
      yAxis: {
        type: 'category',
        data: ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'],
        splitArea: { show: true }
      },
      visualMap: {
        min: 0,
        max: Math.max(1, max),
        calculable: false,
        orient: 'horizontal',
        left: 'center',
        bottom: 0,
        show: false,
        inRange: { color: ['#1e293b', '#dc2626'] }
      },
      series: [
        {
          type: 'heatmap',
          data,
          label: { show: false }
        }
      ]
    };
  });

  function clearFilter(which: 'manifest' | 'kind') {
    if (which === 'manifest') manifest = null;
    if (which === 'kind') kind = null;
    void load();
  }
</script>

<svelte:head>
  <title>Failures — Pitboss insights</title>
</svelte:head>

<div class="mb-6 flex items-center justify-between">
  <div>
    <h1 class="text-2xl font-semibold tracking-tight">Failures</h1>
    <p class="text-muted-foreground text-sm">
      Cross-run failure patterns mined from <code class="bg-muted rounded px-1 text-xs"
        >FailureReason</code
      > + Drain-lite templates.
    </p>
  </div>
  <Button variant="outline" size="sm" onclick={load} disabled={loading}>
    <RefreshCw class="mr-2 size-4 {loading ? 'animate-spin' : ''}" />
    Refresh
  </Button>
</div>

<!-- Window + filter pills -->
<div class="mb-4 flex flex-wrap items-center gap-2">
  {#each Object.keys(WINDOW_SECS) as w (w)}
    <Button
      variant={win === w ? 'default' : 'outline'}
      size="sm"
      onclick={() => {
        win = w as Window;
        void load();
      }}
    >
      {w}
    </Button>
  {/each}

  {#if manifest}
    <Badge variant="secondary" class="ml-2 gap-1">
      manifest: {manifest}
      <button onclick={() => clearFilter('manifest')} aria-label="Clear manifest filter">
        <X class="size-3" />
      </button>
    </Badge>
  {/if}
  {#if kind}
    <Badge variant="secondary" class="gap-1">
      kind: {kind}
      <button onclick={() => clearFilter('kind')} aria-label="Clear kind filter">
        <X class="size-3" />
      </button>
    </Badge>
  {/if}
</div>

{#if error}
  <Card class="border-destructive/50 mb-6">
    <CardContent class="flex items-start gap-3 pt-6">
      <AlertTriangle class="text-destructive mt-0.5 size-5 shrink-0" />
      <div>
        <p class="text-destructive font-medium">Failed to load insights</p>
        <p class="text-muted-foreground mt-1 text-sm">{error}</p>
      </div>
    </CardContent>
  </Card>
{/if}

<!-- KPI strip -->
<div class="mb-4 grid grid-cols-2 gap-3 md:grid-cols-4">
  <Card>
    <CardContent class="pt-6">
      <p class="text-muted-foreground text-xs uppercase">Total failures</p>
      <p class="mt-1 text-2xl font-semibold tabular-nums">{failures.length}</p>
    </CardContent>
  </Card>
  <Card>
    <CardContent class="pt-6">
      <p class="text-muted-foreground text-xs uppercase">Manifests affected</p>
      <p class="mt-1 text-2xl font-semibold tabular-nums">{distinctManifests}</p>
    </CardContent>
  </Card>
  <Card>
    <CardContent class="pt-6">
      <p class="text-muted-foreground text-xs uppercase">Distinct kinds</p>
      <p class="mt-1 text-2xl font-semibold tabular-nums">{distinctKinds}</p>
    </CardContent>
  </Card>
  <Card>
    <CardContent class="pt-6">
      <p class="text-muted-foreground text-xs uppercase">Top kind</p>
      <p class="mt-1 truncate text-2xl font-semibold">{topKind}</p>
    </CardContent>
  </Card>
</div>

<!-- Charts row -->
<div class="mb-4 grid grid-cols-1 gap-3 lg:grid-cols-2">
  <Card>
    <CardHeader class="pb-2">
      <CardTitle class="text-sm">Failure kinds (click to filter)</CardTitle>
    </CardHeader>
    <CardContent>
      {#if clusters.length === 0}
        <p class="text-muted-foreground py-12 text-center text-sm">No failures in this window.</p>
      {:else}
        <EChart option={kindBarOption} onclick={onKindBarClick} class="h-72 w-full" />
      {/if}
    </CardContent>
  </Card>

  <Card>
    <CardHeader class="pb-2">
      <CardTitle class="text-sm">Time-of-day distribution</CardTitle>
    </CardHeader>
    <CardContent>
      {#if failures.length === 0}
        <p class="text-muted-foreground py-12 text-center text-sm">No failures in this window.</p>
      {:else}
        <EChart option={heatmapOption} class="h-72 w-full" />
      {/if}
    </CardContent>
  </Card>
</div>

<!-- Top clusters -->
<Card class="mb-4">
  <CardHeader class="pb-2">
    <CardTitle class="text-sm">Top failure clusters</CardTitle>
  </CardHeader>
  <Table>
    <TableHeader>
      <TableRow>
        <TableHead class="w-[6ch] text-right">Count</TableHead>
        <TableHead class="w-[14ch]">Kind</TableHead>
        <TableHead>Template</TableHead>
        <TableHead class="w-[10ch]">Manifests</TableHead>
        <TableHead class="w-[10ch]">Last seen</TableHead>
      </TableRow>
    </TableHeader>
    <TableBody>
      {#if clusters.length === 0}
        <TableRow>
          <TableCell colspan={5} class="text-muted-foreground py-12 text-center text-sm">
            No clusters in this window.
          </TableCell>
        </TableRow>
      {:else}
        {#each clusters as c, i (i)}
          <TableRow>
            <TableCell class="text-right tabular-nums font-medium">{c.count}</TableCell>
            <TableCell>
              <Badge variant="outline">{c.kind}</Badge>
            </TableCell>
            <TableCell>
              {#if c.template}
                <TemplatePills template={c.template} />
              {:else if c.exemplar_message}
                <span class="text-muted-foreground font-mono text-xs">{c.exemplar_message}</span>
              {:else}
                <span class="text-muted-foreground italic text-xs">structured failure</span>
              {/if}
            </TableCell>
            <TableCell class="text-xs">
              {c.manifests.length === 1 ? c.manifests[0] : `${c.manifests.length} manifests`}
            </TableCell>
            <TableCell class="text-muted-foreground text-xs">
              {c.last_seen ? relativeFromUnix(c.last_seen) : '—'}
            </TableCell>
          </TableRow>
        {/each}
      {/if}
    </TableBody>
  </Table>
</Card>

<!-- Recent failures -->
<Card>
  <CardHeader class="pb-2">
    <CardTitle class="text-sm">Recent failures</CardTitle>
  </CardHeader>
  <Table>
    <TableHeader>
      <TableRow>
        <TableHead class="w-[12ch]">Manifest</TableHead>
        <TableHead class="w-[14ch]">Task</TableHead>
        <TableHead class="w-[14ch]">Kind</TableHead>
        <TableHead>Message</TableHead>
        <TableHead class="w-[10ch]">When</TableHead>
        <TableHead class="w-[6ch]"></TableHead>
      </TableRow>
    </TableHeader>
    <TableBody>
      {#if failures.length === 0}
        <TableRow>
          <TableCell colspan={6} class="text-muted-foreground py-12 text-center text-sm">
            No recent failures.
          </TableCell>
        </TableRow>
      {:else}
        {#each failures as f, i (i)}
          <TableRow>
            <TableCell class="text-xs">{f.manifest_name}</TableCell>
            <TableCell><code class="text-xs">{f.task_id}</code></TableCell>
            <TableCell><Badge variant="outline" class="text-[10px]">{f.failure_kind}</Badge></TableCell>
            <TableCell class="text-muted-foreground truncate font-mono text-xs">
              {f.error_message ?? '—'}
            </TableCell>
            <TableCell class="text-muted-foreground text-xs">
              {f.occurred_at ? relativeFromUnix(f.occurred_at) : '—'}
            </TableCell>
            <TableCell>
              <a
                href="/runs/{f.run_id}"
                class="text-primary text-xs underline-offset-2 hover:underline"
              >
                run
              </a>
            </TableCell>
          </TableRow>
        {/each}
      {/if}
    </TableBody>
  </Table>
</Card>

{#if manifests.length > 0}
  <Card class="mt-4">
    <CardHeader class="pb-2">
      <CardTitle class="text-sm">Manifest health</CardTitle>
    </CardHeader>
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Manifest</TableHead>
          <TableHead class="w-[8ch] text-right">Runs</TableHead>
          <TableHead class="w-[8ch] text-right">Failed</TableHead>
          <TableHead class="w-[10ch] text-right">Success</TableHead>
          <TableHead class="w-[12ch]">Last run</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {#each manifests as m (m.manifest_name)}
          <TableRow>
            <TableCell class="text-xs font-medium">{m.manifest_name}</TableCell>
            <TableCell class="text-right tabular-nums">{m.runs_total}</TableCell>
            <TableCell class="text-right tabular-nums">
              {#if m.runs_failed > 0}
                <span class="text-destructive">{m.runs_failed}</span>
              {:else}
                <span class="text-muted-foreground">0</span>
              {/if}
            </TableCell>
            <TableCell class="text-right tabular-nums text-xs">
              {(m.success_rate * 100).toFixed(0)}%
            </TableCell>
            <TableCell class="text-muted-foreground text-xs">
              <span title={m.last_run_at ? formatUnixSeconds(m.last_run_at) : ''}>
                {m.last_run_at ? relativeFromUnix(m.last_run_at) : '—'}
              </span>
            </TableCell>
          </TableRow>
        {/each}
      </TableBody>
    </Table>
  </Card>
{/if}
