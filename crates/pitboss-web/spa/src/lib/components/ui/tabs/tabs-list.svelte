<script lang="ts">
  import { Tabs as TabsPrimitive } from 'bits-ui';
  import { cn } from '$lib/utils';

  let { class: className, ...rest }: TabsPrimitive.ListProps & { class?: string } = $props();
</script>

<TabsPrimitive.List
  class={cn(
    'bg-muted text-muted-foreground inline-flex h-9 items-center justify-center rounded-lg p-1',
    className
  )}
  {...rest}
/>
