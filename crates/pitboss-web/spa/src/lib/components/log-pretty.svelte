<script lang="ts">
  import { parseStreamJson, toolInputPreview, type StreamJsonRow } from '$lib/stream-json';
  import { Brain, MessageSquare, Wrench, ArrowLeft, AlertTriangle, Cog, Hash } from 'lucide-svelte';

  let {
    text,
    hideHooks = true,
    hideThinking = false,
    class: className = ''
  }: {
    /** Raw NDJSON blob from `tasks/<task_id>/stdout.log`. */
    text: string;
    /** Suppress `system/hook_*` rows by default — they're scaffold noise
     *  for routine log review. Operators can flip the toggle in the
     *  caller. */
    hideHooks?: boolean;
    /** Suppress `assistant.thinking` rows. They're verbose, and post-run
     *  audits often only want the actions + outputs. */
    hideThinking?: boolean;
    class?: string;
  } = $props();

  const rows = $derived(parseStreamJson(text));
  const visible = $derived(
    rows.filter((r) => {
      if (hideHooks && r.kind === 'hook') return false;
      if (hideThinking && r.kind === 'thinking') return false;
      return true;
    })
  );

  function truncate(s: string, n = 240): string {
    if (s.length <= n) return s;
    return s.slice(0, n - 1) + '…';
  }

  /** Stable key: index + kind + a tiny content-derived fingerprint so
   *  re-renders with the same row don't cascade DOM thrash. */
  function rowKey(r: StreamJsonRow, i: number): string {
    if (r.kind === 'tool_use') return `${i}-tu-${r.tool_use_id}`;
    if (r.kind === 'tool_result') return `${i}-tr-${r.tool_use_id}`;
    return `${i}-${r.kind}`;
  }
</script>

<div class={`space-y-1 font-mono text-[11px] leading-relaxed ${className}`}>
  {#if visible.length === 0}
    <p class="text-muted-foreground italic">No parseable rows.</p>
  {:else}
    {#each visible as r, i (rowKey(r, i))}
      {#if r.kind === 'init'}
        <div class="flex items-start gap-2">
          <Cog class="text-muted-foreground mt-0.5 size-3 shrink-0" />
          <div class="text-muted-foreground">
            <span class="font-semibold uppercase tracking-wide">init</span>
            {#if r.model}
              · <span class="text-foreground">{r.model}</span>
            {/if}
            {#if r.tool_count !== undefined}
              · {r.tool_count} tools
            {/if}
            {#if r.cwd}
              · cwd <code>{r.cwd}</code>
            {/if}
            {#if r.session_id}
              · sess <code>{r.session_id.slice(0, 8)}…</code>
            {/if}
          </div>
        </div>
      {:else if r.kind === 'hook'}
        <div class="flex items-start gap-2">
          <Hash class="text-muted-foreground/60 mt-0.5 size-3 shrink-0" />
          <div class="text-muted-foreground/80">
            <span class="uppercase tracking-wide">hook {r.phase}</span>
            {#if r.name}
              · {r.name}
            {/if}
            {#if r.outcome}
              · {r.outcome}
            {/if}
            {#if r.exit_code !== undefined}
              · exit {r.exit_code}
            {/if}
          </div>
        </div>
      {:else if r.kind === 'system'}
        <div class="flex items-start gap-2">
          <Cog class="text-muted-foreground mt-0.5 size-3 shrink-0" />
          <div class="text-muted-foreground">
            <span class="uppercase tracking-wide">system</span> · {r.subtype}
          </div>
        </div>
      {:else if r.kind === 'thinking'}
        <div class="flex items-start gap-2">
          <Brain class="mt-0.5 size-3 shrink-0 text-purple-500/80" />
          <div class="text-muted-foreground/90 italic whitespace-pre-wrap">
            {truncate(r.text, 600)}
          </div>
        </div>
      {:else if r.kind === 'assistant_text'}
        <div class="flex items-start gap-2">
          <MessageSquare class="mt-0.5 size-3 shrink-0 text-sky-500/80" />
          <div class="whitespace-pre-wrap">{r.text}</div>
        </div>
      {:else if r.kind === 'tool_use'}
        <div class="flex items-start gap-2">
          <Wrench class="mt-0.5 size-3 shrink-0 text-amber-500/80" />
          <div class="min-w-0 flex-1">
            <span class="font-semibold text-amber-700 dark:text-amber-400">
              → {r.tool_name}
            </span>
            {#if toolInputPreview(r.input)}
              <span class="text-foreground/90 ml-1">{toolInputPreview(r.input)}</span>
            {/if}
          </div>
        </div>
      {:else if r.kind === 'tool_result'}
        <div class="flex items-start gap-2">
          <ArrowLeft
            class="mt-0.5 size-3 shrink-0 {r.is_error
              ? 'text-destructive'
              : 'text-emerald-500/80'}"
          />
          <div class="min-w-0 flex-1">
            <span
              class="font-semibold {r.is_error
                ? 'text-destructive'
                : 'text-emerald-700 dark:text-emerald-400'}"
            >
              ← {r.is_error ? 'error' : 'result'}
            </span>
            {#if r.content}
              <span class="text-muted-foreground ml-1 whitespace-pre-wrap">
                {truncate(r.content, 400)}
              </span>
            {/if}
          </div>
        </div>
      {:else if r.kind === 'user_message'}
        <div class="flex items-start gap-2">
          <MessageSquare class="text-muted-foreground mt-0.5 size-3 shrink-0" />
          <div class="text-muted-foreground italic whitespace-pre-wrap">
            {truncate(r.text, 400)}
          </div>
        </div>
      {:else if r.kind === 'result'}
        <div class="flex items-start gap-2">
          {#if r.is_error}
            <AlertTriangle class="text-destructive mt-0.5 size-3 shrink-0" />
            <div class="text-destructive font-semibold">
              [done · error{r.subtype ? ` · ${r.subtype}` : ''}]
              {#if r.text}<span class="font-normal ml-1">{truncate(r.text, 240)}</span>{/if}
            </div>
          {:else}
            <Cog class="text-emerald-500 mt-0.5 size-3 shrink-0" />
            <div class="font-semibold text-emerald-700 dark:text-emerald-400">
              [done · success]
              {#if r.text}<span class="text-foreground font-normal ml-1">{truncate(r.text, 240)}</span>{/if}
            </div>
          {/if}
        </div>
      {:else if r.kind === 'rate_limit'}
        <div class="flex items-start gap-2">
          <AlertTriangle class="text-amber-500 mt-0.5 size-3 shrink-0" />
          <div class="text-amber-700 dark:text-amber-400">
            rate-limited · {r.status}{r.resets_at ? ` · resets at ${r.resets_at}` : ''}
          </div>
        </div>
      {:else}
        <div class="flex items-start gap-2">
          <Hash class="text-muted-foreground/60 mt-0.5 size-3 shrink-0" />
          <div class="text-muted-foreground/80">
            unknown: <code class="text-[10px]">{truncate(JSON.stringify(r.raw), 200)}</code>
          </div>
        </div>
      {/if}
    {/each}
  {/if}
</div>
