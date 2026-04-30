# Using Claude in a container

Pitboss ships a Goose-native container image plus a temporary Claude
compatibility image:

| Image | What's inside | When to use |
|-------|---------------|-------------|
| `ghcr.io/sds-mode/pitboss` | Pitboss binaries only | You are layering pitboss into an existing base image. |
| `ghcr.io/sds-mode/pitboss-with-goose` | Pitboss + pinned Goose CLI | Default for `container-dispatch`. |
| `ghcr.io/sds-mode/pitboss-with-claude` | Pitboss + pinned Claude Code CLI | Legacy compatibility for the pre-Goose Claude Code workflow. |

Both images are multi-arch (`linux/amd64` + `linux/arm64`) and follow the same tag scheme (`:latest`, semver tags, `:main`).

The Claude compatibility image pins a specific Claude Code version. To check it at runtime:

```bash
podman inspect ghcr.io/sds-mode/pitboss-with-claude:latest \
  --format '{{index .Config.Labels "ai.anthropic.claude-code.version"}}'
```

## Linux host: mount `~/.claude`

Claude Code on Linux stores OAuth tokens at `~/.claude/.credentials.json`. The bundled container reads credentials from `/home/pitboss/.claude` (via `CLAUDE_CONFIG_DIR`), so bind-mounting your host's `~/.claude` Just Works:

```bash
# One-time on the host:
claude login

# Every pitboss run:
podman run --rm --userns=keep-id \
  -v "$HOME/.claude:/home/pitboss/.claude:rw,z" \
  -v "$PWD/manifest.toml:/run/pitboss.toml:ro,z" \
  ghcr.io/sds-mode/pitboss-with-claude:latest \
  pitboss dispatch /run/pitboss.toml
```

### Why `--userns=keep-id`?

Rootless podman runs the container in a user namespace. Without `--userns=keep-id`, your host UID 1000 maps to in-container UID 0 (fake root), and the bundled `pitboss` user (container UID 1000) maps to a different host subuid — the mounted credentials look root-owned to the in-container `pitboss` user and become unreadable. `--userns=keep-id` aligns the mapping so host UID 1000 maps directly to container UID 1000.

If you're running Docker instead of rootless podman, skip the flag: Docker doesn't use user namespaces by default, so mounted files' UIDs pass through unchanged. Use `-u "$(id -u):$(id -g)"` there if your host UID isn't 1000.

### Why the `:z` flag?

On SELinux-enforcing distros (Fedora, RHEL, CentOS, Rocky), a bind mount without a label is unreadable from the container. The `:z` flag tells podman/docker to apply a shared SELinux label so the container can read the mount. Ubuntu and Debian operators can omit it.

**Important:** ALL bind mounts need `:z`, not just `~/.claude`. Missing `:z` on the manifest mount is a common footgun — it produces a cryptic `Permission denied (os error 13)` from pitboss at manifest-read time.

## macOS host: Keychain can't be mounted

On macOS, claude stores OAuth tokens in the system Keychain — not in `~/.claude/`. The Keychain isn't mountable into a container. Two fallbacks:

### Option A: API key

If you have a standalone Anthropic API key (pay-as-you-go, separate from a Claude subscription):

```bash
docker run --rm \
  -e ANTHROPIC_API_KEY="$ANTHROPIC_API_KEY" \
  -v "$PWD/manifest.toml:/run/pitboss.toml:ro" \
  ghcr.io/sds-mode/pitboss-with-claude:latest \
  pitboss dispatch /run/pitboss.toml
```

### Option B: Persistent named volume

Run `claude login` inside the container once to authenticate via OAuth, store the result in a named volume, then reuse that volume for subsequent runs:

```bash
# One-time: interactive login inside a persistent volume
docker volume create pitboss-claude-auth
docker run --rm -it \
  -v pitboss-claude-auth:/home/pitboss/.claude \
  ghcr.io/sds-mode/pitboss-with-claude:latest \
  claude login

# Every run:
docker run --rm \
  -v pitboss-claude-auth:/home/pitboss/.claude \
  -v "$PWD/manifest.toml:/run/pitboss.toml:ro" \
  ghcr.io/sds-mode/pitboss-with-claude:latest \
  pitboss dispatch /run/pitboss.toml
```

## Podman vs Docker

`podman run` and `docker run` with the arguments above behave equivalently for pitboss's purposes. Key differences operators hit:

- **Rootless podman** uses user namespaces → needs `--userns=keep-id` (see above).
- **Docker** by default creates iptables rules that bypass UFW on Linux hosts. Podman's `netavark`/`slirp4netns` stack respects the host firewall.
- **SELinux**: both honor the `:z` / `:Z` mount flags identically.

Recommend podman for Linux operators who care about firewall enforcement; Docker is simpler for macOS (Docker Desktop) and Windows (WSL2 backend).

## Updating the bundled Claude version

The bundled image pins a specific Claude Code version in CI. To consume a newer version:

1. Open an issue or PR at https://github.com/SDS-Mode/pitboss to bump `CLAUDE_CODE_VERSION` in `.github/workflows/container.yml`.
2. Once merged, a new container release rebuilds with the updated version.

For local/one-off use with a different version:

```bash
podman build --target=with-claude \
  --build-arg CLAUDE_CODE_VERSION=<version> \
  -t pitboss-with-claude:custom .
```

## Troubleshooting

### "Not logged in" / auth error

Check on the host: `claude --version` should work and `ls ~/.claude/.credentials.json` should exist. If the file is missing, run `claude login` on the host.

### "Permission denied" reading credentials in rootless podman

Add `--userns=keep-id`. Rootless podman's default UID namespace maps host UID to in-container UID 0 — see the "Why `--userns=keep-id`?" section.

### "Permission denied (os error 13)" reading the manifest

The manifest bind mount is missing `:z`. Add it: `-v "$PWD/manifest.toml:/run/pitboss.toml:ro,z"`. All bind mounts on SELinux-enforcing hosts need `:z`, not just `~/.claude`.

### SELinux AVC denials in the host audit log

Same cause as above — bind mounts need `:z` or `:Z`. `:z` applies a shared label (compatible across containers). `:Z` applies a private label (prevents other containers from reading the same mount).

### Token refresh failure after a long-running dispatch

OAuth tokens rotate. If the container started with a valid token that expired mid-run, the refresh write-back needs UID alignment (`--userns=keep-id` on rootless podman, or matching `-u` on Docker). Re-run with the correct flag.
