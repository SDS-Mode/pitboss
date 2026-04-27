<script lang="ts">
  import type { HTMLAttributes } from 'svelte/elements';
  import { cn } from '$lib/utils';

  let {
    class: className,
    children,
    ...rest
  }: HTMLAttributes<HTMLElement> & { children?: import('svelte').Snippet } = $props();
</script>

<caption class={cn('text-muted-foreground mt-4 text-sm', className)} {...rest}>
  {@render children?.()}
</caption>
