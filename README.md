# Agent Shire

Headless Rust dispatcher for parallel Claude Code agent sessions ("Hobbits").
Reads a `shire.toml` manifest, fans out N subprocesses under a concurrency cap,
and writes structured per-run artifacts.

**Status:** v0.1 under active development. See
[`docs/superpowers/specs/2026-04-16-agent-shire-design.md`](docs/superpowers/specs/2026-04-16-agent-shire-design.md)
for the authoritative design.

## Build

```
cargo build --release
```

## Test

```
cargo test --workspace
```

## Layout

- `crates/mosaic-core/` — library: session/process/parser/worktree/store machinery
- `crates/shire-cli/`   — binary: `shire` CLI that consumes the library
- `tests-support/fake-claude/` — scripted fake `claude` used only in integration tests
- `docs/` — design spec and implementation plan
