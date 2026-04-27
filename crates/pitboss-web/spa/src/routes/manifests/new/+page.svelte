<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import {
    getSchema,
    saveManifest,
    validateManifest,
    type SchemaSection,
    type ValidateResult,
    ApiError
  } from '$lib/api';
  import {
    AVAILABLE_TOOLS,
    EFFORTS,
    MODELS,
    canAdvanceTo,
    emptyState,
    isValidFilename,
    stepValid,
    type FlatTask,
    type WizardState,
    type WizardStep
  } from '$lib/wizard/state';
  import { composeToml } from '$lib/wizard/compose';
  import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card';
  import { Button } from '$lib/components/ui/button';
  import { Input } from '$lib/components/ui/input';
  import { Textarea } from '$lib/components/ui/textarea';
  import { Label } from '$lib/components/ui/label';
  import { Select } from '$lib/components/ui/select';
  import { Switch } from '$lib/components/ui/switch';
  import { Badge } from '$lib/components/ui/badge';
  import HelpTip from '$lib/components/help-tip.svelte';
  import {
    AlertTriangle,
    ChevronLeft,
    ChevronRight,
    Plus,
    Trash2,
    CheckCircle2
  } from 'lucide-svelte';

  // ---- state ------------------------------------------------------------
  let wizard = $state<WizardState>(emptyState());
  let step = $state<WizardStep>(1);
  let schema = $state<SchemaSection[]>([]);
  let creating = $state(false);
  let validation = $state<ValidateResult | null>(null);
  let error = $state<string | null>(null);

  // ---- mount: fetch schema for tooltips --------------------------------
  onMount(async () => {
    try {
      schema = await getSchema();
    } catch (e) {
      // Tooltips degrade gracefully — wizard still functions without help text.
      console.warn('schema fetch failed', e);
    }
  });

  // Derived: composed TOML for the Review step.
  const previewToml = $derived(composeToml(wizard));
  // Derived: can the user move forward from the current step?
  const nextEnabled = $derived(stepValid(wizard, step));

  function next() {
    if (!nextEnabled) return;
    if (step === 5) return;
    step = (step + 1) as WizardStep;
    validation = null;
  }
  function prev() {
    if (step === 1) return;
    step = (step - 1) as WizardStep;
    validation = null;
  }
  function jumpTo(target: WizardStep) {
    if (target <= step) {
      step = target;
      return;
    }
    if (canAdvanceTo(wizard, target)) step = target;
  }

  function addTask() {
    wizard.tasks = [
      ...wizard.tasks,
      { id: `task-${wizard.tasks.length + 1}`, directory: '', prompt: '' }
    ];
  }
  function removeTask(idx: number) {
    wizard.tasks = wizard.tasks.filter((_, i) => i !== idx);
  }
  function patchTask(idx: number, patch: Partial<FlatTask>) {
    wizard.tasks = wizard.tasks.map((t, i) => (i === idx ? { ...t, ...patch } : t));
  }

  function toggleTool(t: string) {
    wizard.tools = wizard.tools.includes(t)
      ? wizard.tools.filter((x) => x !== t)
      : [...wizard.tools, t];
  }

  async function runValidate() {
    try {
      validation = await validateManifest(previewToml);
    } catch (e) {
      error = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
    }
  }

  async function create() {
    error = null;
    if (!isValidFilename(wizard.filename)) {
      error = 'Invalid filename — use letters, digits, dot, dash, underscore.';
      return;
    }
    creating = true;
    try {
      const v = await validateManifest(previewToml);
      if (!v.ok) {
        validation = v;
        error =
          'Composed manifest failed validation. Edit the wizard fields or open the editor to fix.';
        return;
      }
      await saveManifest(wizard.filename, previewToml);
      goto(`/manifests/${encodeURIComponent(wizard.filename)}`);
    } catch (e) {
      error = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
    } finally {
      creating = false;
    }
  }
