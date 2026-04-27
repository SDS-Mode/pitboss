<script lang="ts">
  import { Dialog as DialogPrimitive } from 'bits-ui';

  let { ...rest }: DialogPrimitive.TriggerProps = $props();
</script>

<DialogPrimitive.Trigger {...rest} />
