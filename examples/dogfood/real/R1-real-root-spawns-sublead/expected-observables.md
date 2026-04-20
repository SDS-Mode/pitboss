# R1 — Expected observables

## Scenario

The root lead (claude-haiku-4-5) receives a prompt describing a two-phase data
pipeline and explicit instructions to use `spawn_sublead` for each phase.

## Expected sequence

1. **Dispatch starts**: pitboss launches the root lead process with an MCP
   config that exposes the pitboss MCP server. The toolset includes
   `spawn_sublead` because `allow_subleads = true`.

2. **Model calls `spawn_sublead`** one or more times (variance expected):
   - The model may spawn exactly two sub-leads (one per phase) if it follows
     the prompt literally.
   - It may spawn one sub-lead if it treats both phases as a single unit.
   - It will not spawn zero sub-leads — the prompt explicitly requires it.
   - Each call receives a response containing `{ "sublead_id": "sublead-<uuid>" }`.

3. **Model calls `wait_actor`** on each spawned sublead_id:
   - Because `spawn_sublead_session` is a no-op stub in v0.6, no sub-lead
     Claude subprocess is actually launched.
   - `wait_actor` will block until `lead_timeout_secs` (300s) or the model's
     own tool timeout fires, whichever comes first.
   - The model may handle the timeout gracefully (e.g. emit a summary anyway)
     or the run may end with a lead timeout.

4. **Run completes**:
   - `pitboss dispatch` exits 0 on clean lead completion.
   - `summary.json` is written with `status = "Success"`, `tasks_failed = 0`,
     `was_interrupted = false`.

## Test success criteria

The test passes when ALL of the following hold:

- `pitboss dispatch` exits with code 0.
- `summary.json` contains `"status": "Success"` for the lead task.
- At least one `sublead-` token appears in the combined process output
  (stdout + stderr), confirming the MCP tool call was made and a sublead_id
  was minted.

## What is NOT asserted

- Exact number of sub-leads (may be 1 or 2 depending on model interpretation).
- Exact prompt text passed to `spawn_sublead`.
- That sub-lead sessions actually run (they don't in v0.6 — stub).
- That `wait_actor` returns successfully (it may timeout due to stub sessions).
- Token counts or cost (subject to Anthropic pricing changes).

## Variance notes

Haiku is a fast, cost-efficient model but does exhibit prompt-following variance.
On repeated runs, the number of `spawn_sublead` calls may differ. The test is
intentionally loose: `>= 1` sublead call is sufficient to prove tool
discoverability and model understanding.
