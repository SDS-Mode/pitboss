<script lang="ts">
  import { Tooltip as TooltipPrimitive } from 'bits-ui';

  let { ...rest }: TooltipPrimitive.RootProps = $props();
</script>

<TooltipPrimitive.Root {...rest} />
