<script lang="ts">
  import {
    type ResourceSampleEvent,
    type ResourcePressureEvent,
    type ResourceHighWater,
    type ResourceSampleEntry
  } from '$lib/api';
  import {
    Card,
    CardContent,
    CardDescription,
    CardHeader,
    CardTitle
  } from '$lib/components/ui/card';
  import {
    Table,
    TableBody,
    TableCell,
    TableHead,
    TableHeader,
    TableRow
  } from '$lib/components/ui/table';
  import { Badge } from '$lib/components/ui/badge';
  import EChart from '$lib/components/charts/echart.svelte';
  import { AlertTriangle } from '@lucide/svelte';

  interface Props {
    /** Live samples (newest last). Empty for finalized runs. */
    samples: Array<{ envelope: ResourceSampleEvent; at: number }>;
    /** Most-recent live sample, if any. */
    latest: ResourceSampleEvent | null;
    /** Most-recent pressure transition (live). `null` if cleared. */
    pressure: ResourcePressureEvent | null;
    /** Roll-up from `summary.json` for finalized runs. */
    highWater: ResourceHighWater | null;
    /** `true` while the SSE bridge is open. */
    inProgress: boolean;
  }

  let { samples, latest, pressure, highWater, inProgress }: Props = $props();

  // ---- Headroom area chart over the rolling sample buffer ---------------
  //
  // X axis: sample timestamp (ms since epoch). Y axis: total RSS bytes.
  // A second flat line marks `cgroup memory.max` so the operator can see
  // how close the run is to the cap.
  const chartOption = $derived.by(() => {
    if (samples.length === 0) {
      return {
        title: {
          text: inProgress ? 'Waiting for first sample…' : 'No samples recorded.',
          left: 'center',
          top: 'middle',
          textStyle: { fontSize: 12, fontWeight: 'normal' }
        }
      };
    }
    const data: Array<[number, number]> = samples.map((s) => {
      const total = (s.envelope.samples ?? []).reduce(
        (acc, e) => acc + (e.rss_bytes ?? 0),
        0
      );
      return [s.at, total];
    });
    const cgroupMax = latest?.cgroup_memory_max_bytes ?? null;
    const series: Record<string, unknown>[] = [
      {
        name: 'Total RSS',
        type: 'line',
        showSymbol: false,
        areaStyle: {},
        data
      }
    ];
    if (cgroupMax) {
      series.push({
        name: 'cgroup memory.max',
        type: 'line',
        showSymbol: false,
        lineStyle: { type: 'dashed' },
        data: data.map(([t]) => [t, cgroupMax])
      });
    }
    return {
      grid: { left: 60, right: 16, top: 24, bottom: 32 },
      xAxis: { type: 'time' },
      yAxis: {
        type: 'value',
        axisLabel: {
          formatter: (v: number) => `${(v / (1024 * 1024 * 1024)).toFixed(1)} GB`
        }
      },
      tooltip: {
        trigger: 'axis',
        valueFormatter: (v: number) =>
          `${(v / (1024 * 1024 * 1024)).toFixed(2)} GB`
      },
      legend: { top: 0, right: 0, textStyle: { fontSize: 11 } },
      series
    };
  });

  // ---- Per-actor table from the latest sample --------------------------
  type ActorRow = {
    actor_id: string;
    pid: number;
    rss_bytes: number;
    rss_peak_bytes: number;
    vsz_bytes: number;
    cpu_pct: number | null;
    spark: number[];
  };

  const perActorRows = $derived.by<ActorRow[]>(() => {
    // Build a per-actor history out of the rolling buffer so the spark
    // column has data. CPU% is the delta of cpu_jiffies / elapsed_sec /
    // ticks_per_sec — we assume 100 Hz, which is the linux default.
    const TICKS_PER_SEC = 100;
    const last = latest?.samples ?? [];
    const byActor = new Map<string, ResourceSampleEntry[]>();
    for (const s of samples) {
      for (const entry of s.envelope.samples ?? []) {
        if (!byActor.has(entry.actor_id)) byActor.set(entry.actor_id, []);
        byActor.get(entry.actor_id)!.push(entry);
      }
    }
    const rows: ActorRow[] = [];
    for (const entry of last) {
      const history = byActor.get(entry.actor_id) ?? [];
      const spark = history.map((h) => h.rss_bytes ?? 0);
      const peak = history.reduce((m, h) => Math.max(m, h.rss_bytes ?? 0), 0);
      let cpu: number | null = null;
      if (history.length >= 2) {
        const last2 = history[history.length - 1];
        const first2 = history[0];
        const djiff = (last2.cpu_jiffies ?? 0) - (first2.cpu_jiffies ?? 0);
        const idxLast = samples.length - 1;
        const idxFirst = Math.max(0, samples.length - history.length);
        const dms = samples[idxLast].at - samples[idxFirst].at;
        if (dms > 0) {
          cpu = (djiff / (dms / 1000) / TICKS_PER_SEC) * 100;
        }
      }
      rows.push({
        actor_id: entry.actor_id,
        pid: entry.pid ?? 0,
        rss_bytes: entry.rss_bytes ?? 0,
        rss_peak_bytes: peak,
        vsz_bytes: entry.vsz_bytes ?? 0,
        cpu_pct: cpu,
        spark
      });
    }
    rows.sort((a, b) => b.rss_bytes - a.rss_bytes);
    return rows;
  });

  function fmtBytes(b: number): string {
    if (b === 0) return '—';
    if (b < 1024 * 1024) return `${(b / 1024).toFixed(0)} KB`;
    if (b < 1024 * 1024 * 1024) return `${(b / (1024 * 1024)).toFixed(0)} MB`;
    return `${(b / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  }

  function fmtPct(p: number | null): string {
    if (p === null) return '—';
    return `${p.toFixed(1)}%`;
  }

  function sparkBars(values: number[], peak: number): string {
    if (values.length === 0 || peak === 0) return '';
    // 8-block Unicode ramp matches the mockup style.
    const ramp = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    const head = values.slice(-12);
    return head
      .map((v) => ramp[Math.min(7, Math.floor(((v / peak) * (ramp.length - 1)) + 0.5))])
      .join('');
  }

  // ---- Headline banner from `pressure` or `highWater` ------------------
  const headline = $derived.by(() => {
    if (pressure) {
      return {
        level: pressure.level,
        text:
          pressure.message ??
          `Memory pressure: ${pressure.level} — ${fmtBytes(pressure.total_rss_bytes ?? 0)} of ${fmtBytes(pressure.available_bytes ?? 0)}`
      };
    }
    if (highWater && (highWater.peak_utilization_pct ?? 0) > 0) {
      const pct = ((highWater.peak_utilization_pct ?? 0) * 100).toFixed(0);
      const totalGb = (
        (highWater.total_rss_bytes_max ?? 0) /
        (1024 * 1024 * 1024)
      ).toFixed(2);
      const availGb = highWater.cgroup_memory_max_bytes
        ? (highWater.cgroup_memory_max_bytes / (1024 * 1024 * 1024)).toFixed(2)
        : null;
      return {
        level: (highWater.peak_utilization_pct ?? 0) >= 0.9 ? 'error' : 'info',
        text: availGb
          ? `Peak memory ${pct}% of cgroup memory.max (${totalGb} / ${availGb} GB) — ${highWater.sample_count ?? 0} samples`
          : `Peak total RSS ${totalGb} GB across ${highWater.sample_count ?? 0} samples`
      };
    }
    return null;
  });
</script>

{#if headline}
  <Card
    class={headline.level === 'error'
      ? 'border-destructive/50 bg-destructive/5'
      : headline.level === 'warn'
        ? 'border-amber-500/50 bg-amber-500/5'
        : 'border-muted-foreground/30'}
  >
    <CardContent class="flex items-start gap-3 pt-6">
      {#if headline.level === 'error' || headline.level === 'warn'}
        <AlertTriangle class="mt-0.5 size-5 shrink-0 text-amber-600" />
      {/if}
      <p class="text-sm">{headline.text}</p>
    </CardContent>
  </Card>
{/if}

<Card>
  <CardHeader class="pb-3">
    <CardTitle class="text-base">Container headroom</CardTitle>
    <CardDescription class="text-xs">
      Total RSS over time, with the cgroup ceiling overlaid when known.
    </CardDescription>
  </CardHeader>
  <CardContent class="pt-0">
    <EChart option={chartOption} class="h-48 w-full" />
  </CardContent>
</Card>

<Card>
  <CardHeader class="pb-3">
    <CardTitle class="text-base">
      Per-actor
      <Badge variant="outline" class="ml-2 text-xs">{perActorRows.length}</Badge>
    </CardTitle>
    <CardDescription class="text-xs">
      RSS / VSZ / CPU% from the latest sample. Spark shows the last
      12 samples per actor.
    </CardDescription>
  </CardHeader>
  <CardContent class="pt-0">
    {#if perActorRows.length === 0}
      <p class="text-muted-foreground py-4 text-center text-xs">
        {inProgress
          ? 'Waiting for first resource sample…'
          : highWater
            ? 'This run completed without a live sample; finalize roll-up shown above.'
            : 'No resource samples recorded for this run.'}
      </p>
    {:else}
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Actor</TableHead>
            <TableHead class="w-[8ch] text-right">PID</TableHead>
            <TableHead class="w-[10ch] text-right">RSS</TableHead>
            <TableHead class="w-[10ch] text-right">Peak</TableHead>
            <TableHead class="w-[10ch] text-right">VSZ</TableHead>
            <TableHead class="w-[8ch] text-right">CPU%</TableHead>
            <TableHead class="w-[14ch]">Spark</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {#each perActorRows as r (r.actor_id)}
            <TableRow>
              <TableCell class="font-mono text-xs">{r.actor_id}</TableCell>
              <TableCell class="text-right tabular-nums text-xs">{r.pid || '—'}</TableCell>
              <TableCell class="text-right tabular-nums text-xs">{fmtBytes(r.rss_bytes)}</TableCell>
              <TableCell class="text-right tabular-nums text-xs">{fmtBytes(r.rss_peak_bytes)}</TableCell>
              <TableCell class="text-right tabular-nums text-xs">{fmtBytes(r.vsz_bytes)}</TableCell>
              <TableCell class="text-right tabular-nums text-xs">{fmtPct(r.cpu_pct)}</TableCell>
              <TableCell class="font-mono text-xs">{sparkBars(r.spark, r.rss_peak_bytes)}</TableCell>
            </TableRow>
          {/each}
        </TableBody>
      </Table>
    {/if}
  </CardContent>
</Card>
