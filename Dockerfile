# Pitboss container image.
#
# Multi-stage build: compile the workspace in a rust toolchain image,
# copy just the two release binaries into a slim debian runtime. The
# runtime image carries `git` (pitboss uses git worktrees for task
# isolation), `ca-certificates` (for webhook notifications), and the
# pitboss + pitboss-tui binaries on $PATH. The `with-goose` target adds
# the Goose CLI used for agent dispatch.
#
# Build:   podman build -t pitboss:local .
# Run:     podman run --rm -it -v $(pwd)/manifest.toml:/run/pitboss.toml \
#              pitboss:local pitboss validate /run/pitboss.toml

# --- Stage 1: build ---
FROM rust:1.82-slim-bookworm AS builder

# System packages needed by our transitive deps (git2 vendored libgit2
# wants pkg-config + libssl headers; reqwest wants a TLS stack). The
# rust-slim image is minimal — we install only what the build actually
# needs.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libssl-dev \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy the full workspace. We don't do the usual `cargo chef` layer
# caching dance because the build is fast enough with a volume-mounted
# target dir during development, and CI gets a fresh checkout anyway.
COPY . .

# Build both binaries in one pass. `--locked` ensures we use the
# committed Cargo.lock; the build fails if it would drift.
RUN cargo build --release --locked -p pitboss-cli -p pitboss-tui

# --- Stage 2: runtime ---
FROM debian:stable-slim AS runtime

