<script lang="ts">
  import { Tabs as TabsPrimitive } from 'bits-ui';
  import { cn } from '$lib/utils';

  let { class: className, ...rest }: TabsPrimitive.RootProps & { class?: string } = $props();
</script>

<TabsPrimitive.Root class={cn('', className)} {...rest} />
