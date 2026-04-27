<script lang="ts">
  import type { HTMLThAttributes } from 'svelte/elements';
  import { cn } from '$lib/utils';

  let {
    class: className,
    children,
    ...rest
  }: HTMLThAttributes & { children?: import('svelte').Snippet } = $props();
</script>

<th
  class={cn(
    'text-muted-foreground h-10 px-2 text-left align-middle font-medium [&:has([role=checkbox])]:pr-0 [&>[role=checkbox]]:translate-y-[2px]',
    className
  )}
  {...rest}
>
  {@render children?.()}
</th>
