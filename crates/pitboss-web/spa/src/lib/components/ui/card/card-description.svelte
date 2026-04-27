<script lang="ts">
  import type { HTMLAttributes } from 'svelte/elements';
  import { cn } from '$lib/utils';

  let {
    class: className,
    children,
    ...rest
  }: HTMLAttributes<HTMLParagraphElement> & { children?: import('svelte').Snippet } = $props();
</script>

<p class={cn('text-muted-foreground text-sm', className)} {...rest}>
  {@render children?.()}
</p>
