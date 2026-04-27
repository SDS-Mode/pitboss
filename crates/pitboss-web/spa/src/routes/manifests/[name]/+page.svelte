<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/state';
  import {
    readManifest,
    saveManifest,
    validateManifest,
    dispatchManifest,
    ApiError,
    type ValidateResult
  } from '$lib/api';
  import {
    Card,
    CardContent,
    CardDescription,
    CardHeader,
    CardTitle
  } from '$lib/components/ui/card';
  import { Button } from '$lib/components/ui/button';
  import { Badge } from '$lib/components/ui/badge';
  import {
    ArrowLeft,
    ChevronRight,
    Save,
    PlayCircle,
    CheckCircle2,
    AlertTriangle,
    Loader2
  } from 'lucide-svelte';

  const name = $derived(decodeURIComponent(page.params.name ?? ''));

  let contents = $state('');
  let original = $state('');
  let loadError = $state<string | null>(null);
  let loading = $state(false);

  let validation = $state<ValidateResult | null>(null);
  let validating = $state(false);
  let validateTimer: ReturnType<typeof setTimeout> | null = null;

  let saving = $state(false);
  let dispatching = $state(false);
  let lastSavedAt = $state<number | null>(null);
  let dispatchError = $state<string | null>(null);

  const dirty = $derived(contents !== original);

  async function load() {
    loading = true;
    loadError = null;
    try {
      const text = await readManifest(name);
      contents = text;
      original = text;
      validation = null;
    } catch (e) {
      loadError = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    if (name) load();
  });

  async function runValidate() {
    validating = true;
    try {
      validation = await validateManifest(contents);
    } catch (e) {
      validation = {
        ok: false,
        errors: [e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e)]
      };
    } finally {
      validating = false;
    }
  }

  // Debounced validate-on-keystroke. Skips empty buffers — pitboss's
  // parser would reject them and the noise distracts mid-edit.
  $effect(() => {
    if (!contents.trim()) {
      validation = null;
      return;
    }
    if (validateTimer) clearTimeout(validateTimer);
    const snapshot = contents;
    validateTimer = setTimeout(() => {
      if (snapshot === contents) runValidate();
    }, 600);
  });

  async function save() {
    saving = true;
    dispatchError = null;
    try {
      await saveManifest(name, contents);
      original = contents;
      lastSavedAt = Date.now();
    } catch (e) {
      dispatchError = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
    } finally {
      saving = false;
    }
  }

  async function dispatch() {
    if (dirty) {
      if (
        !confirm(
          'There are unsaved changes. Save before dispatching? (Cancel to dispatch the on-disk version.)'
        )
      ) {
        // Operator chose to dispatch the on-disk version.
      } else {
        await save();
        if (dispatchError) return;
      }
    }
    dispatching = true;
    dispatchError = null;
    try {
      const res = await dispatchManifest(name);
      const runId = res.descriptor?.run_id;
      if (typeof runId === 'string' && runId.length > 0) {
        await goto(`/runs/${encodeURIComponent(runId)}`);
      } else {
        dispatchError = 'Dispatcher succeeded but did not return a run_id';
      }
    } catch (e) {
      dispatchError = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
    } finally {
      dispatching = false;
    }
  }
</script>

<svelte:head>
  <title>{name} — Pitboss</title>
</svelte:head>

<div class="mb-4 flex items-center gap-3 text-sm">
  <Button variant="ghost" size="sm" href="/manifests">
    <ArrowLeft class="mr-1.5 size-4" /> Manifests
  </Button>
  <ChevronRight class="text-muted-foreground size-4" />
  <code class="text-xs">{name}</code>
  {#if dirty}<Badge variant="outline" class="text-xs">unsaved</Badge>{/if}
</div>

{#if loadError}
  <Card class="border-destructive/50">
    <CardContent class="flex items-start gap-3 pt-6">
      <AlertTriangle class="text-destructive mt-0.5 size-5 shrink-0" />
      <div>
        <p class="text-destructive font-medium">Failed to load</p>
        <p class="text-muted-foreground mt-1 text-sm">{loadError}</p>
      </div>
    </CardContent>
  </Card>
{:else if loading && !contents}
  <Card>
    <CardContent class="text-muted-foreground py-12 text-center text-sm">Loading…</CardContent>
  </Card>
{:else}
  <div class="grid gap-4 lg:grid-cols-[1fr_22rem]">
    <Card>
      <CardHeader class="pb-3">
        <div class="flex items-center justify-between gap-3">
          <div>
            <CardTitle class="text-base">Editor</CardTitle>
            <CardDescription class="text-xs">
              TOML source. Validates as you type.
            </CardDescription>
          </div>
          <div class="flex items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onclick={save}
              disabled={saving || !dirty}
            >
              <Save class="mr-1.5 size-4" />
              {saving ? 'Saving…' : 'Save'}
            </Button>
            <Button size="sm" onclick={dispatch} disabled={dispatching}>
              <PlayCircle class="mr-1.5 size-4" />
              {dispatching ? 'Dispatching…' : 'Dispatch'}
            </Button>
          </div>
        </div>
      </CardHeader>
      <CardContent class="pt-0">
        <textarea
          class="border-input bg-background focus-visible:ring-ring h-[60vh] w-full rounded-md border px-3 py-2 font-mono text-xs leading-snug shadow-sm focus-visible:ring-1 focus-visible:outline-none"
          bind:value={contents}
          spellcheck="false"
        ></textarea>
        {#if lastSavedAt}
          <p class="text-muted-foreground mt-2 text-xs">
            Saved {new Date(lastSavedAt).toLocaleTimeString()}
          </p>
        {/if}
        {#if dispatchError}
          <div
            class="border-destructive/50 bg-destructive/5 text-destructive mt-2 rounded border px-3 py-2 text-xs"
          >
            {dispatchError}
          </div>
        {/if}
      </CardContent>
    </Card>

    <Card>
      <CardHeader class="pb-2">
        <CardTitle class="flex items-center gap-2 text-base">
          Validation
          {#if validating}
            <Loader2 class="text-muted-foreground size-3.5 animate-spin" />
          {:else if validation?.ok}
            <CheckCircle2 class="size-4 text-emerald-600" />
          {:else if validation && !validation.ok}
            <AlertTriangle class="text-destructive size-4" />
          {/if}
        </CardTitle>
        <CardDescription class="text-xs">
          Runs `validate_skip_dir_check` against the editor buffer.
        </CardDescription>
      </CardHeader>
      <CardContent class="pt-0">
        {#if !validation}
          <p class="text-muted-foreground py-2 text-xs">Edit to validate.</p>
        {:else if validation.ok}
          <p class="text-xs text-emerald-700 dark:text-emerald-400">
            Manifest validates cleanly. Ready to dispatch.
          </p>
        {:else}
          <ul class="space-y-1.5">
            {#each validation.errors as err, idx (idx)}
              <li class="bg-destructive/5 text-destructive rounded border-l-2 border-current px-2 py-1.5 text-xs">
                {err}
              </li>
            {/each}
          </ul>
        {/if}
      </CardContent>
    </Card>
  </div>
{/if}
