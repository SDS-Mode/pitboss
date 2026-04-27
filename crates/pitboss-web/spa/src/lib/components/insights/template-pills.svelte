<script lang="ts">
  // Render a Drain-lite canonical template (e.g. "exit <NUM> in <PATH>")
  // with mask placeholders highlighted as muted-grey pills so the
  // operator can scan templates at a glance and tell the structural
  // skeleton from the literal tokens.

  interface Props {
    template: string;
    class?: string;
  }

  let { template, class: cls = '' }: Props = $props();

  const KNOWN_MASKS = new Set([
    '<UUID>',
    '<TS>',
    '<URL>',
    '<STR>',
    '<IP>',
    '<LOC>',
    '<HEX>',
    '<PATH>',
    '<NUM>'
  ]);

  type Part = { kind: 'lit'; text: string } | { kind: 'mask'; tag: string };

  const parts = $derived(splitMasks(template));

  function splitMasks(t: string): Part[] {
    const out: Part[] = [];
    let buf = '';
    let i = 0;
    while (i < t.length) {
      if (t[i] === '<') {
        const end = t.indexOf('>', i);
        if (end !== -1) {
          const tag = t.slice(i, end + 1);
          if (KNOWN_MASKS.has(tag)) {
            if (buf) {
              out.push({ kind: 'lit', text: buf });
              buf = '';
            }
            out.push({ kind: 'mask', tag });
            i = end + 1;
            continue;
          }
        }
      }
      buf += t[i];
      i++;
    }
    if (buf) out.push({ kind: 'lit', text: buf });
    return out;
  }
</script>

<span class="font-mono text-xs whitespace-pre-wrap break-all {cls}">
  {#each parts as part, i (i)}
    {#if part.kind === 'lit'}<span>{part.text}</span
      >{:else}<span
        class="bg-muted text-muted-foreground mx-0.5 inline-block rounded px-1 text-[10px] font-medium uppercase tracking-wide"
        >{part.tag.slice(1, -1)}</span
      >{/if}
  {/each}
</span>
