# Container dispatch (v0.8+)

`pitboss container-dispatch` runs a dispatch inside a Docker or Podman container
with declarative bind mounts. The container image bundles the pitboss binary and
a pinned Goose CLI; you provide the project directory and any reference
material as host mounts. Pitboss auto-mounts Goose auth/state directories.

## When to use it

- You want isolation from the host filesystem for untrusted or experimental dispatches.
- Your CI/CD pipeline doesn't have Goose installed on the runner.
- You want consistent Goose version pinning across runs.

## Manifest schema

Add a `[container]` section and one or more `[[container.mount]]` entries:

```toml
[container]
image   = "ghcr.io/sds-mode/pitboss-with-goose:latest"  # optional; this is the default
runtime = "podman"   # "docker", "podman", or "auto" (default: auto-detect)
workdir = "/project" # optional; defaults to the first mount's container path

[[container.mount]]
host      = "~/projects/myproject"
container = "/project"
readonly  = false

[[container.mount]]
host      = "~/reference-docs"
container = "/ref"
readonly  = true

# Lead and worker directories are container-side paths:
[[lead]]
id        = "my-lead"
directory = "/project"
prompt    = "..."
```

Task and lead `directory` fields are container-side paths. Pitboss skips the
host-filesystem check for these paths at validation time.

## Auto-injected mounts

These mounts are added automatically unless you declare them yourself:

| Host path | Container path | Notes |
|-----------|----------------|-------|
| `~/.config/goose` | `/home/pitboss/.config/goose` | Goose provider config; read-write |
| `~/.local/share/goose` | `/home/pitboss/.local/share/goose` | Goose sessions/data; read-write |
| `~/.local/state/goose` | `/home/pitboss/.local/state/goose` | Goose state/cache; read-write |
| `~/.claude` | `/home/pitboss/.claude` | Claude ACP pass-through compatibility; read-write |
| `~/.local/share/pitboss/runs` | `/home/pitboss/.local/share/pitboss/runs` | Run artifacts; read-write |

The manifest itself is always injected at `/run/pitboss.toml` (read-only).

## Running

```bash
# Dry-run: print the assembled podman/docker command without launching
pitboss container-dispatch manifest.toml --dry-run

# Live dispatch
pitboss container-dispatch manifest.toml

# Attach TUI after the run starts (in a second terminal)
pitboss-tui
```

The `--runtime` flag overrides auto-detection: `--runtime docker` or
`--runtime podman`.

## UID alignment

- **Rootless podman** — `--userns=keep-id` maps host UID to the same UID inside
  the container, so mounted files have consistent ownership.
- **Docker** — if the host UID differs from the container's `pitboss` user (UID 1000),
  `-u uid:gid` is passed so writes land with the correct host ownership.

## SELinux

On SELinux-enforcing systems, the `:z` suffix on each bind mount relabels the
content for container access. Pitboss adds `:z` automatically alongside `:rw` or
`:ro`.

## Attaching the TUI

The run directory is mounted to the host path, so `pitboss-tui` can attach from the
host exactly as for a native dispatch:

```bash
pitboss-tui                  # open the most recent run
pitboss-tui <run-id-prefix>  # open by prefix
```

## Compatibility with older images

The `[container]` section is stripped from the manifest before it is mounted
into the container. This means the container's `pitboss dispatch` binary —
which may predate v0.8 and not know the `[container]` field — parses the
manifest cleanly. The only version requirement is that the container image
carries a pitboss binary new enough to understand the rest of your manifest.
