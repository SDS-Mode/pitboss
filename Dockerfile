# Pitboss container image.
#
# Multi-stage build: compile the workspace in a rust toolchain image,
# copy just the two release binaries into a slim debian runtime. The
# runtime image carries `git` (pitboss uses git worktrees for task
# isolation), `ca-certificates` (for webhook notifications), and the
# pitboss + pitboss-tui binaries on $PATH. It intentionally does NOT
# bundle `claude` — that's caller-supplied, usually via a volume mount
# of the host's Claude Code install.
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

USER pitboss
WORKDIR /home/pitboss

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["pitboss", "--help"]
