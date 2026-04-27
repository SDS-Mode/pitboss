<script lang="ts">
  import type { HTMLAttributes } from 'svelte/elements';
  import { cn } from '$lib/utils';

  let {
    class: className,
    children,
    ...rest
  }: HTMLAttributes<HTMLTableSectionElement> & { children?: import('svelte').Snippet } = $props();
</script>

<tfoot class={cn('bg-muted/50 border-t font-medium [&>tr]:last:border-b-0', className)} {...rest}>
  {@render children?.()}
</tfoot>
