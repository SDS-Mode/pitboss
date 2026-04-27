<script lang="ts">
  import { ScrollArea as ScrollAreaPrimitive } from 'bits-ui';
  import { cn } from '$lib/utils';

  let {
    class: className,
    orientation = 'vertical',
    ...rest
  }: ScrollAreaPrimitive.ScrollbarProps & { class?: string } = $props();
</script>

<ScrollAreaPrimitive.Scrollbar
  {orientation}
  class={cn(
    'flex touch-none select-none transition-colors',
    orientation === 'vertical' && 'h-full w-2.5 border-l border-l-transparent p-[1px]',
    orientation === 'horizontal' && 'h-2.5 flex-col border-t border-t-transparent p-[1px]',
    className
  )}
  {...rest}
>
  <ScrollAreaPrimitive.Thumb class="bg-border relative flex-1 rounded-full" />
</ScrollAreaPrimitive.Scrollbar>
