# R3 — Real claude root lead adapts after request_approval is rejected

## What it proves

A real `claude-haiku-4-5` model session, given a prompt that asks it to call
`request_approval` before writing files, receives a rejection response (via the
`auto_reject` approval policy). The model's next turn adapts its plan based on
the rejection, producing CSV output instead of JSON and explicitly referencing
that the approval was rejected.

Specifically this validates:

- `request_approval` is callable by a real claude session via the MCP bridge.
- The tool return value (with `approved: false`) is visible to and understood by
  the real model.
- The model exhibits LLM-adaptive behavior: reading the tool response and
  adjusting its plan rather than ignoring the rejection.

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
   bash examples/dogfood/real/R3-real-reject-with-reason/run.sh
   ```

   Or run the Rust integration test (requires `--ignored` + `PITBOSS_DOGFOOD_REAL=1`):
   ```
   PITBOSS_DOGFOOD_REAL=1 cargo test --test dogfood_real_flows real_reject_with_reason -- --ignored
   ```

## Cost estimate

~$0.10-$0.20 per run (haiku model, prompt + approval tool round-trip + adaptation).

## Approval mechanism

The manifest uses `approval_policy = "auto_reject"`, which causes pitboss to
immediately reject any `request_approval` call with `approved: false` and
`comment: "auto-rejected by policy"`. No human operator is needed — the
rejection fires automatically.

The prompt encodes the adaptation instructions: "if rejected, use CSV instead
of JSON." This tests that the model:
1. Calls `request_approval` as instructed
2. Reads the rejection response
3. Adapts its output format accordingly

## Caveats

- **Reason in prompt, not response**: The `auto_reject` policy returns a generic
  "auto-rejected by policy" comment rather than a custom "use CSV instead" reason.
  The CSV adaptation instruction is embedded in the initial prompt. A production
  R3 would use a real operator sending `ControlOp::Approve { approved: false,
  reason: Some("use CSV instead") }` via the control socket, which the lead
  receives in the `request_approval` response. This simpler form tests the
  same model-adaptive behavior with less test infrastructure.
- **Model variance**: The model may adapt differently on each run. The assertion
  only checks for loose keywords ("csv", "reject", "format", etc.).
- **No file writes**: The lead has `max_workers = 0` and is instructed to output
  to stdout only, keeping the test self-contained.
