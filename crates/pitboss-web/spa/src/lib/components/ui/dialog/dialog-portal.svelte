<script lang="ts">
  import { Dialog as DialogPrimitive } from 'bits-ui';

  let { ...rest }: DialogPrimitive.PortalProps = $props();
</script>

<DialogPrimitive.Portal {...rest} />
