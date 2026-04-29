<script lang="ts">
  import { page } from '$app/state';
  import { getTaskLog, getTaskDetail, ApiError, type TaskRecord } from '$lib/api';
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
  let detail = $state<TaskRecord | null>(null);
  const limit = 256 * 1024; // 256 KiB

  async function load() {
    loading = true;
    error = null;
    try {
      // Fetch metadata + log in parallel; metadata 404 is non-fatal
      // (older runs may pre-date the endpoint, summary.jsonl missing).
      const [logResult, detailResult] = await Promise.allSettled([
        getTaskLog(runId, taskId, { limit, tail }),
        getTaskDetail(runId, taskId),
      ]);
      if (logResult.status === 'fulfilled') {
        log = logResult.value;
      } else {
        log = null;
        const e = logResult.reason;
        error = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
      }
      detail = detailResult.status === 'fulfilled' ? detailResult.value : null;
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    if (runId && taskId) load();
  });

  function fmtDuration(ms: number): string {
    if (ms < 1000) return `${ms} ms`;
    if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
    const m = Math.floor(ms / 60_000);
    const s = Math.floor((ms % 60_000) / 1000);
    return `${m}m ${s}s`;
  }

  function statusVariant(s: string): 'default' | 'destructive' | 'secondary' | 'outline' {
    if (s === 'Success') return 'default';
    if (s === 'Failed' || s === 'SpawnFailed') return 'destructive';
    if (s === 'Cancelled' || s === 'TimedOut') return 'secondary';
    return 'outline';
  }

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

{#if detail}
  <Card class="mb-4">
    <CardContent class="grid grid-cols-2 gap-x-6 gap-y-2 pt-4 text-sm md:grid-cols-4">
      <div>
        <div class="text-muted-foreground text-xs">Status</div>
        <Badge variant={statusVariant(detail.status)}>{detail.status}</Badge>
      </div>
      <div>
        <div class="text-muted-foreground text-xs">Duration</div>
        <div>{fmtDuration(detail.duration_ms)}</div>
      </div>
      <div>
        <div class="text-muted-foreground text-xs">Exit code</div>
        <div><code class="text-xs">{detail.exit_code ?? '—'}</code></div>
      </div>
      <div>
        <div class="text-muted-foreground text-xs">Model</div>
        <div><code class="text-xs">{detail.model ?? '—'}</code></div>
      </div>
      <div>
        <div class="text-muted-foreground text-xs">Tokens (in / out)</div>
        <div>
          <code class="text-xs"
            >{detail.token_usage.input.toLocaleString()} / {detail.token_usage.output.toLocaleString()}</code
          >
        </div>
      </div>
      <div>
        <div class="text-muted-foreground text-xs">Approvals (req / ok / rej)</div>
        <div>
          <code class="text-xs"
            >{detail.approvals_requested} / {detail.approvals_approved} / {detail.approvals_rejected}</code
          >
        </div>
      </div>
      <div>
        <div class="text-muted-foreground text-xs">Parent</div>
        <div>
          {#if detail.parent_task_id}
            <code class="text-xs">{detail.parent_task_id}</code>
          {:else}
            <span class="text-muted-foreground text-xs">root</span>
          {/if}
        </div>
      </div>
      <div>
        <div class="text-muted-foreground text-xs">Pause / reprompt</div>
        <div><code class="text-xs">{detail.pause_count} / {detail.reprompt_count}</code></div>
      </div>
      {#if detail.failure_reason}
        <div class="col-span-2 md:col-span-4">
          <div class="text-muted-foreground text-xs">Failure</div>
          <pre class="bg-muted/40 mt-1 max-h-32 overflow-auto rounded p-2 text-xs"><code
              >{JSON.stringify(detail.failure_reason, null, 2)}</code
            ></pre>
        </div>
      {/if}
      {#if detail.final_message_preview}
        <div class="col-span-2 md:col-span-4">
          <div class="text-muted-foreground text-xs">Final message</div>
          <p class="mt-1 text-sm">{detail.final_message_preview}</p>
        </div>
      {/if}
    </CardContent>
  </Card>
{/if}

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
