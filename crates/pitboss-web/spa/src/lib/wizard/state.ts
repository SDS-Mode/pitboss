// Wizard state for /manifests/new. The shape is intentionally flat —
// it's transformed into TOML by `composeToml` in compose.ts and is the
// single thing the wizard mutates as the user clicks through steps.
//
// Step ordering is fixed: 1 Basics → 2 Mode → 3 Defaults → 4 Work → 5
// Review. The Next button's enabled state is driven by `stepValid()`.

export type WizardStep = 1 | 2 | 3 | 4 | 5;

export type Mode = 'flat' | 'hierarchical';
export type Effort = 'low' | 'medium' | 'high' | 'xhigh' | 'max';

export interface FlatTask {
  id: string;
  directory: string;
  prompt: string;
}

export interface LeadConfig {
  id: string;
  directory: string;
  prompt: string;
  max_workers: number;
  budget_usd: number;
  lead_timeout_secs: number;
}

export interface WizardState {
  // Step 1 — Basics
  filename: string;
  run_name: string;
  // Step 2 — Mode
  mode: Mode;
  // Step 3 — Defaults
  model: string;
  effort: Effort;
  tools: string[];
  use_worktree: boolean;
  timeout_secs: number | null;
  // Step 4 — Work (which one is used depends on mode)
  tasks: FlatTask[];
  lead: LeadConfig;
}

export const AVAILABLE_TOOLS = ['Read', 'Write', 'Edit', 'Bash', 'Glob', 'Grep'] as const;

export const MODELS = [
  'claude-haiku-4-5',
  'claude-sonnet-4-6',
  'claude-opus-4-7'
] as const;

export const EFFORTS: Effort[] = ['low', 'medium', 'high', 'xhigh', 'max'];

export function emptyState(): WizardState {
  return {
    filename: '',
    run_name: '',
    mode: 'flat',
    model: 'claude-sonnet-4-6',
    effort: 'medium',
    tools: ['Read', 'Grep', 'Glob', 'Bash'],
    use_worktree: true,
    timeout_secs: 1800,
    tasks: [{ id: 'task-1', directory: '', prompt: '' }],
    lead: {
      id: 'coordinator',
      directory: '',
      prompt: '',
      max_workers: 4,
      budget_usd: 5.0,
      lead_timeout_secs: 1800
    }
  };
}

/** Same allow-list the backend's `sanitize_manifest_name` enforces. */
export function isValidFilename(s: string): boolean {
  const trimmed = s.trim();
  if (!trimmed || trimmed.length > 64) return false;
  if (trimmed.includes('/') || trimmed.includes('\\') || trimmed.includes('..')) return false;
  return /^[A-Za-z0-9._-]+$/.test(trimmed);
}

/** True when the current step's required fields are all populated. */
export function stepValid(state: WizardState, step: WizardStep): boolean {
  switch (step) {
    case 1:
      return isValidFilename(state.filename) && state.run_name.trim().length > 0;
    case 2:
      return state.mode === 'flat' || state.mode === 'hierarchical';
    case 3:
      return MODELS.includes(state.model as (typeof MODELS)[number]);
    case 4:
      if (state.mode === 'flat') {
        return (
          state.tasks.length > 0 &&
          state.tasks.every(
            (t) => t.id.trim() && t.directory.trim() && t.prompt.trim()
          )
        );
      }
      return Boolean(
        state.lead.id.trim() &&
          state.lead.directory.trim() &&
          state.lead.prompt.trim() &&
          state.lead.max_workers >= 1 &&
          state.lead.max_workers <= 16
      );
    case 5:
      return true;
  }
}

/** All steps valid up to and including `target`. */
export function canAdvanceTo(state: WizardState, target: WizardStep): boolean {
  for (let s = 1; s < target; s++) {
    if (!stepValid(state, s as WizardStep)) return false;
  }
  return true;
}
