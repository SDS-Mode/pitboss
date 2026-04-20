# The Rule of Two

The Rule of Two is a recognized pattern in AI agent design: **an agent should hold AT MOST TWO of the following three properties at once**:

- **(A) Untrusted input** — the agent reads content that was not authored by the operator.
- **(B) Sensitive data access** — the agent can read secrets, customer PII, internal-only documents, or anything the operator would not post publicly.
- **(C) State-changing actions** — the agent can take actions that modify state outside its own conversation: writing files, running shell commands, calling external APIs, sending notifications.

Each pair has known failure modes. All three together is unsafe by default.

This page applies the Rule of Two to pitboss manifest design.

---

## Defining the terms in pitboss context

**Untrusted input** is anything a worker reads that the operator did not author:
- External documents or web pages retrieved via `WebFetch`
- User-submitted content passed via the prompt or a shared-store entry
- The output of another worker that itself processed untrusted input (injection can propagate through worker chains)
- Files in a repository where external contributors have write access

**Sensitive data** is any information that should not be publicly visible:
- Credentials and API keys (even if stored as env vars, a worker with `Read` could discover them if they're also present in files)
- Customer or user PII
- Internal architecture documents, unreleased roadmap data, proprietary source code
- Anything the operator would not include in a public bug report

**State-changing actions** in pitboss correspond to specific tool grants:
- `Write`, `Edit`, `NotebookEdit` — modify files on the operator's filesystem
- `Bash` — run arbitrary shell commands (the broadest capability; subsumes most others)
- Custom MCP tools that trigger external side effects (deploy pipelines, notification endpoints, databases)

Tool grants are set via the `tools = [...]` field on `[defaults]`, `[[task]]`, `[[lead]]`, or in the `spawn_worker` call's `tools` argument.

---

## The three permitted pairs

| Pair | What it means | Known failure mode |
|------|---------------|--------------------|
| **A + C** (untrusted input + state-changing, no sensitive data) | Worker processes external content and can write output, but has no access to sensitive data. | Injected instructions can corrupt output files or trigger external actions; they cannot read secrets. |
| **B + C** (sensitive data + state-changing, no untrusted input) | Worker touches internal data and can act on it, but reads only operator-authored prompts and trusted internal data. | Bugs or model errors can cause incorrect mutations; external injection is not a path because the worker never reads untrusted content. |
| **A + B** (untrusted input + sensitive data, no state-changing) | Worker reads external content alongside internal data but cannot write or act. | Can produce misleading reports if injected; cannot modify state. This is the lead-as-evaluator pattern. |

### A + C: untrusted input plus state-changing actions

Use this pair for workers that process external sources and write their output somewhere, but that do not need access to sensitive internal data.

```toml
[[task]]
id        = "scrape-and-summarize"
directory = "/output/public"
tools     = ["WebFetch", "Read", "Write", "Glob"]
prompt    = """
Fetch the URLs in urls.txt. Write a summary of each to summaries/.
Do not read files outside this directory.
"""
```

The worker can be injected, but an injected instruction can only write to the output directory and fetch URLs. It cannot read `~/.ssh/`, env vars, or any file not under `/output/public`. Limit `directory` tightly and restrict `Read` to paths within it.

### B + C: sensitive data plus state-changing actions

Use this pair for workers that act on internal data only — your own repository, your own credentials (passed via `env`), your own infrastructure. These workers must never read untrusted external content.

```toml
[[task]]
id        = "apply-refactor"
directory = "/internal/repo"
tools     = ["Read", "Write", "Edit", "Glob", "Grep"]
prompt    = """
Apply the refactor described in /internal/repo/PLAN.md.
Do not fetch external URLs or read paths outside this repository.
"""
```

There is no `WebFetch` or `Bash` here. The prompt is fully operator-authored. There is no external input path for injection. The risk is model error, not injection — which is mitigated by review (plan approval, approval-gated writes) rather than tool restriction.

### A + B: untrusted input plus sensitive data (no state-changing)

This is the **lead-as-evaluator pattern**. A read-only lead (or worker) ingests external content and internal context simultaneously, but cannot act. Its output is a report or recommendation — the operator (or a separate write-capable worker) decides whether to act on it.

```toml
[[lead]]
id        = "evaluator"
directory = "/internal/repo"
tools     = ["Read", "Glob", "Grep"]
prompt    = """
Read the user-submitted PR description in /tmp/pr-body.txt.
Read the affected source files in this repo.
Produce a structured review report to stdout.
Do not spawn workers that have Write or Bash.
"""
```

No `Write`, `Edit`, or `Bash`. An injected instruction in `pr-body.txt` can only affect what the report says — it cannot modify the repository.

---

## Wiring the Rule of Two in a pitboss manifest

### Tool restrictions per worker

Set `tools` at `[defaults]` for a baseline, then tighten per-task or per-lead:

```toml
[defaults]
# Baseline: read-only
tools = ["Read", "Glob", "Grep"]

[[lead]]
id    = "root"
# The root lead only needs to read and coordinate
tools = ["Read", "Glob", "Grep"]

# Spawn workers with expanded tools only when explicitly needed:
# spawn_worker(prompt = "...", tools = ["Read", "Write", "Edit"])
```

The `tools` argument on `spawn_worker` overrides the per-task default for that worker. See [Manifest schema](../operator-guide/manifest-schema.md) for the field reference.

### Sub-leads as isolation envelopes

A sub-lead's `budget_usd` and `max_workers` cap what the sub-tree can consume, but isolation also comes from the KV layer boundary and `read_down = false`. Sub-leads with different Rule-of-Two profiles should not have `read_down = true` pointing at each other.

```toml
# Untrusted-input sub-tree: small budget, no read-down into trusted tree
spawn_sublead(
  prompt    = "Process the user-submitted documents in /tmp/uploads/...",
  budget_usd = 0.50,
  max_workers = 2,
  read_down  = false
)
```

### Approval policy as a gate on state-changing tools

Use `[[approval_policy]]` to require operator review before any state-changing tool invocation. This does not prevent state changes — it gates them on explicit approval:

```toml
# Auto-approve reads; block anything that writes or runs commands
[[approval_policy]]
match  = { category = "tool_use", tool_name = "Read" }
action = "auto_approve"

[[approval_policy]]
match  = { category = "tool_use", tool_name = "Grep" }
action = "auto_approve"

[[approval_policy]]
match  = { category = "tool_use" }
action = "block"
```

See [Defense-in-depth patterns → Approval-gated state-changing tools](./defense-in-depth.md#approval-gated-state-changing-tools) for a complete example.

### The lead-as-evaluator pattern in detail

The lead-as-evaluator splits the A+B pair from the C property by using separate actors:

1. **Evaluator lead** — holds A+B, no C. Reads external content + internal data, produces a structured plan. Tool set: `["Read", "Glob", "Grep"]`.
2. **Action worker** — holds B+C, no A. Receives only the evaluator's plan (operator-reviewed, operator-authored at the point of handoff). Tool set: `["Read", "Write", "Edit"]` or `["Bash"]`.

The handoff goes through the operator approval queue. The evaluator's `propose_plan` output is reviewed before any write-capable worker is spawned. See [Defense-in-depth patterns](./defense-in-depth.md) for a runnable manifest.

---

## Common violations and their consequences

| Violation | Consequence |
|-----------|-------------|
| Giving `Bash` to a worker that reads user-submitted content | Arbitrary shell execution on the host if the content contains an injected instruction |
| Passing secrets via the prompt or `/ref/*` to a worker that reads external URLs | Secrets exfiltrated if the worker is injected |
| Running a depth-2 sub-lead with `read_down = true` that also processes untrusted input | Root KV contents visible to an injected sub-tree |
| Not setting `budget_usd` on a hierarchical run that could receive externally-triggered work | Unbounded cost if the lead is manipulated into spawning continuously |

---

**See also:**
- [Threat model](./threat-model.md) — what pitboss does and does not defend against
- [Defense-in-depth patterns](./defense-in-depth.md) — runnable manifest recipes for each of these patterns
- [Manifest schema → `tools`](../operator-guide/manifest-schema.md) — `tools` field reference
- [Approvals](../operator-guide/approvals.md) — `[[approval_policy]]` reference
