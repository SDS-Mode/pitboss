<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { mode } from 'mode-watcher';
  import type { EChartsType } from 'echarts';

  interface Props {
    /** ECharts option object. Re-applied with `notMerge: true` on change. */
    option: Record<string, unknown>;
    /** CSS height. Defaults to `h-64`. */
    class?: string;
    /** Optional click handler — receives the raw ECharts event params. */
    onclick?: (params: unknown) => void;
  }

  let { option, class: cls = 'h-64 w-full', onclick }: Props = $props();

  let host: HTMLDivElement;
  let chart: EChartsType | null = null;
  let resizeObs: ResizeObserver | null = null;
  let lastTheme: 'light' | 'dark' | null = null;

  // Subscribe to mode-watcher's store via $store auto-subscription.
  const themeName = $derived($mode === 'dark' ? 'dark' : 'light');

  onMount(async () => {
    const echarts = await import('echarts');
    chart = echarts.init(host, themeName);
    chart.setOption(option, { notMerge: true });
    lastTheme = themeName;
    if (onclick) chart.on('click', onclick);
    resizeObs = new ResizeObserver(() => chart?.resize());
    resizeObs.observe(host);
  });

  onDestroy(() => {
    resizeObs?.disconnect();
    chart?.dispose();
    chart = null;
  });

  // Re-apply option when caller updates it.
  $effect(() => {
    if (chart) chart.setOption(option, { notMerge: true });
  });

  // Re-init on theme toggle (echarts theme is baked at init time).
  $effect(() => {
    if (!chart || lastTheme === themeName) return;
    void rebuild(themeName);
  });

  async function rebuild(t: 'light' | 'dark') {
    const echarts = await import('echarts');
    chart?.dispose();
    chart = echarts.init(host, t);
    chart.setOption(option, { notMerge: true });
    if (onclick) chart.on('click', onclick);
    lastTheme = t;
  }
</script>

<div bind:this={host} class={cls}></div>
