# R2 — Real claude root lead adapts after worker is killed-with-reason

## What it proves

A real `claude-haiku-4-5` model session, given a prompt that asks it to spawn
a worker and wait for results, has a worker cancelled mid-flight by an external
operator with a corrective reason. The dispatcher's `cancel_actor_with_reason`
mechanism fires and delivers a synthetic `[SYSTEM]` reprompt to the lead's
layer, which is confirmed via the tracing log.

Specifically this validates:

- Real claude (root lead) calls `spawn_worker` when given an appropriate prompt.
- `cancel_worker` with `reason=...` triggers `cancel_actor_with_reason`, which
  calls `LayerState::send_synthetic_reprompt` on the root layer.
- The reason text appears in the `info`-level tracing log, confirming the
  routing primitive worked end-to-end through the MCP bridge.
- The run completes with exit code 0 even when a worker is cancelled mid-flight.

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

4. Run the Rust integration test (requires `--ignored` + `PITBOSS_DOGFOOD_REAL=1`):
   ```
   PITBOSS_DOGFOOD_REAL=1 cargo test --test dogfood_real_flows real_kill_with_reason -- --ignored
   ```

## Cost estimate

~$0.10-$0.20 per run (haiku model, lead + worker spawn + reprompt round-trip).

## Orchestration pattern

R2 uses the **subprocess-driven dispatch** pattern (same as R1):

1. `pitboss dispatch` runs as a child process with `--run-dir <tmpdir>` and
   `XDG_RUNTIME_DIR` unset, so the MCP socket appears at
   `<tmpdir>/<run_id>/mcp.sock`.
2. A concurrent test task polls `<tmpdir>` for the run subdirectory and socket
   to appear.
3. Once the socket is live, a `FakeMcpClient` connects and polls `list_workers`
   until real claude spawns a worker.
4. The test calls `cancel_worker` with `target=<worker_id>` and
   `reason="use CSV format instead of JSON"`.
5. The dispatcher processes the cancel, calls `cancel_actor_with_reason`, and
   logs the synthetic reprompt at `info` level.
6. The test waits for `pitboss dispatch` to exit with code 0.

## Caveats: reprompt delivery is currently a stub

`LayerState::send_synthetic_reprompt` logs the reason at `info` level but does
**not** inject it into the running claude session. Real session wiring (claude
`--resume` with a prepended system message) is deferred to a future task. As a
result, R2 validates the kill-with-reason *routing mechanism*, not that real
claude's model output reflects the reason.

The "lead adapts" assertion (checking that the final message mentions "CSV",
"format", etc.) is intentionally omitted until session delivery is implemented.
What R2 asserts is:

- `pitboss dispatch` exits 0.
- The reason keyword appears in the tracing log, confirming `cancel_actor_with_reason`
  routed the reason to the root layer.
- At least one worker appeared before the cancel (confirming real claude called
  `spawn_worker`).
