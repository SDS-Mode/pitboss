# Running Pitboss with docker-compose

Compose files for the common deployment shapes. All examples work with
`podman compose` or `docker compose` unchanged — the files below use
plain Compose v2 syntax with no Docker-specific extensions.

If you haven't yet: pull the image once.

```bash
podman pull ghcr.io/sds-mode/pitboss:latest
```

## Shared prerequisites

- **Host auth:** `claude login` has been run on the host at least once,
  so `~/.claude/.credentials.json` exists. This file gets bind-mounted
  into every example.
- **Host `claude` binary:** until the `pitboss-with-claude` variant
  ships (v0.7+ — see [Using Claude in a container](./using-claude-in-container.md)
  once PR2 lands), operators using the bare `pitboss` image also mount
  their host's `claude` binary. Find it with:

  ```bash
  which claude
  # typical: /usr/local/bin/claude  (npm global)
  #          ~/.claude/local/claude-bundle/claude  (Anthropic installer)
  ```

  The examples assume `/usr/local/bin/claude`. Adjust if yours differs.
- **SELinux hosts** (Fedora/RHEL/Rocky) need `:z` on bind mounts — the
  examples include it. It's a no-op on Ubuntu/Debian.
- **UID alignment:** set `UID`/`GID` env vars before running compose so
  the container process matches your host user and mounted files stay
  writable:

  ```bash
  export UID=$(id -u)
  export GID=$(id -g)
  ```

## Example 1 — One-shot headless dispatch

Fires off a dispatch and exits. Good for CI, cron jobs, "run this
manifest against this repo and email me when done" scripts.

Project layout:

```
my-project/
├── docker-compose.yml
├── manifest.toml
├── repo/              # your target git repo
└── runs/              # created on first run; pitboss writes here
```

`docker-compose.yml`:

```yaml
services:
  pitboss:
    image: ghcr.io/sds-mode/pitboss:latest
    user: "${UID:-1000}:${GID:-1000}"
    working_dir: /workspace
    command: pitboss dispatch /run/pitboss.toml
    volumes:
      # Host auth (OAuth tokens). Read-write: claude rotates tokens.
      - ${HOME}/.claude:/home/pitboss/.claude:rw,z

      # Host claude binary. Remove once pitboss-with-claude ships.
      - /usr/local/bin/claude:/usr/local/bin/claude:ro

      # Manifest + target repo + run-output dir.
      - ./manifest.toml:/run/pitboss.toml:ro
      - ./repo:/workspace:rw,z
      - ./runs:/home/pitboss/.local/share/pitboss:rw,z
```

Run it:

```bash
podman compose up            # stream logs, exit when done
podman compose up --abort-on-container-exit  # if using docker compose
```

Inspect the run afterward:

```bash
ls runs/                     # one directory per run-id
cat runs/<run-id>/summary.json | jq
```

## Example 2 — Long-running dispatch + TUI attached

Use when you want the TUI's live floor view while a hierarchical run
is in flight. Two services share the run-state directory; the TUI runs
attached to a TTY.

`docker-compose.yml`:

```yaml
x-pitboss-env: &pitboss-env
  user: "${UID:-1000}:${GID:-1000}"
  working_dir: /workspace

services:
  dispatch:
    <<: *pitboss-env
    image: ghcr.io/sds-mode/pitboss:latest
    command: pitboss dispatch /run/pitboss.toml
    volumes:
      - ${HOME}/.claude:/home/pitboss/.claude:rw,z
      - /usr/local/bin/claude:/usr/local/bin/claude:ro
      - ./manifest.toml:/run/pitboss.toml:ro
      - ./repo:/workspace:rw,z
      - pitboss-runs:/home/pitboss/.local/share/pitboss

  tui:
    <<: *pitboss-env
    image: ghcr.io/sds-mode/pitboss:latest
    command: pitboss-tui
    tty: true
    stdin_open: true
    depends_on:
      - dispatch
    volumes:
      - pitboss-runs:/home/pitboss/.local/share/pitboss:rw

volumes:
  pitboss-runs:
```

Run with:

```bash
podman compose up -d dispatch       # start dispatch in background
podman compose run --rm tui         # attach TUI to a TTY
```

