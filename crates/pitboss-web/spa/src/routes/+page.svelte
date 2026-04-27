<script lang="ts">
  import { onMount } from 'svelte';
  import { listRuns, type RunDto, ApiError } from '$lib/api';
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
  import { RefreshCw, AlertTriangle } from 'lucide-svelte';

  let runs = $state<RunDto[] | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);

  async function load() {
    loading = true;
    error = null;
    try {
      runs = await listRuns();
    } catch (e) {
      runs = null;
      error = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
    } finally {
      loading = false;
    }
  }

  onMount(load);
</script>

<svelte:head>
  <title>Runs — Pitboss</title>
</svelte:head>

<div class="mb-6 flex items-center justify-between">
  <div>
    <h1 class="text-2xl font-semibold tracking-tight">Runs</h1>
    <p class="text-muted-foreground text-sm">All dispatched runs visible to this console.</p>
  </div>
  <Button variant="outline" size="sm" onclick={load} disabled={loading}>
    <RefreshCw class="mr-2 size-4 {loading ? 'animate-spin' : ''}" />
    Refresh
  </Button>
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

<Card>
  <Table>
    <TableHeader>
      <TableRow>
        <TableHead class="w-[26ch]">Run ID</TableHead>
        <TableHead class="w-[10ch]">Status</TableHead>
        <TableHead class="w-[8ch] text-right">Tasks</TableHead>
        <TableHead class="w-[8ch] text-right">Failed</TableHead>
        <TableHead>Updated</TableHead>
      </TableRow>
    </TableHeader>
    <TableBody>
      {#if runs === null && !error}
        <TableRow>
          <TableCell colspan={5} class="text-muted-foreground py-12 text-center text-sm">
            Loading runs…
          </TableCell>
        </TableRow>
      {:else if runs && runs.length === 0}
        <TableRow>
          <TableCell colspan={5} class="text-muted-foreground py-12 text-center text-sm">
            No runs found. Dispatch one with <code class="bg-muted rounded px-1.5 py-0.5"
              >pitboss dispatch &lt;manifest.toml&gt;</code
            >.
          </TableCell>
        </TableRow>
      {:else if runs}
        {#each runs as r (r.run_id)}
          <TableRow class="hover:bg-muted/50 cursor-pointer">
            <TableCell>
              <a href="/runs/{r.run_id}" class="block">
                <code class="text-xs font-medium tracking-tight">{r.run_id.slice(0, 18)}…</code>
              </a>
            </TableCell>
            <TableCell>
              <StatusBadge status={r.status} label={r.status_label} />
            </TableCell>
            <TableCell class="text-right tabular-nums">{r.tasks_total}</TableCell>
            <TableCell class="text-right tabular-nums">
              {#if r.tasks_failed > 0}
                <span class="text-destructive font-medium">{r.tasks_failed}</span>
              {:else}
                <span class="text-muted-foreground">0</span>
              {/if}
            </TableCell>
            <TableCell>
              <span title={formatUnixSeconds(r.mtime_unix)} class="text-muted-foreground text-sm">
                {relativeFromUnix(r.mtime_unix)}
              </span>
            </TableCell>
          </TableRow>
        {/each}
      {/if}
    </TableBody>
  </Table>
</Card>
