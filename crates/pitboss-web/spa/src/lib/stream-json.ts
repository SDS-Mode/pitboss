// Parser for the `claude --output-format stream-json` line dialect that
// pitboss captures verbatim into `tasks/<task_id>/stdout.log`. Mirrors
// the variants in `pitboss_core::parser::Event` (events.rs) but with
// the extra detail the SPA needs for human display: tool_use input,
// tool_result content, the thinking-vs-text split inside an assistant
// message.
//
// Unknown shapes fall through to `kind: 'unknown'` rather than throw —
// claude's wire format evolves and one new field shouldn't blank an
// operator's whole log view.

/** One parsed line from `stdout.log`. Discriminated by `kind`. */
export type StreamJsonRow =
  /** `type:"system", subtype:"init"` — once per session start. */
  | {
      kind: 'init';
      cwd?: string;
      model?: string;
      session_id?: string;
      tool_count?: number;
      claude_code_version?: string;
    }
  /** `type:"system", subtype:"hook_started"|"hook_response"` — Claude's
   *  pre/post hook scaffold. Verbose; usually noise for log review. */
  | {
      kind: 'hook';
      phase: 'started' | 'response';
      name?: string;
      event?: string;
      outcome?: string;
      exit_code?: number;
    }
  /** Other system events (rate-limit notices, end-of-turn etc.). */
  | { kind: 'system'; subtype: string; raw: unknown }
  /** assistant.content[i].type === 'thinking' */
  | { kind: 'thinking'; text: string }
  /** assistant.content[i].type === 'text' */
  | { kind: 'assistant_text'; text: string }
  /** assistant.content[i].type === 'tool_use' */
  | { kind: 'tool_use'; tool_name: string; tool_use_id: string; input: unknown }
  /** user.content[i].type === 'tool_result' (carries previous tool's output) */
  | { kind: 'tool_result'; tool_use_id: string; content: string; is_error: boolean }
  /** Free-form user message (rare in pitboss runs but possible during reprompt). */
  | { kind: 'user_message'; text: string }
  /** Final-turn `type:"result"` row. */
  | {
      kind: 'result';
      subtype?: string;
      session_id?: string;
      text?: string;
      is_error: boolean;
    }
  /** First-class rate-limit row (Claude Code emits these inline). */
  | { kind: 'rate_limit'; status: string; resets_at?: number }
  /** Anything else. The `raw` payload is JSON-stringified for the toggle. */
  | { kind: 'unknown'; raw: unknown };

/** Parse one stream-json line. Returns `null` for empty / blank lines or
 *  bytes that don't parse as JSON at all (partial-write tail, etc.) so
 *  the caller can `filter(Boolean)` them out. */
export function parseStreamJsonLine(line: string): StreamJsonRow | null {
  const trimmed = line.trim();
  if (!trimmed) return null;
  let obj: unknown;
  try {
    obj = JSON.parse(trimmed);
  } catch {
    return null;
  }
  if (!obj || typeof obj !== 'object') return null;
  const o = obj as Record<string, unknown>;
  const type = typeof o.type === 'string' ? o.type : '';

  if (type === 'system') return parseSystem(o);
  if (type === 'assistant') return parseAssistant(o);
  if (type === 'user') return parseUser(o);
  if (type === 'result') return parseResult(o);
  if (type === 'rate_limit') return parseRateLimit(o);

  return { kind: 'unknown', raw: obj };
}

/** Parse all lines in a NDJSON blob. Skips unparseable / blank lines. */
export function parseStreamJson(blob: string): StreamJsonRow[] {
  const rows: StreamJsonRow[] = [];
  for (const line of blob.split('\n')) {
    const row = parseStreamJsonLine(line);
    if (row) rows.push(row);
  }
  return rows;
}

// ---- internals ----------------------------------------------------------

function parseSystem(o: Record<string, unknown>): StreamJsonRow {
  const subtype = typeof o.subtype === 'string' ? o.subtype : '';
  if (subtype === 'init') {
    const tools = Array.isArray(o.tools) ? o.tools : null;
    return {
      kind: 'init',
      cwd: strField(o, 'cwd'),
      model: strField(o, 'model'),
      session_id: strField(o, 'session_id'),
      tool_count: tools ? tools.length : undefined,
      claude_code_version: strField(o, 'claude_code_version')
    };
  }
  if (subtype === 'hook_started' || subtype === 'hook_response') {
    return {
      kind: 'hook',
      phase: subtype === 'hook_started' ? 'started' : 'response',
      name: strField(o, 'hook_name'),
      event: strField(o, 'hook_event'),
      outcome: strField(o, 'outcome'),
      exit_code: numField(o, 'exit_code')
    };
  }
  return { kind: 'system', subtype: subtype || '(unknown)', raw: o };
}

