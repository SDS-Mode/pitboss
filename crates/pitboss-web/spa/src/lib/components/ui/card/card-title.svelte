<script lang="ts">
  import type { HTMLAttributes } from 'svelte/elements';
  import { cn } from '$lib/utils';

  let {
    class: className,
    children,
    ...rest
  }: HTMLAttributes<HTMLHeadingElement> & { children?: import('svelte').Snippet } = $props();
</script>

<h3 class={cn('font-semibold leading-none tracking-tight', className)} {...rest}>
  {@render children?.()}
</h3>
