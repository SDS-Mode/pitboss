<script lang="ts">
  import { ScrollArea as ScrollAreaPrimitive } from 'bits-ui';
  import ScrollBar from './scroll-bar.svelte';
  import { cn } from '$lib/utils';

  let {
    class: className,
    orientation = 'vertical',
    children,
    ...rest
  }: ScrollAreaPrimitive.RootProps & {
    class?: string;
    orientation?: 'vertical' | 'horizontal' | 'both';
    children?: import('svelte').Snippet;
  } = $props();
</script>

<ScrollAreaPrimitive.Root class={cn('relative overflow-hidden', className)} {...rest}>
  <ScrollAreaPrimitive.Viewport class="h-full w-full rounded-[inherit]">
    {@render children?.()}
  </ScrollAreaPrimitive.Viewport>
  {#if orientation === 'vertical' || orientation === 'both'}
    <ScrollBar orientation="vertical" />
  {/if}
  {#if orientation === 'horizontal' || orientation === 'both'}
    <ScrollBar orientation="horizontal" />
  {/if}
  <ScrollAreaPrimitive.Corner />
</ScrollAreaPrimitive.Root>
