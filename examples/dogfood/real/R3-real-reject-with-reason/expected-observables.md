# R3 — Expected observables

## Scenario

The root lead (claude-haiku-4-5) is given a prompt asking it to:
1. Call `request_approval` with summary="Write results as JSON to output.json"
2. If approved: write a JSON file
3. If rejected: produce CSV output to stdout and explicitly state it was rejected

The manifest uses `approval_policy = "auto_reject"`, so the tool response is
immediately `{ approved: false, comment: "auto-rejected by policy" }`.

## Expected sequence

1. **Dispatch starts**: pitboss launches the root lead process with the MCP
   server configured. `approval_policy = "auto_reject"` is active.

2. **Lead calls `request_approval`**: The lead calls the tool with
   `summary="Write results as JSON to output.json"`.

3. **Auto-rejection fires**: Because `approval_policy = "auto_reject"`, pitboss
   immediately returns `{ "approved": false, "comment": "auto-rejected by policy" }`
   to the lead's tool call. No operator interaction is required.

4. **Lead adapts**: The lead reads the rejection response and, following the
   prompt instructions, produces CSV output to stdout and states that approval
   was rejected.

5. **Run completes**: The lead exits 0. `pitboss dispatch` writes `summary.json`
   with `status = "Success"` for the lead task.

## Test success criteria

The test passes when ALL of the following hold:

- `pitboss dispatch` exits with code 0.
- `summary.json` is written with `status = "Success"` for the lead task.
- The lead's combined output (stdout.log or final_message_preview in summary.json)
  contains at least one of:
  - `"csv"` (case-insensitive) — the model adapted to use CSV format
  - `"reject"` (case-insensitive) — the model acknowledged the rejection
  - `"not approved"` (case-insensitive) — the model understood the denial
  - `"format"` (case-insensitive) — the model mentioned format change
  - `"instead"` (case-insensitive) — the model mentioned switching

## What is NOT asserted

- Exact phrasing of the adaptation response.
- That a CSV file is written (the prompt asks for stdout output).
- That the model uses the exact phrase "auto-rejected by policy" (it reads the
  `approved: false` field, not the comment text).
- Token counts or cost.

## Variance notes

The adaptation assertion is deliberately loose: any mention of "csv", "reject",
"not approved", "format", or "instead" is sufficient. Real models exhibit
prompt-following variance; the key behavior is that the model reads the tool
response and does NOT proceed with the JSON write path.
