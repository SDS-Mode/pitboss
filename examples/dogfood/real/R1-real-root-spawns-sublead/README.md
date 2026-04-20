# R1 — Real claude root lead uses spawn_sublead

## What it proves

A real `claude-haiku-4-5` model session, given a prompt instructing it to
decompose a two-phase job into sub-leads, actually calls the `spawn_sublead`
MCP tool with reasonable arguments.

Specifically this validates:

- The `spawn_sublead` tool is discoverable via `tools/list` when
  `allow_subleads = true` is set in the manifest.
- The tool description is clear enough for the haiku model to invoke it
  correctly without additional guidance.
- The model understands the distinction between `spawn_sublead` (for
  sub-orchestrators) and `spawn_worker` (for leaf tasks).
- The arguments produced by the model are sensible: `budget_usd > 0`,
  `max_workers > 0`, and the prompt describes the assigned phase.

## PREREQUISITES

1. Install the `claude` CLI and authenticate:
   See https://docs.claude.com/en/docs/claude-code for installation.

2. Export your Anthropic API key:
   ```
   export ANTHROPIC_API_KEY=sk-ant-...
   ```

3. Build the pitboss release binary from the repo root:
   ```
   cargo build --workspace --release
   ```

4. Run the smoke test via the shell script:
   ```
   bash examples/dogfood/real/R1-real-root-spawns-sublead/run.sh
   ```

   Or run the Rust integration test (requires `--ignored` + `PITBOSS_DOGFOOD_REAL=1`):
   ```
   PITBOSS_DOGFOOD_REAL=1 cargo test --test dogfood_real_flows real_root_spawns_sublead -- --ignored
   ```

## Cost estimate

~$0.05 per run (haiku model, small decomposition prompt + MCP tool calls).

## Caveats

- **Model variance**: The exact number of sub-leads spawned may vary (haiku may
  spawn one or two depending on its interpretation of the prompt). The test
  asserts `>= 1` sub-lead, not exactly 2.
- **Stub sessions**: `spawn_sublead_session` is a no-op stub in v0.6 (Task 2.3
  wires real sub-lead Claude sessions). The sub-lead processes do not actually
  run; `wait_actor` on a spawned sub-lead will block until the lead's
  `lead_timeout_secs` expires. The test uses a short timeout on `wait_actor`
  so the run completes rather than hanging.
- **No exact output assertion**: The test does not assert on exact model output,
  exact number of sub-leads, or specific prompt wording — only that at least
  one `spawn_sublead` call occurred.
