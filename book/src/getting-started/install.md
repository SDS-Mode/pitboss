# Install

Pitboss ships three binaries: `pitboss` (the CLI dispatcher), `pitboss-tui` (the terminal UI), and `pitboss-web` (the web console + SSE event stream). Install whichever subset you need — `pitboss` is the only hard requirement; the two consoles are optional read-side views over the same run-artifact directory.

## Via shell installer (recommended)

Releases are built with [cargo-dist](https://github.com/astral-sh/cargo-dist) and include `curl | sh` installers. Each installer detects your platform, downloads the matching tarball, verifies its SHA-256, and drops the binary into `~/.cargo/bin`.

```bash
curl -LsSf https://github.com/SDS-Mode/pitboss/releases/latest/download/pitboss-cli-installer.sh | sh
curl -LsSf https://github.com/SDS-Mode/pitboss/releases/latest/download/pitboss-tui-installer.sh | sh
curl -LsSf https://github.com/SDS-Mode/pitboss/releases/latest/download/pitboss-web-installer.sh | sh

pitboss version
pitboss-tui --version
pitboss-web --version
```

**Supported targets:** `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`.

## Via Homebrew

```bash
brew install SDS-Mode/pitboss/pitboss-cli
brew install SDS-Mode/pitboss/pitboss-tui
brew install SDS-Mode/pitboss/pitboss-web
```

Formulae are auto-published to the [SDS-Mode/homebrew-pitboss](https://github.com/SDS-Mode/homebrew-pitboss) tap on every release.

## Via container image

Published to GitHub Container Registry on every push to `main` and every release tag (`linux/amd64` + `linux/arm64`):

```bash
podman pull ghcr.io/sds-mode/pitboss:latest

# Validate a manifest inside the container
podman run --rm -v $(pwd)/pitboss.toml:/run/pitboss.toml \
    ghcr.io/sds-mode/pitboss:latest \
    pitboss validate /run/pitboss.toml
```

> **Note:** The image includes `git` (needed for worktree isolation) but does **not** include the `claude` binary. Mount your host's Claude Code install or build a derived image that layers it in.

## Direct tarball download

```bash
curl -L https://github.com/SDS-Mode/pitboss/releases/latest/download/pitboss-cli-x86_64-unknown-linux-gnu.tar.xz \
  | tar xJ -C ~/.local/bin
```

Tarballs and SHA-256 checksums are attached to every [GitHub release](https://github.com/SDS-Mode/pitboss/releases/latest).

## From source

```bash
git clone https://github.com/SDS-Mode/pitboss.git
cd pitboss
cargo install --path crates/pitboss-cli
cargo install --path crates/pitboss-tui
# pitboss-web bundles an embedded SvelteKit SPA — build it before installing:
(cd crates/pitboss-web/spa && npm ci && npm run build)
cargo install --path crates/pitboss-web
```

## Shell completions

`pitboss` and `pitboss-tui` emit completion scripts (`pitboss-web` is a server binary and doesn't):

```bash
# bash
pitboss completions bash     > ~/.local/share/bash-completion/completions/pitboss
pitboss-tui completions bash > ~/.local/share/bash-completion/completions/pitboss-tui

# zsh (adjust for your $fpath)
pitboss completions zsh      > ~/.zsh/completions/_pitboss
pitboss-tui completions zsh  > ~/.zsh/completions/_pitboss-tui
```

Fish, elvish, and powershell are also supported.

## Prerequisites

- **`claude` CLI** — pitboss is a dispatcher on top of Claude Code. Install it from [claude.ai/code](https://claude.ai/code) and authenticate normally. No `ANTHROPIC_API_KEY` required on Claude Code login systems.
- **Git** — required for worktree isolation (the default). Every task runs in its own `git worktree` on its own branch. Set `use_worktree = false` to skip this for read-only analysis runs.

## Next step

→ [Your first dispatch (flat mode)](./first-dispatch.md)