function parseAssistant(o: Record<string, unknown>): StreamJsonRow {
  const message = (o.message as Record<string, unknown> | undefined) ?? null;
  if (!message) return { kind: 'unknown', raw: o };
  const content = Array.isArray(message.content) ? message.content : [];
  // Each line emits one block at a time in claude's stream-json mode, so
  // `content` typically has length 1. We render the first non-empty
  // block we recognise; multi-block lines fall back to assistant_text.
  for (const block of content) {
    if (!block || typeof block !== 'object') continue;
    const b = block as Record<string, unknown>;
    const t = typeof b.type === 'string' ? b.type : '';
    if (t === 'thinking') {
      const text = strField(b, 'thinking') ?? '';
      if (text) return { kind: 'thinking', text };
    } else if (t === 'text') {
      const text = strField(b, 'text') ?? '';
      if (text) return { kind: 'assistant_text', text };
    } else if (t === 'tool_use') {
      return {
        kind: 'tool_use',
        tool_name: strField(b, 'name') ?? '?',
        tool_use_id: strField(b, 'id') ?? '',
        input: b.input ?? null
      };
    }
  }
  return { kind: 'unknown', raw: o };
}

function parseUser(o: Record<string, unknown>): StreamJsonRow {
  const message = (o.message as Record<string, unknown> | undefined) ?? null;
  if (!message) return { kind: 'unknown', raw: o };
  const content = message.content;
  if (typeof content === 'string') {
    return { kind: 'user_message', text: content };
  }
  if (Array.isArray(content)) {
    for (const block of content) {
      if (!block || typeof block !== 'object') continue;
      const b = block as Record<string, unknown>;
      const t = typeof b.type === 'string' ? b.type : '';
      if (t === 'tool_result') {
        return {
          kind: 'tool_result',
          tool_use_id: strField(b, 'tool_use_id') ?? '',
          content: extractToolResultText(b.content),
          is_error: b.is_error === true
        };
      }
      if (t === 'text') {
        const text = strField(b, 'text') ?? '';
        if (text) return { kind: 'user_message', text };
      }
    }
  }
  return { kind: 'unknown', raw: o };
}

function parseResult(o: Record<string, unknown>): StreamJsonRow {
  return {
    kind: 'result',
    subtype: strField(o, 'subtype'),
    session_id: strField(o, 'session_id'),
    text: strField(o, 'result') ?? strField(o, 'text'),
    is_error: o.is_error === true || strField(o, 'subtype') === 'error_during_execution'
  };
}

function parseRateLimit(o: Record<string, unknown>): StreamJsonRow {
  return {
    kind: 'rate_limit',
    status: strField(o, 'status') ?? 'rate_limited',
    resets_at: numField(o, 'resets_at')
  };
}

/** Tool-result content is sometimes a string, sometimes an array of
 *  `{type: 'text', text: '...'}` blocks. Flatten to a single string. */
function extractToolResultText(content: unknown): string {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) {
    const parts: string[] = [];
    for (const block of content) {
      if (typeof block === 'string') {
        parts.push(block);
      } else if (block && typeof block === 'object') {
        const b = block as Record<string, unknown>;
        const text = strField(b, 'text');
        if (text) parts.push(text);
      }
    }
    return parts.join('\n');
  }
  return '';
}

function strField(o: Record<string, unknown>, key: string): string | undefined {
  const v = o[key];
  return typeof v === 'string' ? v : undefined;
}

function numField(o: Record<string, unknown>, key: string): number | undefined {
  const v = o[key];
  return typeof v === 'number' ? v : undefined;
}

/** Compact one-line preview of a tool_use input object. Truncates to
 *  `maxLen` chars; uses `command` / `file_path` / `pattern` when present
 *  (Bash / Read / Grep) so the row reads naturally. */
export function toolInputPreview(input: unknown, maxLen = 120): string {
  if (!input || typeof input !== 'object') return '';
  const o = input as Record<string, unknown>;
  // Common fields used by built-in claude tools.
  const ranked = ['command', 'file_path', 'pattern', 'path', 'url', 'query', 'prompt'];
  for (const key of ranked) {
    const v = o[key];
    if (typeof v === 'string' && v.length > 0) {
      return truncate(v, maxLen);
    }
  }
  // Fallback: first string field, in declaration order.
  for (const key of Object.keys(o)) {
    const v = o[key];
    if (typeof v === 'string' && v.length > 0) {
      return truncate(`${key}=${v}`, maxLen);
    }
  }
  return truncate(JSON.stringify(o), maxLen);
}

function truncate(s: string, maxLen: number): string {
  const collapsed = s.replace(/\s+/g, ' ').trim();
  return collapsed.length > maxLen ? collapsed.slice(0, maxLen - 1) + '…' : collapsed;
}
