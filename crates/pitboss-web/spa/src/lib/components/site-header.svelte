<script lang="ts">
  import { Activity, Moon, Sun } from 'lucide-svelte';
  import { Button } from '$lib/components/ui/button';
  import { toggleMode } from 'mode-watcher';
</script>

<header class="border-border/40 bg-background/80 sticky top-0 z-30 border-b backdrop-blur">
  <div class="mx-auto flex h-14 max-w-7xl items-center gap-4 px-4 sm:px-6 lg:px-8">
    <a href="/" class="flex items-center gap-2 font-semibold tracking-tight">
      <Activity class="text-primary size-5" />
      <span>Pitboss</span>
      <span class="text-muted-foreground text-xs font-normal">console</span>
    </a>

    <nav class="ml-6 flex items-center gap-1 text-sm">
      <a
        href="/"
        class="text-muted-foreground hover:text-foreground rounded-md px-3 py-1.5 transition"
      >
        Runs
      </a>
      <a
        href="/manifests"
        class="text-muted-foreground hover:text-foreground rounded-md px-3 py-1.5 transition"
      >
        Manifests
      </a>
    </nav>

    <div class="ml-auto flex items-center gap-1">
      <Button variant="ghost" size="icon" onclick={toggleMode} aria-label="Toggle theme">
        <Sun class="size-4 dark:hidden" />
        <Moon class="hidden size-4 dark:inline" />
      </Button>
    </div>
  </div>
</header>
