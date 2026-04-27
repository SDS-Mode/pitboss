<script lang="ts">
  import { Badge } from '$lib/components/ui/badge';
  import type { RunStatus } from '$lib/api';

  let { status, label }: { status: RunStatus; label?: string } = $props();

  const variant = $derived(
    status === 'complete'
      ? 'default'
      : status === 'running'
        ? 'secondary'
        : status === 'stale'
          ? 'outline'
          : 'destructive'
  );
  const colorClass = $derived(
    status === 'complete'
      ? 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 border-emerald-500/30'
      : status === 'running'
        ? 'bg-sky-500/15 text-sky-700 dark:text-sky-400 border-sky-500/30 animate-pulse'
        : status === 'stale'
          ? 'bg-amber-500/15 text-amber-700 dark:text-amber-400 border-amber-500/30'
          : 'bg-red-500/15 text-red-700 dark:text-red-400 border-red-500/30'
  );
</script>

<Badge {variant} class="border {colorClass} font-medium uppercase tracking-wide">
  {label ?? status}
</Badge>
