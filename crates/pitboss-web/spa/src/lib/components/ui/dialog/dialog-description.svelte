<script lang="ts">
  import { Dialog as DialogPrimitive } from 'bits-ui';
  import { cn } from '$lib/utils';

  let { class: className, ...rest }: DialogPrimitive.DescriptionProps & { class?: string } =
    $props();
</script>

<DialogPrimitive.Description class={cn('text-muted-foreground text-sm', className)} {...rest} />
