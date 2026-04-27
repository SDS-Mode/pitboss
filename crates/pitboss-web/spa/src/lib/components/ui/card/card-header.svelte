<script lang="ts">
  import type { HTMLAttributes } from 'svelte/elements';
  import { cn } from '$lib/utils';

  let {
    class: className,
    children,
    ...rest
  }: HTMLAttributes<HTMLDivElement> & { children?: import('svelte').Snippet } = $props();
</script>

<div class={cn('flex flex-col space-y-1.5 p-6', className)} {...rest}>
  {@render children?.()}
</div>