# Runtime deps:
#   git             — pitboss runs `git worktree add` per task
#   ca-certificates — webhook notifications, potential future HTTPS calls
#   tini            — proper PID 1 so Ctrl-C + signal forwarding work
RUN apt-get update && apt-get install -y --no-install-recommends \
        git \
        ca-certificates \
        tini \
    && rm -rf /var/lib/apt/lists/*

# Non-root user. Pitboss writes runs to $HOME/.local/share/pitboss by
# default; `pitboss` UID 1000 is a sensible host-portable choice.
RUN useradd --create-home --shell /bin/bash --uid 1000 pitboss

COPY --from=builder /build/target/release/pitboss     /usr/local/bin/pitboss
COPY --from=builder /build/target/release/pitboss-tui /usr/local/bin/pitboss-tui

# Bundled agent-facing reference doc. Mirrors `pitboss agents-md` output at
# a standard filesystem path so shell-first agents can discover it without
# invoking the binary. Both routes serve identical content — the binary's
# `AGENTS_MD` const is `include_str!`'d from the same source file.
COPY AGENTS.md /usr/share/doc/pitboss/AGENTS.md

USER pitboss
WORKDIR /home/pitboss

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["pitboss", "--help"]

# --- Stage 3: runtime + Goose CLI ---
#
# Canonical agent runtime image for the Goose provider pivot. Goose owns
# provider auth and model dispatch; pitboss owns orchestration.
FROM runtime AS with-goose

USER root

ARG TARGETARCH=amd64
ARG GOOSE_VERSION=1.33.1
RUN test -n "$GOOSE_VERSION" || (echo "GOOSE_VERSION build arg is required" && exit 1)

RUN apt-get update && apt-get install -y --no-install-recommends \
        curl \
        libgomp1 \
    && rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    case "${TARGETARCH}" in \
        amd64) goose_arch="x86_64" ;; \
        arm64) goose_arch="aarch64" ;; \
        *) echo "unsupported TARGETARCH=${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    tmp="$(mktemp -d)"; \
    curl -fsSL "https://github.com/aaif-goose/goose/releases/download/v${GOOSE_VERSION}/goose-${goose_arch}-unknown-linux-gnu.tar.gz" \
        -o "$tmp/goose.tar.gz"; \
    tar -xzf "$tmp/goose.tar.gz" -C "$tmp"; \
    goose_bin="$(find "$tmp" -type f -name goose | head -n 1)"; \
    test -n "$goose_bin"; \
    install -m 0755 "$goose_bin" /usr/local/bin/goose; \
    rm -rf "$tmp"; \
    goose --version

RUN mkdir -p /usr/share/doc/goose && \
    printf '%s\n' \
      'This image bundles aaif-goose/goose (Apache-2.0).' \
      'Source: https://github.com/aaif-goose/goose' \
      "Version: ${GOOSE_VERSION}" \
      > /usr/share/doc/goose/ATTRIBUTION

USER pitboss

LABEL ai.aaif.goose.version="${GOOSE_VERSION}"

# Entrypoint and default CMD are inherited from `runtime`.

# --- Stage 2b (intermediate): Node.js 20 source ---
#
# We pull Node.js 20 from the official upstream image and COPY it into
# the `with-claude` stage below. This is more reproducible than piping
# NodeSource's setup script to bash — that script is remote and mutable
# and its content can drift between builds of the same
# CLAUDE_CODE_VERSION. The official `node:20-bookworm-slim` image is
# pinned by tag and cached by buildx.
FROM node:20-bookworm-slim AS node

# --- Stage 3: runtime + Claude Code CLI (opt-in variant) ---
#
# Superset of the `runtime` stage: adds Node.js 20 (copied from the
# `node` stage above) and the pinned Claude Code CLI. Config is expected
# via a host bind-mount of `~/.claude` at /home/pitboss/.claude (see
# book/src/operator-guide/using-claude-in-container.md for the run
# pattern and UID alignment details).
FROM runtime AS with-claude

USER root

# CLAUDE_CODE_VERSION is required. A sensible default makes local
# `podman build --target=with-claude .` work; CI always passes
# --build-arg to match the workflow-level pin.
ARG CLAUDE_CODE_VERSION=2.1.114
RUN test -n "$CLAUDE_CODE_VERSION" || (echo "CLAUDE_CODE_VERSION build arg is required" && exit 1)

# Node + npm: only the node binary and the npm package tree are copied
# from the official image. npm and npx are then re-symlinked into
# /usr/local/bin/ to point at their canonical entry scripts inside
# node_modules — COPY-ing /usr/local/bin/npm directly would dereference
# the symlink and break npm's relative `require('../lib/cli.js')`.
COPY --from=node /usr/local/bin/node              /usr/local/bin/node
COPY --from=node /usr/local/lib/node_modules/npm  /usr/local/lib/node_modules/npm
RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && ln -s /usr/local/lib/node_modules/npm/bin/npx-cli.js /usr/local/bin/npx

ENV NPM_CONFIG_PREFIX=/usr/local/share/npm-global
ENV PATH=$PATH:/usr/local/share/npm-global/bin

RUN mkdir -p ${NPM_CONFIG_PREFIX} \
    && npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION} \
    && npm cache clean --force \
    && rm -rf /root/.npm \
    && chown -R pitboss:pitboss ${NPM_CONFIG_PREFIX}

# Attribution for the bundled Anthropic artifact. Not legally required,
# but a posture-strengthener and a breadcrumb for operators who
# introspect the image.
RUN mkdir -p /usr/share/doc/claude-code && \
    printf '%s\n' \
      'This image bundles @anthropic-ai/claude-code (installed via npm).' \
      'Copyright (c) Anthropic PBC. All rights reserved.' \
      'Use is subject to Anthropic'\''s Commercial Terms of Service:' \
      '  https://www.anthropic.com/legal/commercial-terms' \
      'Pitboss bundles this package for operator convenience only and' \
      'makes no representations on behalf of Anthropic PBC.' \
      > /usr/share/doc/claude-code/ATTRIBUTION

USER pitboss
ENV CLAUDE_CONFIG_DIR=/home/pitboss/.claude

LABEL ai.anthropic.claude-code.version="${CLAUDE_CODE_VERSION}"

# Entrypoint and default CMD are inherited from `runtime`.
