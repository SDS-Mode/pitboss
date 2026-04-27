<script lang="ts">
  import '../app.css';
  import { ModeWatcher } from 'mode-watcher';
  import { Tooltip } from 'bits-ui';
  import SiteHeader from '$lib/components/site-header.svelte';

  let { children } = $props();
</script>

<ModeWatcher />

<!-- bits-ui v2 Tooltip.Root requires a Provider ancestor; mounting it
     here covers every page (used by HelpTip throughout the wizard). -->
<Tooltip.Provider delayDuration={400}>
  <div class="bg-background text-foreground min-h-screen antialiased">
    <SiteHeader />
    <div class="mx-auto max-w-7xl px-4 py-6 sm:px-6 lg:px-8">
      {@render children?.()}
    </div>
  </div>
</Tooltip.Provider>
