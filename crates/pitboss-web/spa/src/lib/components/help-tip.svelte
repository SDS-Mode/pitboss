<script lang="ts">
  // Inline tooltip showing the canonical FieldDescriptor.help text from
  // /api/schema. Two usage modes:
  //
  // <HelpTip help="literal string" />  — inline; renders only when help
  //                                      text is present.
  // <HelpTip {schema} section="[run]" field="name" />
  //                                    — looks up the help string from a
  //                                      pre-fetched schema map.
  //
  // The trigger is a small ? icon next to the field label so the wizard
  // never has to invent help text — every string is the canonical one
  // shipped from pitboss_schema.

  import { Tooltip } from 'bits-ui';
  import { HelpCircle } from 'lucide-svelte';
  import type { SchemaSection } from '$lib/api';

  interface Props {
    /** Direct help string. Takes precedence over schema lookup. */
    help?: string;
    /** Pre-fetched schema (from `getSchema()`). */
    schema?: SchemaSection[];
    /** Section's `toml_path`, e.g. `"[run]"`. */
    section?: string;
    /** Field `name` within that section. */
    field?: string;
    /** Override sr-only label. Defaults to "Show help". */
    label?: string;
  }

  let { help, schema, section, field, label = 'Show help' }: Props = $props();

  const text = $derived.by(() => {
    if (help) return help;
    if (!schema || !section || !field) return '';
    const sec = schema.find((s) => s.toml_path === section);
    if (!sec) return '';
    return sec.fields.find((f) => f.name === field)?.help ?? '';
  });
</script>

{#if text}
  <Tooltip.Root delayDuration={400}>
    <Tooltip.Trigger
      class="text-muted-foreground hover:text-foreground inline-flex items-center justify-center rounded-full focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
      aria-label={label}
    >
      <HelpCircle class="size-3.5" />
    </Tooltip.Trigger>
    <Tooltip.Content
      sideOffset={4}
      class="bg-primary text-primary-foreground z-50 max-w-xs overflow-hidden rounded-md px-3 py-1.5 text-xs leading-relaxed shadow-md"
    >
      {text}
    </Tooltip.Content>
  </Tooltip.Root>
{/if}
