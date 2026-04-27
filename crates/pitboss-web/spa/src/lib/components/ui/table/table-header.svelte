<script lang="ts">
  import type { HTMLAttributes } from 'svelte/elements';
  import { cn } from '$lib/utils';

  let {
    class: className,
    children,
    ...rest
  }: HTMLAttributes<HTMLTableSectionElement> & { children?: import('svelte').Snippet } = $props();
</script>

<thead class={cn('[&_tr]:border-b', className)} {...rest}>
  {@render children?.()}
</thead>
