# Writing effective leads

A lead prompt is the strategic layer of a hierarchical run: it tells Claude what to do *as an orchestrator*, not as a performer. Getting this right is the single highest-leverage thing you can do to improve run reliability. This page covers the patterns that make lead prompts work well, the anti-patterns that don't, and ends with a complete annotated example you can adapt.

---

## What a lead actually does

A lead is a Claude session that receives the orchestration MCP toolset — `spawn_worker`, `wait_actor`, `request_approval`, `kv_set`, and the rest. The *prompt* sets the strategy; the *tools* enact it. The important detail: Claude's default behavior is to do work itself. Given a task and the ability to read files, Claude will read files. You have to actively counter this tendency and push it toward delegation — otherwise you'll get a lead that solves the whole problem in-context and spawns nothing, which defeats the purpose of using hierarchical mode at all.

---

## The decomposition framing

The opening of a lead prompt should explicitly state that the lead's job is coordination, not execution. Left implicit, models default to monolithic execution — they complete the work themselves and never invoke `spawn_worker`.

What good decomposition framing contains:

- **An explicit non-execution instruction.** "Your job is to coordinate. Do NOT do the work yourself."
- **A concrete decomposition heuristic.** The lead needs to know *when* to spawn. Vague instructions ("spawn workers as needed") produce unpredictable behavior. Name the decomposition axis: "one worker per file", "one worker per phase", "one worker per unique dependency".
- **Worker invocation parameters spelled out.** Specify which model, budget, tools, and prompt template each worker should receive. Leaving these open-ended produces inconsistent workers.
- **An explicit wait instruction** naming the tool. Don't assume the lead knows to call `wait_actor`; say "after spawning all workers, call `wait_actor` on each one before proceeding."
- **A summary instruction at the end.** Without it, leads often end with conversational filler rather than a structured result.

**Anti-pattern — too vague:**
```
Audit the security headers for each URL in /tmp/urls.txt. Spawn workers as needed.
```
This prompt doesn't tell the lead when to spawn, what the workers should do, or what to produce. Most models will just process the URLs themselves.

**Better:**
```
Your job is to COORDINATE, not to do the work yourself.
For each URL in /tmp/urls.txt, spawn exactly one worker via spawn_worker.
Workers should use model = "claude-sonnet-4-6", budget = 0.20.
After spawning all workers, call wait_actor on each worker_id before continuing.
When all workers have finished, produce a final report (see "Final report" below).
```

---

## Worker prompt templates

The lead writes worker prompts at runtime. If you don't teach it a template, it will improvise — and inconsistent worker prompts produce inconsistent worker outputs that are hard to aggregate.

Teach the lead a template in the lead prompt itself:

```
Spawn each worker using this exact prompt template (substitute [URL] and [WORKER_N]):

"You are WORKER_N. Fetch [URL] and check the following security headers:
Content-Security-Policy, X-Frame-Options, Strict-Transport-Security.
Produce a report with exactly three sections:
1. FOUND: headers that are present and their values
2. MISSING: headers that are absent
3. RECOMMENDATION: one sentence per missing header
Keep the report under 400 words. Do not include any other content."
```

Why templates matter: free-form worker prompts lead to workers that structure their output differently — some use prose, some use tables, some omit sections entirely. The lead then can't reduce the results cleanly. A template enforces the shape of each worker's output and makes the lead's aggregation step deterministic.

---

## Handling partial results

Workers fail. They time out, hit context limits, or return error messages. A lead without explicit instructions for this case will often stall, retry indefinitely, or crash.

Three patterns — pick one and name it in the prompt:

**Fail-fast:** Any worker failure cancels remaining work and reports aggregate failure. Use when work is sequential or when partial results are useless.

```
If any worker returns an error or times out, do NOT spawn additional workers.
Collect the errors and produce a final report listing which inputs failed and why.
```

**Best-effort:** Collect what completes, note what didn't, exit cleanly. Use when work is independent (e.g., one worker per file — a failed file audit doesn't affect the others).

```
If a worker returns an error, record the failure and continue with the remaining workers.
Do not retry failed workers. In the final report, list which inputs were successfully
processed and which were not, with the error reason for each failure.
```

**Retry-on-failure:** Respawn with a corrected prompt up to N times. Use when failures are typically transient (network, race conditions, intermittent tool errors).

```
If a worker returns an error, inspect the error. If it appears transient (network timeout,
tool unavailable), respawn that worker once with the same prompt. If it fails again,
treat it as a permanent failure and record it as such. Do not retry more than once per input.
```

