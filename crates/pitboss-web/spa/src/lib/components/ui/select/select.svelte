<script lang="ts">
  import type { HTMLSelectAttributes } from 'svelte/elements';
  import type { Snippet } from 'svelte';
  import { cn } from '$lib/utils';

  // Lightweight select: native <select> styled to match the Input
  // primitive. Sufficient for fixed enum lists (model, effort) without
  // pulling in the full bits-ui combobox machinery. Accepts <option>
  // children via a snippet so callers can render whatever option set
  // they need.

  let {
    class: className,
    value = $bindable(),
    children,
    ...rest
  }: HTMLSelectAttributes & { class?: string; children?: Snippet } = $props();
</script>

<select
  class={cn(
    'border-input bg-background ring-offset-background focus-visible:ring-ring flex h-9 w-full rounded-md border px-3 py-1 text-sm shadow-sm transition-colors focus-visible:ring-1 focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50',
    className
  )}
  bind:value
  {...rest}
>
  {@render children?.()}
</select>
