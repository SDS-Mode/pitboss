# Defense-in-depth patterns

Each pattern below maps to a specific pitboss feature. For each one: what threat it addresses, a minimal manifest snippet, and what it does not cover.

---

## 1. Read-only lead, write-capable worker

**Addresses:** Prompt injection in the evaluation phase. The lead reads and reasons; workers act only after operator review.

This is the lead-as-evaluator pattern from [The Rule of Two](./rule-of-two.md). The lead holds (A+B) — it may read untrusted content alongside internal data — but has no `Write`, `Edit`, or `Bash`. Workers hold (B+C) but receive only an operator-reviewed plan.

```toml
[run]
max_workers          = 4
budget_usd           = 5.00
require_plan_approval = true

[[lead]]
id        = "evaluator"
directory = "/repo"
tools     = ["Read", "Glob", "Grep"]
prompt    = """
Read the user-submitted spec in /tmp/spec.md and the existing codebase.
Produce a plan via propose_plan listing every file to change and why.
Do not spawn workers until the plan is approved.
"""
```

When the lead calls `propose_plan`, the TUI surfaces it for operator review. Only after the operator approves does `spawn_worker` become permitted. The operator can reject with a reason, and the lead can revise.

Workers are spawned with explicit tool grants at spawn time:

```toml
# Example lead prompt continues:
# spawn_worker(
#   prompt = "Implement the plan. Write only to the paths listed.",
#   tools  = ["Read", "Write", "Edit", "Glob", "Grep"]
# )
```

**What this does not cover:** The operator approves the plan text, not every individual write. A worker that implements the approved plan can still make incorrect edits within that scope. Use per-write `request_approval` calls (pattern 3) if you need individual write approval.

---

## 2. Untrusted input quarantine via sub-leads

**Addresses:** Prompt injection in an externally-sourced sub-task propagating to the rest of the run.

A sub-lead that processes untrusted external content is given a bounded envelope and strict KV isolation. Its workers have no `Write` or `Bash`. Findings return only through `Event::Result` — the root lead reads the result, decides what (if anything) to do with it.

```toml
[run]
max_workers    = 8
budget_usd     = 10.00
lead_timeout_secs = 3600

[[lead]]
id              = "root"
allow_subleads  = true
max_subleads    = 3
directory       = "/repo"
tools           = ["Read", "Glob", "Grep"]
prompt          = """
For each URL in /tmp/urls.txt, spawn a sub-lead to fetch and summarize it.
Use budget_usd = 0.50 and max_workers = 2 per sub-lead.
Set read_down = false on each sub-lead.
After all sub-leads finish, read their terminal results and produce a
combined report. Do not pass any sub-lead's raw output directly to another.
"""

[lead.sublead_defaults]
budget_usd        = 0.50
max_workers       = 2
lead_timeout_secs = 300
read_down         = false
```

