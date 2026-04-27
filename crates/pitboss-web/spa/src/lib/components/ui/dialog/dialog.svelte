<script lang="ts">
  import { Dialog as DialogPrimitive } from 'bits-ui';

  let { ...rest }: DialogPrimitive.RootProps = $props();
</script>

<DialogPrimitive.Root {...rest} />