The TUI process exits when you `q`. Dispatch keeps running in the
background. `podman compose down` when the dispatch finishes (or
before to cancel).

**Shared volume note:** `pitboss-runs` is a named volume rather than a
host bind mount so both services see the same state dir without
SELinux label juggling. If you want the runs on the host filesystem,
swap it for `./runs:/home/pitboss/.local/share/pitboss:rw,z` in both
services.

## Example 3 — Headless dispatch with webhook notifications

Same as Example 1, but the manifest is wired to fire a Slack webhook
when an approval is pending or the run finishes. Useful for
long-running batch work where you want the run to continue autonomously
but still get poked when it ends or needs you.

`manifest.toml`:

```toml
[run]
max_workers = 6
budget_usd = 2.00
lead_timeout_secs = 3600
approval_policy = "block"

[[notification]]
type = "slack"
url = "${SLACK_WEBHOOK_URL}"
events = ["approval_pending", "run_finished"]
severity_min = "info"

[[lead]]
id = "main"
directory = "/workspace"
prompt = "..."
```

`docker-compose.yml`:

```yaml
services:
  pitboss:
    image: ghcr.io/sds-mode/pitboss:latest
    user: "${UID:-1000}:${GID:-1000}"
    working_dir: /workspace
    command: pitboss dispatch /run/pitboss.toml
    environment:
      SLACK_WEBHOOK_URL: ${SLACK_WEBHOOK_URL}
    volumes:
      - ${HOME}/.claude:/home/pitboss/.claude:rw,z
      - /usr/local/bin/claude:/usr/local/bin/claude:ro
      - ./manifest.toml:/run/pitboss.toml:ro
      - ./repo:/workspace:rw,z
      - ./runs:/home/pitboss/.local/share/pitboss:rw,z
```

Run with the webhook URL in the shell environment:

```bash
export SLACK_WEBHOOK_URL="https://hooks.slack.com/services/..."
podman compose up
```

The `${VAR}` substitution in `manifest.toml` is done by pitboss itself
at dispatch-time, so the env var flows: shell → compose `environment:`
→ container env → pitboss → manifest.

## Example 4 — pitboss-with-claude variant (v0.7+)

Once the bundled variant ships (PR2 of the 2-PR sequence adding
`ghcr.io/sds-mode/pitboss-with-claude`), drop the host-claude bind mount
and switch the image name:

```yaml
services:
  pitboss:
    image: ghcr.io/sds-mode/pitboss-with-claude:latest
    user: "${UID:-1000}:${GID:-1000}"
    working_dir: /workspace
    command: pitboss dispatch /run/pitboss.toml
    volumes:
      - ${HOME}/.claude:/home/pitboss/.claude:rw,z
      # No host-claude mount needed — claude is bundled at a pinned version.
      - ./manifest.toml:/run/pitboss.toml:ro
      - ./repo:/workspace:rw,z
      - ./runs:/home/pitboss/.local/share/pitboss:rw,z
```

## Troubleshooting

**"claude: command not found" inside the container.** The host-binary
mount path doesn't match where your claude is installed. Run
`which claude` on the host and update the `/usr/local/bin/claude`
line in the compose file.

**"Permission denied" reading `.credentials.json`.** UID mismatch
between the container process and the mounted file. Make sure `UID`
and `GID` are exported in your shell before `podman compose up`.

**Worker worktrees fail with "repository is dirty".** The bind mount
at `/workspace` points at a repo with uncommitted changes, and
`use_worktree = true` (the default) wants a clean tree. Either commit
first, or set `use_worktree = false` in `[defaults]` for read-only
analysis runs.

**SELinux AVC denials in the host audit log.** Add `,z` to the bind
mount flags (`./repo:/workspace:rw,z`). The `z` label tells SELinux
this mount is shared across containers/host, applying a compatible
context.

**Rootless podman + `:z` label.** Rootless podman can't write SELinux
labels on directories it doesn't own. Workaround: `chcon -Rt
container_file_t ./runs ./repo` once as a privileged user, or use
named volumes (Example 2's pattern).

## See also

- [Using Claude in a container](./using-claude-in-container.md) (available in v0.7+)
- [Notifications](./notifications.md) — full `[[notification]]` sink reference
- [TUI](./tui.md) — operator-side TUI guide
