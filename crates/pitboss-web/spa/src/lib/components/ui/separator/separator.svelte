<script lang="ts">
  import { Separator as SeparatorPrimitive } from 'bits-ui';
  import { cn } from '$lib/utils';

  type Props = SeparatorPrimitive.RootProps & { class?: string };

  let {
    class: className,
    orientation = 'horizontal',
    decorative = true,
    ...rest
  }: Props = $props();
</script>

<SeparatorPrimitive.Root
  class={cn(
    'bg-border shrink-0',
    orientation === 'horizontal' ? 'h-[1px] w-full' : 'h-full w-[1px]',
    className
  )}
  {orientation}
  {decorative}
  {...rest}
/>
