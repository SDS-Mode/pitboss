<script lang="ts" module>
  import { type VariantProps, tv } from 'tailwind-variants';

  export const badgeVariants = tv({
    base: 'focus:ring-ring inline-flex items-center rounded-md border px-2 py-0.5 text-xs font-semibold transition-colors focus:ring-2 focus:ring-offset-2 focus:outline-none',
    variants: {
      variant: {
        default: 'bg-primary text-primary-foreground hover:bg-primary/80 border-transparent shadow',
        secondary:
          'bg-secondary text-secondary-foreground hover:bg-secondary/80 border-transparent',
        destructive:
          'bg-destructive text-destructive-foreground hover:bg-destructive/80 border-transparent shadow',
        outline: 'text-foreground'
      }
    },
    defaultVariants: { variant: 'default' }
  });

  export type BadgeVariant = VariantProps<typeof badgeVariants>['variant'];
</script>

<script lang="ts">
  import type { HTMLAttributes } from 'svelte/elements';
  import { cn } from '$lib/utils';

  let {
    class: className,
    variant = 'default',
    children,
    ...rest
  }: HTMLAttributes<HTMLSpanElement> & {
    variant?: BadgeVariant;
    children?: import('svelte').Snippet;
  } = $props();
</script>

<span class={cn(badgeVariants({ variant }), className)} {...rest}>
  {@render children?.()}
</span>
