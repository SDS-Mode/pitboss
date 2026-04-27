<script lang="ts">
  import type { HTMLLabelAttributes } from 'svelte/elements';
  import type { Snippet } from 'svelte';
  import { cn } from '$lib/utils';

  let {
    class: className,
    children,
    ...rest
  }: HTMLLabelAttributes & { class?: string; children?: Snippet } = $props();
</script>

<label
  class={cn(
    'text-sm leading-none font-medium peer-disabled:cursor-not-allowed peer-disabled:opacity-70',
    className
  )}
  {...rest}
>
  {@render children?.()}
</label>
