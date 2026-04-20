# R2 — Expected observables

## Scenario

The root lead (claude-haiku-4-5) is given a prompt asking it to:
1. Spawn a worker to collect data as JSON
2. Wait for the worker
3. Summarize what happened, including any system messages

While the lead is waiting on the worker, the test acts as operator and calls
`cancel_worker` with `reason="output should be CSV not JSON"`. This injects a
synthetic `[SYSTEM]` reprompt into the lead's active session. The lead's next
turn should reference the kill reason.

## Expected sequence

1. **Dispatch starts**: pitboss launches the root lead process with the MCP
   server configured. The lead receives a prompt asking it to spawn a worker.

2. **Lead calls `spawn_worker`**: The lead spawns one worker with a prompt
   asking for JSON data collection.

3. **Lead calls `wait_actor`**: The lead waits for the worker to complete,
   blocking on the MCP call.

4. **Test calls `cancel_worker` with reason**: Before wait_actor returns, the
   test (as operator) cancels the worker with reason="output should be CSV not
   JSON". This:
   - Terminates the worker's cancel token
   - Injects `[SYSTEM] Actor <id> was killed by operator.\nReason: output
     should be CSV not JSON\nAdjust your plan accordingly.` as a synthetic
     reprompt into the lead's session.

5. **`wait_actor` returns**: The lead's blocked `wait_actor` call returns with
   a terminal result (Cancelled status).

6. **Lead processes the reprompt**: The lead's next assistant turn reads the
   injected `[SYSTEM]` message and adjusts its summary to reference the reason.

7. **Run completes**: The lead emits a final summary that mentions CSV, format
   change, or similar. `pitboss dispatch` exits.

## Test success criteria (when fully implemented)

The test passes when ALL of the following hold:

- `pitboss dispatch` exits with code 0.
- `summary.json` is written with the lead task present.
- The lead's final assistant message (from `final_message_preview` in
  `summary.json` or from `stdout.log`) contains at least one of:
  - `"csv"` (case-insensitive)
  - `"format"` (case-insensitive)
  - `"reason"` (case-insensitive)
  - `"adjust"` (case-insensitive)

## What is NOT asserted

- Exact phrasing of the model's adaptation response.
- That the worker actually ran (workers may be stub no-ops in some builds).
- That `wait_actor` returns a specific status code.
- Token counts or cost.

## Variance notes

Real models exhibit prompt-following variance. The key behavior asserted is that
the kill reason text propagates through the synthetic reprompt mechanism and
influences the model's next response — not that any specific phrasing is used.
