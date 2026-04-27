<script lang="ts">
  import type { HTMLTdAttributes } from 'svelte/elements';
  import { cn } from '$lib/utils';

  let {
    class: className,
    children,
    ...rest
  }: HTMLTdAttributes & { children?: import('svelte').Snippet } = $props();
</script>

<td
  class={cn(
    'p-2 align-middle [&:has([role=checkbox])]:pr-0 [&>[role=checkbox]]:translate-y-[2px]',
    className
  )}
  {...rest}
>
  {@render children?.()}
</td>