Name the pattern explicitly in your prompt. Don't rely on the lead to infer the right behavior from context.

---

## Graceful budget exhaustion

Leads spawn workers that consume budget. When `budget_usd` runs low, `spawn_worker` returns a `budget exceeded` error. Without explicit handling instructions, leads either crash on this error or retry the spawn indefinitely — both bad outcomes.

The instruction to add:

```
You have a budget of $X for this run. Before each spawn_worker call, estimate whether
you have enough remaining budget to complete the current worker plus any workers you
still need to spawn. If you don't, stop spawning new workers immediately. Produce a
final report listing what was completed and what was deferred due to budget exhaustion.
```

The same pattern applies to the other resource limits:

- **`max_workers` cap:** "If spawn_worker returns `max_workers exceeded`, wait for a running worker to complete before spawning the next one."
- **`lead_timeout_secs`:** "If you have fewer than 5 minutes remaining on your wall-clock limit, stop spawning and produce your final report with what has completed so far."

Without these instructions, budget and concurrency caps become crash sites rather than graceful stopping points.

---

## The summary instruction

A lead's final assistant message becomes the `final_message_preview` field of the `TaskRecord`. This is what shows up in the TUI after the run, in notifications, and in `summary.jsonl` post-run inspection. Without an explicit instruction to produce a structured summary, leads often end with phrases like "Let me know if you need anything else" — which tells the operator nothing.

Instruct the lead explicitly:

```
When all workers have completed (or budget is exhausted), produce a final report
with the following sections in this order:

1. STATUS: one of "success", "partial", or "failed"
2. COMPLETED: one bullet per worker — worker ID, input, one-sentence result
3. DEFERRED OR FAILED: one bullet per unprocessed input — input and reason
4. COST SUMMARY: list of worker IDs and their approximate spend in USD

Do not include any text after the final report.
```

The structure matters: tools that parse `final_message_preview` (notification webhooks, downstream scripts) need a predictable format. Even if you're just reading it yourself in the TUI, a consistent structure is much easier to scan.

---

## A complete example

The following lead prompt audits HTTP security headers across a list of URLs. It incorporates all the patterns above: explicit non-execution framing, decomposition heuristic, worker template, best-effort failure handling, budget exhaustion guard, and a structured summary.

```toml
[[lead]]
id        = "security-audit"
model     = "claude-haiku-4-5"
directory = "/tmp/audit-workdir"
prompt    = """
Your job is to COORDINATE this security audit. Do NOT fetch any URLs or check
any headers yourself — that is the workers' job.

## Decomposition

Read /tmp/urls.txt. It contains one URL per line.
Spawn exactly one worker per URL via spawn_worker.

Worker parameters:
  model      = "claude-sonnet-4-6"
  budget_usd = 0.20
  tools      = ["WebFetch", "Read"]

Use this exact prompt template for each worker (substitute [URL] and [N]):
---
You are worker [N] auditing [URL].
Fetch the URL's HTTP response headers using WebFetch.
Check for these headers: Content-Security-Policy, X-Frame-Options,
Strict-Transport-Security, X-Content-Type-Options, Referrer-Policy.

Produce a report with exactly three sections:
FOUND: list each present header and its value (one per line)
MISSING: list each absent header (one per line)
RECOMMENDATION: one sentence per missing header explaining why it matters

Keep the report under 400 words. No other content.
---

## Waiting

After spawning all workers, call wait_actor on each worker_id before continuing.

## Failure handling

If a worker returns an error, record the failure and continue with remaining workers.
Do not retry. In the final report, list which URLs were not processed and why.

## Budget exhaustion

Before each spawn_worker call, check your remaining budget. If insufficient for
another worker, stop spawning and proceed to the final report, listing which URLs
were deferred.

## Final report

When all workers have completed or you have exhausted your budget, produce a report
with these sections in order:

1. STATUS: "success", "partial", or "failed"
2. COMPLETED: one bullet per processed URL — URL, finding summary (one sentence)
3. DEFERRED OR FAILED: one bullet per unprocessed URL — URL, reason
4. COST SUMMARY: worker IDs and approximate spend in USD

Do not include any text after the final report.
"""
```

Operators can adapt this template by swapping the decomposition axis (files instead of URLs, phases instead of inputs), adjusting the worker model and budget, and replacing the worker prompt template with task-specific instructions.

---

## See also

- [Approvals](./approvals.md) — for leads that gate actions on operator review before proceeding
- [Defense-in-depth → Read-only lead pattern](../security/defense-in-depth.md) — for leads structured as read-only auditors that delegate writes to approved workers
