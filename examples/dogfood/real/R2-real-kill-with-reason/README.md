# R2 — Real claude root lead adapts after worker is killed-with-reason

## What it proves

A real `claude-haiku-4-5` model session, given a prompt that asks it to spawn
a worker and wait for results, receives a synthetic `[SYSTEM]` reprompt when an
external operator kills the worker with a corrective reason. The model's next
turn visibly references the kill reason, demonstrating LLM-adaptive replanning
in response to the `cancel_worker` reason field.

Specifically this validates:

- `cancel_worker` with `reason=...` causes a synthetic `[SYSTEM]` reprompt to
  be injected into the lead's active claude session.
- The real model reads the injected reason and adjusts its plan (mentions CSV,
  format change, or similar adaptation keyword).
- The kill-with-reason mechanism works end-to-end through the MCP bridge.

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

4. Run the smoke test via the shell script (drives the side-channel operator):
   ```
   bash examples/dogfood/real/R2-real-kill-with-reason/run.sh
   ```

   Or run the Rust integration test (requires `--ignored` + `PITBOSS_DOGFOOD_REAL=1`):
   ```
   PITBOSS_DOGFOOD_REAL=1 cargo test --test dogfood_real_flows real_kill_with_reason -- --ignored
   ```

## Cost estimate

~$0.10-$0.20 per run (haiku model, lead + worker spawn + reprompt round-trip).

## Orchestration pattern

R2 requires a side-channel operator that calls `cancel_worker` with a reason
while the real-claude root lead is mid-flight. The test implementation uses a
background tokio task that connects to the MCP socket directly and issues the
`cancel_worker` call after observing the first worker appear in the dispatch
state.

This is more complex than R1's "dispatch, wait, inspect" pattern. R2 uses an
in-process DispatchState + real claude subprocess — the "Option A" pattern
described in the R2 design document.

## Current implementation status

R2 is currently implemented as a stub that returns early with a skip message.
Full Option A orchestration (in-process DispatchState + real claude subprocess +
FakeMcpClient side-channel cancel) requires additional test infrastructure that
was deferred. See `crates/pitboss-cli/tests/dogfood_real_flows.rs` for the stub.

## Caveats

- **Side-channel complexity**: Driving `cancel_worker` while real claude is
  mid-flight requires the test to act as a concurrent MCP client, racing the
  lead's tool calls. Timing is non-trivial.
- **Model variance**: The model may or may not spawn a worker on the first turn
  depending on prompt interpretation. The test requires at least one worker to
  appear before cancellation.
- **Stub sessions**: If real worker subprocess support is limited, the worker
  may never start — the reprompt still fires because the cancel happens at the
  MCP layer, not the worker-process layer.
