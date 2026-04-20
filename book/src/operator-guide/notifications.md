# Notifications

Pitboss can push notifications to external sinks when key run events occur. This is useful for monitoring long-running dispatches from outside the TUI — for example, getting a Slack message when a budget-intensive run finishes.

## Configuration

Add a `[[notification]]` section to your manifest for each sink:

```toml
[[notification]]
type = "slack"
url = "${SLACK_WEBHOOK_URL}"      # env-var substitution supported
events = ["run_finished", "budget_exceeded"]
severity_min = "info"

[[notification]]
type = "discord"
url = "${DISCORD_WEBHOOK_URL}"
events = ["approval_request", "run_finished"]

[[notification]]
type = "webhook"
url = "https://my-server.example.com/pitboss-events"
events = ["approval_request", "budget_exceeded", "run_finished"]
```

## Supported sinks

| Sink | `type` value | Notes |
|------|-------------|-------|
| Generic HTTP POST | `"webhook"` | Sends a JSON payload with the event |
| Slack Incoming Webhook | `"slack"` | Formats as a Slack message block |
| Discord Webhook | `"discord"` | Formats as a Discord embed |
| Log only | `"log"` | Writes to stderr; useful for debugging |

## Events

| Event | When it fires |
|-------|--------------|
| `approval_request` | When an approval is enqueued for operator action (v0.6+) |
| `run_finished` | When the dispatch completes (all tasks settled or cancelled) |
| `budget_exceeded` | When a `spawn_worker` or `spawn_sublead` fails due to budget exhaustion |

## Delivery semantics

- Notifications fire asynchronously via `tokio::spawn` — they don't block the dispatch.
- Failed deliveries are retried up to 3 times with exponential backoff (100ms → 300ms → 900ms).
- An LRU dedup cache (size 64) prevents retry storms for the same event.
- Delivery failures are recorded in `<run-dir>/tasks/<id>/events.jsonl` as `TaskEvent::NotificationFailed`.

## Env-var substitution

URLs support `${VAR_NAME}` substitution from the process environment. This keeps secrets out of manifest files:

```toml
[[notification]]
type = "slack"
url = "${SLACK_WEBHOOK_URL}"
events = ["run_finished"]
```

## Coming soon

- Per-sink `severity_min` filter (`"info"`, `"warn"`, `"error"`)
- `approval_pending` event variant (in the v0.6+ event schema)

For the canonical notification schema reference, see [`AGENTS.md`](https://github.com/SDS-Mode/pitboss/blob/main/AGENTS.md) in the source tree.
