<script lang="ts">
  import { page } from '$app/state';
  import { getTaskLog, ApiError } from '$lib/api';
  import { Card, CardContent } from '$lib/components/ui/card';
  import { Button } from '$lib/components/ui/button';
  import { Badge } from '$lib/components/ui/badge';
  import { ArrowLeft, ChevronRight, RefreshCw, AlertTriangle, Download } from 'lucide-svelte';

  const runId = $derived(page.params.id ?? '');
  const taskId = $derived(page.params.task_id ?? '');

  let log = $state<string | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(false);
  let tail = $state(true);
  const limit = 256 * 1024; // 256 KiB

  async function load() {
    loading = true;
    error = null;
    try {
      log = await getTaskLog(runId, taskId, { limit, tail });
    } catch (e) {
      log = null;
      error = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    if (runId && taskId) load();
  });

  function downloadFull() {
    const url = `/api/runs/${encodeURIComponent(runId)}/tasks/${encodeURIComponent(taskId)}/log?limit=8388608`;
    window.open(url, '_blank');
  }
</script>

<svelte:head>
  <title>Task {taskId} — Pitboss</title>
</svelte:head>

<div class="mb-4 flex items-center gap-3 text-sm">
  <Button variant="ghost" size="sm" href="/">
    <ArrowLeft class="mr-1.5 size-4" /> All runs
  </Button>
  <ChevronRight class="text-muted-foreground size-4" />
  <a href="/runs/{runId}" class="hover:text-foreground text-muted-foreground">
    <code class="text-xs">{runId.slice(0, 18)}…</code>
  </a>
  <ChevronRight class="text-muted-foreground size-4" />
  <code class="text-xs font-medium">{taskId}</code>
</div>

<div class="mb-4 flex items-start justify-between gap-4">
  <div>
    <h1 class="text-xl font-semibold tracking-tight">Task log</h1>
    <p class="text-muted-foreground text-xs">
      <code>tasks/{taskId}/stdout.log</code>
    </p>
  </div>
  <div class="flex items-center gap-2">
    <Badge
      variant={tail ? 'default' : 'outline'}
      class="cursor-pointer"
      onclick={() => (tail = !tail)}
    >
      {tail ? 'Tail' : 'Head'}
    </Badge>
    <Button variant="outline" size="sm" onclick={downloadFull}>
      <Download class="mr-2 size-4" /> Full
    </Button>
    <Button variant="outline" size="sm" onclick={load} disabled={loading}>
      <RefreshCw class="mr-2 size-4 {loading ? 'animate-spin' : ''}" />
      Refresh
    </Button>
  </div>
</div>

{#if error}
  <Card class="border-destructive/50">
    <CardContent class="flex items-start gap-3 pt-6">
      <AlertTriangle class="text-destructive mt-0.5 size-5 shrink-0" />
      <div>
        <p class="text-destructive font-medium">Failed to load log</p>
        <p class="text-muted-foreground mt-1 text-sm">{error}</p>
      </div>
    </CardContent>
  </Card>
{:else}
  <Card>
    <CardContent class="pt-4">
      {#if log === null}
        <p class="text-muted-foreground py-6 text-center text-sm">Loading log…</p>
      {:else if log.length === 0}
        <p class="text-muted-foreground py-6 text-center text-sm">Log is empty.</p>
      {:else}
        <pre
          class="bg-muted/40 max-h-[75vh] overflow-auto rounded-md p-4 text-xs leading-relaxed font-mono"><code
            >{log}</code
          ></pre>
      {/if}
    </CardContent>
  </Card>
{/if}