</script>

<svelte:head>
  <title>New manifest — Pitboss</title>
</svelte:head>

<div class="mb-6">
  <h1 class="text-2xl font-semibold tracking-tight">New manifest</h1>
  <p class="text-muted-foreground text-sm">
    Five quick steps. The wizard composes a TOML draft; you can edit anything afterwards in the
    full editor.
  </p>
</div>

<!-- Step indicator -->
<div class="mb-6 flex items-center gap-2 text-sm">
  {#each [1, 2, 3, 4, 5] as s, i (s)}
    <button
      type="button"
      class="flex items-center gap-2 {s === step ? 'text-foreground' : 'text-muted-foreground'} hover:text-foreground"
      onclick={() => jumpTo(s as WizardStep)}
      disabled={!canAdvanceTo(wizard, s as WizardStep) && s !== step}
    >
      <span
        class="flex size-6 items-center justify-center rounded-full border text-xs tabular-nums {s === step
          ? 'border-primary bg-primary text-primary-foreground'
          : stepValid(wizard, s as WizardStep)
            ? 'border-emerald-500/50 text-emerald-500'
            : 'border-border'}"
      >
        {#if stepValid(wizard, s as WizardStep) && s !== step}
          <CheckCircle2 class="size-3" />
        {:else}
          {s}
        {/if}
      </span>
      <span class="hidden sm:inline">{stepLabel(s as WizardStep)}</span>
    </button>
    {#if i < 4}
      <span class="text-muted-foreground/40">·</span>
    {/if}
  {/each}
</div>

{#if error}
  <Card class="border-destructive/50 mb-4">
    <CardContent class="flex items-start gap-3 pt-6">
      <AlertTriangle class="text-destructive mt-0.5 size-5 shrink-0" />
      <p class="text-destructive text-sm">{error}</p>
    </CardContent>
  </Card>
{/if}

<!-- ===== Step 1: Basics ===== -->
{#if step === 1}
  <Card>
    <CardHeader>
      <CardTitle>Basics</CardTitle>
    </CardHeader>
    <CardContent class="space-y-4">
      <div class="space-y-1.5">
        <div class="flex items-center gap-1.5">
          <Label for="filename">Filename</Label>
          <HelpTip help="Saved as <name>.toml in the manifests workspace. Letters, digits, dot, dash, underscore. Max 64 chars." />
        </div>
        <Input
          id="filename"
          placeholder="nightly-sync"
          bind:value={wizard.filename}
        />
        {#if wizard.filename && !isValidFilename(wizard.filename)}
          <p class="text-destructive text-xs">
            Use only letters, digits, dot, dash, underscore (max 64 chars).
          </p>
        {/if}
      </div>

      <div class="space-y-1.5">
        <div class="flex items-center gap-1.5">
          <Label for="run_name">Run name</Label>
          <HelpTip {schema} section="[run]" field="name" />
        </div>
        <Input
          id="run_name"
          placeholder="nightly-sync"
          bind:value={wizard.run_name}
        />
        <p class="text-muted-foreground text-xs">
          Human-readable label used in the operational console to group related runs.
        </p>
      </div>
    </CardContent>
  </Card>
{/if}

<!-- ===== Step 2: Mode ===== -->
{#if step === 2}
  <Card>
    <CardHeader>
      <CardTitle>Mode</CardTitle>
    </CardHeader>
    <CardContent class="space-y-3">
      <button
        type="button"
        onclick={() => (wizard.mode = 'flat')}
        class="block w-full rounded-lg border p-4 text-left transition {wizard.mode === 'flat'
          ? 'border-primary bg-primary/5'
          : 'border-border hover:border-foreground/30'}"
      >
        <div class="flex items-start gap-3">
          <div class="mt-0.5 size-4 shrink-0 rounded-full border-2 {wizard.mode === 'flat' ? 'border-primary bg-primary' : 'border-border'}"></div>
          <div>
            <div class="font-medium">Flat — list of <code class="text-xs">[[task]]</code></div>
            <p class="text-muted-foreground mt-1 text-sm">
              Independent tasks that run in parallel up to the configured cap. No coordination
              between them. Pick this for one-shot batches like "audit each of these
              repos."
            </p>
          </div>
        </div>
      </button>

      <button
        type="button"
        onclick={() => (wizard.mode = 'hierarchical')}
        class="block w-full rounded-lg border p-4 text-left transition {wizard.mode === 'hierarchical'
          ? 'border-primary bg-primary/5'
          : 'border-border hover:border-foreground/30'}"
      >
        <div class="flex items-start gap-3">
          <div class="mt-0.5 size-4 shrink-0 rounded-full border-2 {wizard.mode === 'hierarchical' ? 'border-primary bg-primary' : 'border-border'}"></div>
          <div>
            <div class="font-medium">Hierarchical — one <code class="text-xs">[lead]</code></div>
            <p class="text-muted-foreground mt-1 text-sm">
              A coordinator (the lead) plans the work, then spawns workers as needed. Pick this
              for multi-step jobs where one Claude needs to dispatch follow-ups based on
              earlier results.
            </p>
          </div>
        </div>
      </button>
    </CardContent>
  </Card>
{/if}

<!-- ===== Step 3: Defaults ===== -->
{#if step === 3}
  <Card>
    <CardHeader>
      <CardTitle>Defaults</CardTitle>
      <p class="text-muted-foreground text-sm">
        Applied to every task / lead / worker that doesn't override them.
      </p>
    </CardHeader>
    <CardContent class="space-y-4">
      <div class="grid grid-cols-1 gap-4 sm:grid-cols-2">
        <div class="space-y-1.5">
          <div class="flex items-center gap-1.5">
            <Label for="model">Model</Label>
            <HelpTip {schema} section="[defaults]" field="model" />
          </div>
          <Select id="model" bind:value={wizard.model}>
            {#each MODELS as m (m)}
              <option value={m}>{m}</option>
            {/each}
          </Select>
        </div>

        <div class="space-y-1.5">
          <div class="flex items-center gap-1.5">
            <Label for="effort">Effort</Label>
            <HelpTip {schema} section="[defaults]" field="effort" />
          </div>
          <Select id="effort" bind:value={wizard.effort}>
            {#each EFFORTS as e (e)}
              <option value={e}>{e}</option>
            {/each}
          </Select>
        </div>
      </div>

      <div class="space-y-1.5">
        <div class="flex items-center gap-1.5">
          <Label>Tools</Label>
          <HelpTip {schema} section="[defaults]" field="tools" />
        </div>
        <div class="flex flex-wrap gap-2">
          {#each AVAILABLE_TOOLS as t (t)}
            <button
              type="button"
              onclick={() => toggleTool(t)}
              class="rounded-md border px-3 py-1 text-sm transition {wizard.tools.includes(t)
                ? 'border-primary bg-primary/10 text-foreground'
                : 'border-border text-muted-foreground hover:text-foreground'}"
            >
              {t}
            </button>
          {/each}
        </div>
      </div>

      <div class="grid grid-cols-1 gap-4 sm:grid-cols-2">
        <div class="space-y-1.5">
          <div class="flex items-center gap-1.5">
            <Label for="timeout">Timeout (seconds)</Label>
            <HelpTip {schema} section="[defaults]" field="timeout_secs" />
          </div>
          <Input
            id="timeout"
            type="number"
            min="0"
            placeholder="1800"
            value={wizard.timeout_secs ?? ''}
            oninput={(e) => {
              const v = (e.target as HTMLInputElement).valueAsNumber;
              wizard.timeout_secs = Number.isFinite(v) && v > 0 ? v : null;
            }}
          />
        </div>

        <div class="flex items-center justify-between gap-3 rounded-md border p-3">
          <div>
            <div class="flex items-center gap-1.5">
              <Label>Use git worktree</Label>
              <HelpTip {schema} section="[defaults]" field="use_worktree" />
            </div>
            <p class="text-muted-foreground mt-1 text-xs">
              Isolate each worker in its own branch.
            </p>
          </div>
          <Switch bind:checked={wizard.use_worktree} />
        </div>
      </div>
    </CardContent>
  </Card>
{/if}

<!-- ===== Step 4: Work (flat or hierarchical) ===== -->
{#if step === 4 && wizard.mode === 'flat'}
  <Card>
    <CardHeader>
      <CardTitle>Tasks</CardTitle>
      <p class="text-muted-foreground text-sm">
        One <code class="text-xs">[[task]]</code> per parallel job. Each gets its own working
        directory + prompt.
      </p>
    </CardHeader>
    <CardContent class="space-y-4">
      {#each wizard.tasks as t, i (i)}
        <div class="rounded-md border p-4">
          <div class="mb-3 flex items-center justify-between">
            <Badge variant="outline">Task #{i + 1}</Badge>
            {#if wizard.tasks.length > 1}
              <Button size="sm" variant="ghost" onclick={() => removeTask(i)}>
                <Trash2 class="size-4" />
              </Button>
            {/if}
          </div>
          <div class="space-y-3">
            <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <div class="space-y-1.5">
                <div class="flex items-center gap-1.5">
                  <Label>ID</Label>
                  <HelpTip {schema} section="[[task]]" field="id" />
                </div>
                <Input
                  value={t.id}
                  oninput={(e) => patchTask(i, { id: (e.target as HTMLInputElement).value })}
                />
              </div>
              <div class="space-y-1.5">
                <div class="flex items-center gap-1.5">
                  <Label>Directory</Label>
                  <HelpTip {schema} section="[[task]]" field="directory" />
                </div>
                <Input
                  placeholder="/path/to/repo"
                  value={t.directory}
                  oninput={(e) => patchTask(i, { directory: (e.target as HTMLInputElement).value })}
                />
              </div>
            </div>
            <div class="space-y-1.5">
              <div class="flex items-center gap-1.5">
                <Label>Prompt</Label>
                <HelpTip {schema} section="[[task]]" field="prompt" />
              </div>
              <Textarea
                rows={4}
                placeholder="Describe what this worker should do…"
                value={t.prompt}
                oninput={(e) => patchTask(i, { prompt: (e.target as HTMLTextAreaElement).value })}
              />
            </div>
          </div>
        </div>
      {/each}
      <Button variant="outline" size="sm" onclick={addTask}>
        <Plus class="mr-2 size-4" /> Add task
      </Button>
    </CardContent>
  </Card>
{/if}

{#if step === 4 && wizard.mode === 'hierarchical'}
  <Card>
    <CardHeader>
      <CardTitle>Lead</CardTitle>
      <p class="text-muted-foreground text-sm">
        The coordinator that plans the work and spawns workers as needed.
      </p>
    </CardHeader>
    <CardContent class="space-y-4">
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <div class="space-y-1.5">
          <div class="flex items-center gap-1.5">
            <Label for="lead-id">ID</Label>
            <HelpTip {schema} section="[lead]" field="id" />
          </div>
          <Input id="lead-id" bind:value={wizard.lead.id} />
        </div>
        <div class="space-y-1.5">
          <div class="flex items-center gap-1.5">
            <Label for="lead-dir">Directory</Label>
            <HelpTip {schema} section="[lead]" field="directory" />
          </div>
          <Input
            id="lead-dir"
            placeholder="/path/to/your/project"
            bind:value={wizard.lead.directory}
          />
        </div>
      </div>
      <div class="space-y-1.5">
        <div class="flex items-center gap-1.5">
          <Label for="lead-prompt">Prompt</Label>
          <HelpTip {schema} section="[lead]" field="prompt" />
        </div>
        <Textarea
          id="lead-prompt"
          rows={5}
          placeholder="Plan the work, then delegate to workers…"
          bind:value={wizard.lead.prompt}
        />
      </div>
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-3">
        <div class="space-y-1.5">
          <div class="flex items-center gap-1.5">
            <Label for="max-workers">Max workers</Label>
            <HelpTip {schema} section="[lead]" field="max_workers" />
          </div>
          <Input
            id="max-workers"
            type="number"
            min="1"
            max="16"
            bind:value={wizard.lead.max_workers}
          />
        </div>
        <div class="space-y-1.5">
          <div class="flex items-center gap-1.5">
            <Label for="budget">Budget (USD)</Label>
            <HelpTip {schema} section="[lead]" field="budget_usd" />
          </div>
          <Input
            id="budget"
            type="number"
            step="0.01"
            min="0"
            bind:value={wizard.lead.budget_usd}
          />
        </div>
        <div class="space-y-1.5">
          <div class="flex items-center gap-1.5">
            <Label for="lead-timeout">Lead timeout (s)</Label>
            <HelpTip {schema} section="[lead]" field="lead_timeout_secs" />
          </div>
          <Input
            id="lead-timeout"
            type="number"
            min="60"
            bind:value={wizard.lead.lead_timeout_secs}
          />
        </div>
      </div>
    </CardContent>
  </Card>
{/if}

<!-- ===== Step 5: Review ===== -->
{#if step === 5}
  <Card>
    <CardHeader>
      <CardTitle>Review</CardTitle>
      <p class="text-muted-foreground text-sm">
        Final TOML preview. Click Create to save and open the editor where you can edit
        anything.
      </p>
    </CardHeader>
    <CardContent class="space-y-4">
      <div class="space-y-2">
        <div class="flex items-center justify-between gap-2">
          <Label>Composed TOML</Label>
          <Button variant="outline" size="sm" onclick={runValidate}>Validate</Button>
        </div>
        <pre
          class="bg-muted/40 max-h-96 overflow-auto rounded-md border p-3 text-xs"><code
            >{previewToml}</code
          ></pre>
      </div>

      {#if validation}
        {#if validation.ok}
          <div class="border-emerald-500/40 bg-emerald-500/5 flex items-start gap-2 rounded-md border p-3 text-sm">
            <CheckCircle2 class="text-emerald-500 mt-0.5 size-4 shrink-0" />
            <span class="text-emerald-500">Validates cleanly.</span>
          </div>
        {:else}
          <div class="border-destructive/40 bg-destructive/5 rounded-md border p-3 text-sm">
            <p class="text-destructive font-medium mb-1">
              Validation found {validation.errors.length} issue{validation.errors.length === 1
                ? ''
                : 's'}:
            </p>
            <ul class="text-destructive/90 space-y-0.5 text-xs">
              {#each validation.errors as e, i (i)}
                <li>· {e}</li>
              {/each}
            </ul>
          </div>
        {/if}
      {/if}
    </CardContent>
  </Card>
{/if}

<!-- ===== Nav ===== -->
<div class="mt-6 flex items-center justify-between">
  <Button variant="outline" onclick={prev} disabled={step === 1}>
    <ChevronLeft class="mr-1 size-4" /> Back
  </Button>
  {#if step < 5}
    <Button onclick={next} disabled={!nextEnabled}>
      Next <ChevronRight class="ml-1 size-4" />
    </Button>
  {:else}
    <Button onclick={create} disabled={creating}>
      {creating ? 'Creating…' : 'Create manifest'}
    </Button>
  {/if}
</div>

<script module lang="ts">
  function stepLabel(s: 1 | 2 | 3 | 4 | 5): string {
    return ['Basics', 'Mode', 'Defaults', 'Work', 'Review'][s - 1];
  }
</script>
