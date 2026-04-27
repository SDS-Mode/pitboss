<script lang="ts">
  import { goto } from '$app/navigation';
  import { listManifests, saveManifest, ApiError, type ManifestEntry } from '$lib/api';
  import { relativeFromUnix } from '$lib/utils';
  import {
    Card,
    CardContent,
    CardDescription,
    CardHeader,
    CardTitle
  } from '$lib/components/ui/card';
  import { Button } from '$lib/components/ui/button';
  import {
    Table,
    TableBody,
    TableCell,
    TableHead,
    TableHeader,
    TableRow
  } from '$lib/components/ui/table';
  import { FileText, Plus, AlertTriangle, RefreshCw } from 'lucide-svelte';

  let manifests = $state<ManifestEntry[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);

  async function load() {
    loading = true;
    error = null;
    try {
      manifests = await listManifests();
    } catch (e) {
      error = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    load();
  });

  async function createNew() {
    const name = window.prompt('Name for the new manifest (without .toml)?');
    if (!name || !name.trim()) return;
    const stub = `# Pitboss manifest — ${name.trim()}\n\n[run]\nname = "${name.trim()}"\nmax_parallel_tasks = 2\n\n[[task]]\nid = "first"\ndirectory = "."\nprompt = "Replace me with the work for this task."\n`;
    try {
      const res = await saveManifest(name.trim(), stub);
      await goto(`/manifests/${encodeURIComponent(res.name)}`);
    } catch (e) {
      const msg = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
      window.alert(`Failed to create manifest: ${msg}`);
    }
  }

  function fmtBytes(n: number): string {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`;
    return `${(n / 1024 / 1024).toFixed(1)} MiB`;
  }
</script>

<svelte:head>
  <title>Manifests — Pitboss</title>
</svelte:head>

<div class="mb-6 flex items-end justify-between gap-3">
  <div>
    <h1 class="text-xl font-semibold tracking-tight">Manifests</h1>
    <p class="text-muted-foreground mt-1 text-sm">
      Workspace for authoring and dispatching pitboss manifests.
    </p>
  </div>
  <div class="flex items-center gap-2">
    <Button variant="outline" size="sm" onclick={load} disabled={loading}>
      <RefreshCw class="mr-2 size-4 {loading ? 'animate-spin' : ''}" />
      Refresh
    </Button>
    <Button size="sm" onclick={createNew}>
      <Plus class="mr-1.5 size-4" /> New manifest
    </Button>
  </div>
</div>

{#if error}
  <Card class="border-destructive/50">
    <CardContent class="flex items-start gap-3 pt-6">
      <AlertTriangle class="text-destructive mt-0.5 size-5 shrink-0" />
      <div>
        <p class="text-destructive font-medium">Failed to load manifests</p>
        <p class="text-muted-foreground mt-1 text-sm">{error}</p>
      </div>
    </CardContent>
  </Card>
{:else if loading && manifests.length === 0}
  <Card>
    <CardContent class="text-muted-foreground py-12 text-center text-sm">Loading…</CardContent>
  </Card>
{:else if manifests.length === 0}
  <Card>
    <CardContent class="flex flex-col items-center gap-3 py-16 text-center">
      <FileText class="text-muted-foreground size-10" />
      <div>
        <p class="text-sm font-medium">No manifests yet</p>
        <p class="text-muted-foreground mt-1 text-sm">
          Create one to start dispatching runs from the console.
        </p>
      </div>
      <Button size="sm" onclick={createNew}>
        <Plus class="mr-1.5 size-4" /> New manifest
      </Button>
    </CardContent>
  </Card>
{:else}
  <Card>
    <CardHeader>
      <CardTitle class="text-base">Workspace</CardTitle>
      <CardDescription class="text-xs">
        {manifests.length} manifest{manifests.length === 1 ? '' : 's'} on disk.
      </CardDescription>
    </CardHeader>
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>Name</TableHead>
          <TableHead class="w-[12ch] text-right">Size</TableHead>
          <TableHead class="w-[18ch]">Modified</TableHead>
          <TableHead class="w-[10ch]"></TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {#each manifests as m (m.name)}
          <TableRow>
            <TableCell>
              <a class="hover:underline" href="/manifests/{encodeURIComponent(m.name)}">
                <code class="text-xs">{m.name}</code>
              </a>
            </TableCell>
            <TableCell class="text-right tabular-nums text-xs">{fmtBytes(m.size)}</TableCell>
            <TableCell class="text-muted-foreground text-xs">{relativeFromUnix(m.mtime_unix)}</TableCell>
            <TableCell>
              <a
                href="/manifests/{encodeURIComponent(m.name)}"
                class="text-primary text-xs hover:underline">Open</a
              >
            </TableCell>
          </TableRow>
        {/each}
      </TableBody>
    </Table>
  </Card>
{/if}
