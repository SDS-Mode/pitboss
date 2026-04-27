<script lang="ts">
  import { postControlOp, ApiError, type PolicyRule } from '$lib/api';
  import {
    Card,
    CardContent,
    CardDescription,
    CardHeader,
    CardTitle
  } from '$lib/components/ui/card';
  import { Button } from '$lib/components/ui/button';
  import { Badge } from '$lib/components/ui/badge';
  import { ChevronDown, ChevronUp, Save, Plus, Trash2, AlertTriangle } from 'lucide-svelte';

  /**
   * Initial rules from the dispatcher's `Hello` event. The component
   * tracks edits locally and only POSTs `update_policy` when the
   * operator hits Save.
   */
  let {
    runId,
    initialRules
  }: {
    runId: string;
    initialRules: PolicyRule[];
  } = $props();

  // Round-trip the rules through JSON for stable text editing. The
  // editor is intentionally raw — schema-driven match builders are
  // Phase 4 work; for now operators paste TOML-equivalent JSON.
  // svelte-ignore state_referenced_locally
  let drafts = $state<string[]>(initialRules.map((r) => JSON.stringify(r, null, 2)));
  let expanded = $state(false);
  let saving = $state(false);
  let error = $state<string | null>(null);
  let lastSavedAt = $state<number | null>(null);

  // Re-seed drafts whenever the dispatcher pushes a new policy_rules
  // (happens on reconnect / take-over). Use a stable key so we don't
  // clobber in-progress edits on every render.
  // svelte-ignore state_referenced_locally
  let lastSeed = $state(JSON.stringify(initialRules));
  $effect(() => {
    const fresh = JSON.stringify(initialRules);
    if (fresh !== lastSeed) {
      lastSeed = fresh;
      drafts = initialRules.map((r) => JSON.stringify(r, null, 2));
    }
  });

  function addRule() {
    drafts = [
      ...drafts,
      JSON.stringify(
        {
          match: { category: 'tool_use' },
          action: { action: 'require_operator' }
        },
        null,
        2
      )
    ];
    expanded = true;
  }

  function removeRule(idx: number) {
    drafts = drafts.filter((_, i) => i !== idx);
  }

  async function save() {
    saving = true;
    error = null;
    let parsed: PolicyRule[];
    try {
      parsed = drafts.map((d, i) => {
        try {
          return JSON.parse(d) as PolicyRule;
        } catch {
          throw new Error(`Rule #${i + 1} is not valid JSON`);
        }
      });
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      saving = false;
      return;
    }

    try {
      await postControlOp(runId, { op: 'update_policy', rules: parsed });
      lastSavedAt = Date.now();
    } catch (e) {
      error = e instanceof ApiError ? `${e.status}: ${e.body || e.message}` : String(e);
    } finally {
      saving = false;
    }
  }
</script>

<Card>
  <CardHeader class="cursor-pointer pb-3" onclick={() => (expanded = !expanded)}>
    <div class="flex items-center justify-between">
      <div>
        <CardTitle class="text-base">
          Approval policy
          <Badge variant="outline" class="ml-2 text-xs">{drafts.length} rule{drafts.length === 1 ? '' : 's'}</Badge>
        </CardTitle>
        <CardDescription class="text-xs">
          Live `[[approval_policy]]` rule set. Changes apply immediately on save.
        </CardDescription>
      </div>
      {#if expanded}
        <ChevronUp class="text-muted-foreground size-4" />
      {:else}
        <ChevronDown class="text-muted-foreground size-4" />
      {/if}
    </div>
  </CardHeader>

  {#if expanded}
    <CardContent class="space-y-3 pt-0">
      {#if drafts.length === 0}
        <p class="text-muted-foreground py-2 text-center text-xs">
          No declarative rules. Every approval falls through to the operator queue.
        </p>
      {/if}

      {#each drafts as _, idx (idx)}
        <div class="space-y-1.5">
          <div class="text-muted-foreground flex items-center justify-between text-xs">
            <span>Rule {idx + 1}</span>
            <Button
              variant="ghost"
              size="sm"
              class="h-6 px-2"
              onclick={() => removeRule(idx)}
              disabled={saving}
            >
              <Trash2 class="size-3" />
            </Button>
          </div>
          <textarea
            class="border-input bg-background focus-visible:ring-ring w-full rounded-md border px-3 py-2 font-mono text-xs leading-snug shadow-sm focus-visible:ring-1 focus-visible:outline-none"
            rows="6"
            bind:value={drafts[idx]}
            disabled={saving}
            spellcheck="false"
          ></textarea>
        </div>
      {/each}

      {#if error}
        <p class="text-destructive flex items-start gap-2 text-xs">
          <AlertTriangle class="mt-0.5 size-3 shrink-0" />
          {error}
        </p>
      {/if}

      <div class="flex items-center justify-between gap-2">
        <Button variant="outline" size="sm" onclick={addRule} disabled={saving}>
          <Plus class="mr-1.5 size-3" /> Add rule
        </Button>
        <div class="flex items-center gap-2">
          {#if lastSavedAt}
            <span class="text-muted-foreground text-xs">
              Saved {new Date(lastSavedAt).toLocaleTimeString()}
            </span>
          {/if}
          <Button size="sm" onclick={save} disabled={saving}>
            <Save class="mr-1.5 size-3" />
            {saving ? 'Saving…' : 'Save policy'}
          </Button>
        </div>
      </div>
    </CardContent>
  {/if}
</Card>
