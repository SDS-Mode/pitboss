<script lang="ts">
  import { Tooltip as TooltipPrimitive } from 'bits-ui';

  let { ...rest }: TooltipPrimitive.TriggerProps = $props();
</script>

<TooltipPrimitive.Trigger {...rest} />
