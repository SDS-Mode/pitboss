<script lang="ts">
  import { goto } from '$app/navigation';
  import {
    listManifests,
    saveManifest,
    validateManifest,
    exportManifestUrl,
    ApiError,
    type ManifestEntry
  } from '$lib/api';
  import { isValidFilename } from '$lib/wizard/state';
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
  import {
    AlertTriangle,
    Download,
    FileText,
    RefreshCw,
    Upload,
    Sparkles
  } from 'lucide-svelte';

  const MAX_BYTES = 256 * 1024;

  let manifests = $state<ManifestEntry[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let importErrors = $state<string[] | null>(null);

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

  function triggerImport() {
    importErrors = null;
    const input = document.createElement('input');
    input.type = 'file';
    input.accept = '.toml,text/plain,application/toml';
    input.onchange = async () => {
      const file = input.files?.[0];
      if (!file) return;
      if (file.size > MAX_BYTES) {
        importErrors = [`File too large: ${file.size} bytes (max ${MAX_BYTES}).`];
        return;
      }
      const stem = file.name.replace(/\.toml$/i, '');
      if (!isValidFilename(stem)) {
        importErrors = [
          `Filename "${file.name}" is not allowed. Use letters, digits, dot, dash, underscore (max 64 chars).`
        ];
        return;
      }
      const contents = await file.text();
      try {
        const v = await validateManifest(contents);
        if (!v.ok) {
          importErrors = [`Validation failed for ${file.name}:`, ...v.errors];
          return;
        }
        await saveManifest(stem, contents);
        await load();
        await goto(`/manifests/${encodeURIComponent(stem)}`);
      } catch (e) {
        importErrors = [
          e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e)
        ];
      }
    };
    input.click();
  }

  function exportManifest(name: string) {
    // Browser handles the download via Content-Disposition. The auth
    // middleware accepts ?token= as a header fallback (same as SSE).
    window.location.href = exportManifestUrl(name);
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
    <Button variant="outline" size="sm" onclick={triggerImport}>
      <Upload class="mr-2 size-4" /> Import
    </Button>
    <Button href="/manifests/new" size="sm">
      <Sparkles class="mr-2 size-4" /> Guided
    </Button>
  </div>
</div>

{#if importErrors}
  <Card class="border-destructive/50 mb-4">
    <CardContent class="flex items-start gap-3 pt-6">
      <AlertTriangle class="text-destructive mt-0.5 size-5 shrink-0" />
      <div class="space-y-1">
        {#each importErrors as e, i (i)}
          <p class={i === 0 ? 'text-destructive font-medium text-sm' : 'text-muted-foreground text-xs font-mono'}>
            {e}
          </p>
        {/each}
      </div>
    </CardContent>
  </Card>
{/if}

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
          Build one with the guided wizard or import a TOML file from disk.
        </p>
      </div>
      <div class="flex gap-2">
        <Button href="/manifests/new" size="sm">
          <Sparkles class="mr-2 size-4" /> Guided
        </Button>
        <Button variant="outline" size="sm" onclick={triggerImport}>
          <Upload class="mr-2 size-4" /> Import
        </Button>
      </div>
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
          <TableHead class="w-[16ch] text-right"></TableHead>
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
            <TableCell class="text-muted-foreground text-xs">
              {relativeFromUnix(m.mtime_unix)}
            </TableCell>
            <TableCell class="text-right">
              <a
                href="/manifests/{encodeURIComponent(m.name)}"
                class="text-primary text-xs hover:underline mr-3"
              >
                Open
              </a>
              <button
                type="button"
                class="text-muted-foreground hover:text-foreground inline-flex items-center text-xs"
                onclick={() => exportManifest(m.name)}
                aria-label="Export {m.name}"
              >
                <Download class="mr-1 size-3" /> Export
              </button>
            </TableCell>
          </TableRow>
        {/each}
      </TableBody>
    </Table>
  </Card>
{/if}
