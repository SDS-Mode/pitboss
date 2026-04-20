# Threat model

This page frames pitboss's attack surface honestly. It is aimed at operators evaluating whether pitboss fits a security-sensitive deployment, and at leads designed to process external content.

---

## What pitboss is

Pitboss is an **orchestrator**. It:

- Spawns `claude` subprocesses, one per worker or lead, with a specific prompt and tool set.
- Captures their stream-JSON output, persists structured artifacts per run, and exposes a small MCP socket on a Unix domain socket.
- In hierarchical mode, lets a lead dynamically spawn additional workers and sub-leads at runtime.

That is the complete list. Pitboss is not a sandbox, not a content filter, not an identity provider.

---

## What pitboss is NOT

**Not a runtime jail.** If a worker is given `Bash` in its `tools` list, it can run arbitrary shell commands as the OS user that launched pitboss. Pitboss does not interpose on subprocess execution, does not apply seccomp profiles, and does not restrict filesystem access beyond what the OS already enforces.

**Not an auth/identity provider.** The MCP socket is unauthenticated. Pitboss assumes the only process connecting to the MCP socket is the `claude` subprocess it spawned. There is no per-request credential, no session token, no verification that the connecting client is the expected worker. Do not expose the MCP socket to other processes.

**Not a content filter.** Pitboss does not inspect what a worker reads, what it writes, or what it outputs. If a worker's `Bash` call exfiltrates data to an external endpoint, pitboss will faithfully log the command in `stdout.log` after the fact — it will not prevent it.

**Not an egress firewall.** Pitboss makes no network-level restrictions on what the host or workers can contact. Workers with `Bash` or `WebFetch` can reach any endpoint reachable from the host.

---

## Risks specific to LLM-orchestrated work

### Prompt injection

A worker that reads external content — web pages, user-submitted documents, output from a previous worker that itself processed untrusted input — is exposed to prompt injection. Malicious content in that input can manipulate the worker's subsequent behavior.

The severity depends on the worker's tool set:

- **Read-only tools only** — an injected instruction can cause the worker to produce a misleading report. The damage is informational.
- **Write or Edit tools** — an injected instruction can cause the worker to modify files on the operator's filesystem.
- **Bash** — an injected instruction can cause the worker to run arbitrary shell commands. There is no pitboss-level defense against this. Mitigation is tool restriction: workers that process untrusted input should not have `Bash`.

Pitboss does not prevent prompt injection. The mitigation available to operators is scoping tool permissions so that a successfully injected worker cannot take state-changing actions. See [The Rule of Two](./rule-of-two.md) for the framework and [Defense-in-depth patterns](./defense-in-depth.md) for concrete manifest recipes.

### Runaway cost

A misbehaving lead — whether from a model error, a prompt-injected instruction, or a bug in the lead's own prompt — can spawn workers continuously. The `budget_usd` and `max_workers` fields on `[run]` and per-sublead envelopes are the primary defense. Without them, cost is unbounded. The `budget_usd` cap is enforced via reservation accounting: `spawn_worker` fails before launch once `spent + reserved + next_estimate > budget`.

### Capability escalation through chained tools

A worker with `Read` and `Write` can be tricked into discovering secrets in one location and writing them to another. A worker that reads `~/.ssh/id_rsa` and writes to a world-readable output directory has effectively exfiltrated a key. The worker does not need `Bash` to do this — `Read` + `Write` is sufficient.

Tool restrictions should be designed with the worst-case injected instruction in mind, not the happy-path prompt.

### Sensitive data exposure through observability paths

Workers emit stream-JSON to stdout. The TUI renders live log output per worker. If a worker's output contains sensitive content (credentials it discovered, PII from files it read), that content may appear in:

- `tasks/<id>/stdout.log` in the run directory
- The TUI's tile grid and log pane
- Any webhook notification payloads if `notifications` are configured with `include_output`

Token and cost data is reported in stream-JSON for every worker. Operators should treat run directories as potentially sensitive artifacts.

### Plan-to-action drift

When `require_plan_approval = true`, a lead must have a `propose_plan` approved before `spawn_worker` calls are permitted. However, the approval gates the plan text, not every subsequent action. The lead can behave differently when actually spawning workers than it described in the plan. Approval is a checkpoint, not a binding constraint. Operators who need tighter control over individual spawns should use `request_approval` calls before significant actions, not just `propose_plan`.

---

## What is in your trust boundary

The following are inside your trust boundary as operator:

- The `claude` binary and Anthropic's API. Pitboss trusts the output of `claude` subprocesses to be honest (not itself adversarial).
- The host you run pitboss on, including the filesystem, environment variables, and network stack.
- The manifest you write. Pitboss executes it as specified; it does not attempt to validate that your prompts are safe.
- Any HTTP endpoints configured in `[notifications]`. Pitboss will POST to them; ensure they are trusted and require no authentication that you'd rather not expose in the manifest.

---

## What pitboss does not provide (operator responsibilities)

| Gap | Operator action |
|-----|-----------------|
| Egress filtering | Firewall the host. Pitboss workers have full network access if `Bash` or `WebFetch` are allowed. |
| Per-tool-invocation audit log | Pitboss produces one `TaskRecord` per worker (in `summary.jsonl`), not a per-tool-call log. If you need a record of every `Bash` invocation, you need a wrapper or a Claude-level audit hook. |
| Argument validation on tool calls | `--allowedTools` restricts which tools a worker may call, but not the arguments. A worker with `Write` can write to any path writable by the pitboss process user. |
| Secrets management | Do not put API keys or credentials in the manifest. Use env vars in `[defaults].env` or source them from the environment. The manifest is written verbatim to `manifest.snapshot.toml` in the run directory. |
| Identity / multi-tenancy | Pitboss assumes the operator is the only authenticated user. The MCP socket, TUI, and approval queue have no per-user access control. Multi-tenant deployments require an external wrapper. |

---

**Next:** [The Rule of Two](./rule-of-two.md) — a framework for scoping worker tool permissions based on what each worker processes and touches.