Each sub-lead spawned for external URLs gets:
- `budget_usd = 0.50` — cost cap per external document
- `read_down = false` — the root lead cannot see the sub-lead's KV store, so a sub-lead cannot smuggle injected data into `/ref/*` that root then acts on
- Workers with read-only tools only (configured by the sub-lead's own prompt)

The sub-lead's workers might have:

```toml
# Sub-lead spawns workers like:
# spawn_worker(
#   prompt = "Fetch the URL and write a 3-bullet summary. Nothing else.",
#   tools  = ["WebFetch", "Read"]
# )
```

**What this does not cover:** The sub-lead's `Event::Result` text is itself untrusted output — an injected worker could craft a malicious result. The root lead that reads results is read-only, so injected result content can affect the root's report but not cause write actions directly. Apply pattern 3 or pattern 1 on top if root needs to act on the results.

---

## 3. Approval-gated state-changing tools {#approval-gated-state-changing-tools}

**Addresses:** Unreviewed file writes or shell commands. Every state-changing tool invocation surfaces to the operator before executing.

Use `[[approval_policy]]` to auto-approve cheap read operations and block all writes and shell invocations:

```toml
[run]
max_workers     = 4
budget_usd      = 8.00
approval_policy = "block"

# Auto-approve reads (high volume, low risk)
[[approval_policy]]
match  = { category = "tool_use", tool_name = "Read" }
action = "auto_approve"

[[approval_policy]]
match  = { category = "tool_use", tool_name = "Glob" }
action = "auto_approve"

[[approval_policy]]
match  = { category = "tool_use", tool_name = "Grep" }
action = "auto_approve"

# Block all other tool-use (Write, Edit, Bash, etc.) for operator review
[[approval_policy]]
match  = { category = "tool_use" }
action = "block"
```

Rules are evaluated first-match-wins. `Read`, `Glob`, and `Grep` are auto-approved. Any other tool-use — including `Write`, `Edit`, `Bash`, and any custom MCP tool — blocks for operator review.

**What this does not cover:** `[[approval_policy]]` is not argument-aware. It gates whether `Write` is invoked, not which path the `Write` targets. Combine with tight `directory` scoping and read-only leads (pattern 1) for path-level control.

See [Approvals](../operator-guide/approvals.md) for the full policy model.

---

## 4. Cost firewall via per-sub-lead envelopes

**Addresses:** A prompt-injected sub-tree spawning unbounded workers and consuming unbounded budget.

Each sub-lead spawned for externally-triggered work gets a budget cap enforced at the dispatcher level. Even if the sub-lead is injected with an instruction to spawn 100 workers, the envelope enforces the cap before any worker is launched.

```toml
[run]
max_workers    = 20
budget_usd     = 50.00
lead_timeout_secs = 7200

[[lead]]
id                     = "root"
allow_subleads         = true
max_subleads           = 10
max_sublead_budget_usd = 1.00   # hard cap: no sub-lead can get more than $1
max_workers_across_tree = 16
directory              = "/repo"
prompt                 = """
For each incoming task in /tmp/queue.json, spawn a sub-lead with
budget_usd = 0.50, max_workers = 2, read_down = false.
"""

[lead.sublead_defaults]
budget_usd        = 0.50
max_workers       = 2
lead_timeout_secs = 600
read_down         = false
```

`max_sublead_budget_usd = 1.00` means a root lead cannot grant a sub-lead more than $1.00 even if it tries. The `[lead.sublead_defaults]` sets the default to $0.50. The combination caps per-task cost at $0.50 with a hard ceiling of $1.00.

When a sub-lead hits its `budget_usd` ceiling, `spawn_worker` returns `budget exceeded` and the sub-lead terminates (or handles the error, if its prompt instructs it to). The root lead receives the sub-lead's terminal result and can decide whether to alert.

Configure `[notifications]` with `budget_alert_threshold_pct` to receive a webhook when any actor reaches a configured percentage of its budget.

**What this does not cover:** Budget caps do not prevent a sub-lead from using its full envelope on a single expensive operation. They cap total spend, not per-operation cost.

---

## 5. Run-global lease as serialization gate

**Addresses:** Multiple agents concurrently modifying a sensitive shared resource (a deploy pipeline, a credential store, a shared output file) and corrupting it through interleaved writes.

Require a run-global lease before any agent touches the shared resource. Only one agent at a time holds the lease; the rest wait or fail fast. The lease auto-releases if the holder crashes, so a dead agent cannot hold the lock indefinitely.

```toml
[run]
max_workers  = 8
budget_usd   = 20.00

[[lead]]
id        = "root"
directory = "/deploy"
allow_subleads = true
prompt    = """
For each service in services.txt:
  1. Call run_lease_acquire("deploy.lock", ttl_secs=300) — wait up to 60s.
  2. Perform the deploy steps.
  3. Call run_lease_release(lease_id).
If acquire times out, report the service as skipped and continue.
"""
```

The `ttl_secs = 300` ensures that if the deploy worker crashes mid-deploy, the lease expires after 5 minutes and the next actor can proceed. Do not set `ttl_secs` longer than the maximum acceptable stall duration.

`run_lease_acquire` is the run-global variant. For resources internal to a single sub-tree, use `lease_acquire` instead. See [Leases & coordination](../operator-guide/leases.md) for when to use each.

**What this does not cover:** Leases serialize access but do not validate what the holder does during the lease. A holder that writes corrupt data will not be detected by the lease mechanism. Combine with plan approval (pattern 1) for write validation.

---

## 6. TTL + auto-reject fallback

**Addresses:** An approval that the operator cannot reach (off-hours, disconnected TUI, operational incident) stalling the run indefinitely — and then being approved automatically when the operator reconnects without reviewing the context.

Set a TTL on approval requests. When the TTL expires without a response, `fallback = "auto_reject"` causes the request to be rejected rather than queued for later approval.

```toml
[run]
max_workers     = 4
budget_usd      = 10.00
# Default: block approvals if no TUI connected
approval_policy = "block"
```

The lead's prompt instructs it to pass a TTL on sensitive requests:

```toml
# In the lead's prompt:
# request_approval(
#   summary    = "About to run the deploy script for prod.",
#   timeout_secs = 300,
#   plan = {
#     summary  = "Run deploy.sh in /deploy/prod",
#     risks    = ["Deploys to production; irreversible without rollback procedure"],
#     rollback = "Run deploy.sh --rollback"
#   }
# )
#
# The lead prompt should handle rejection:
# If rejected or timed out, abort this task and report why.
```

To set a run-level fallback on all approval requests, combine the TTL with `[[approval_policy]]`:

```toml
# Block all approvals; set a cost-over firewall for large events
[[approval_policy]]
match  = { category = "cost", cost_over = 1.00 }
action = "block"
```

Operator-side: if you expect off-hours runs where the TUI may be unattended, set `approval_policy = "auto_reject"` in `[run]` as the baseline. Approvals that aren't explicitly auto-approved by a policy rule will reject rather than queue indefinitely.

**What this does not cover:** Auto-reject stops the action but does not roll back work already done. For actions that should be atomic (approve before any work or none), use `propose_plan` with `require_plan_approval = true` before any workers are spawned.

---

## What you still need to provide

These are operator responsibilities that pitboss does not address:

**Egress filtering.** Firewall the host. Workers with `Bash` or `WebFetch` can reach any endpoint reachable from the OS. Pitboss makes no network-level restrictions.

**Secrets handling.** Do not put API keys or credentials in the manifest. Use `[defaults].env` to pass secrets from the environment, or use a secrets manager. The manifest is written verbatim to `manifest.snapshot.toml` in the run directory.

**Per-tool-invocation audit log.** Pitboss produces one `TaskRecord` per worker (`summary.jsonl`), not a per-tool-call log. If you need a record of every `Bash` invocation or every path written to, you need a process-level audit hook or a claude-level wrapper.

**Identity and access control.** Pitboss assumes the operator is the only user. The MCP socket, TUI, and approval queue have no per-user access control. Multi-tenant deployments require an external wrapper.

---

**See also:**
- [Threat model](./threat-model.md) — full list of what pitboss does and does not defend against
- [The Rule of Two](./rule-of-two.md) — framework for deciding which tools each worker should have
- [Approvals](../operator-guide/approvals.md) — full `[[approval_policy]]` reference
- [Leases & coordination](../operator-guide/leases.md) — per-layer vs. run-global leases
- [Depth-2 sub-leads](../operator-guide/depth-2-subleads.md) — sub-lead envelope and isolation model
