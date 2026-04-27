<script lang="ts">
  import { Dialog as DialogPrimitive } from 'bits-ui';

  let { ...rest }: DialogPrimitive.CloseProps = $props();
</script>

<DialogPrimitive.Close {...rest} />
