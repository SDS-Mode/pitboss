<script lang="ts">
  import '../app.css';
  import { ModeWatcher } from 'mode-watcher';
  import SiteHeader from '$lib/components/site-header.svelte';

  let { children } = $props();
</script>

<ModeWatcher />

<div class="bg-background text-foreground min-h-screen antialiased">
  <SiteHeader />
  <div class="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8">
    {@render children?.()}
  </div>
</div>
