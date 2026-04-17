# Agent Shire

Headless Rust dispatcher for parallel Claude Code agent sessions.

## Install

```
cargo install --path crates/shire-cli
```

## Quick start

Create `shire.toml` in a directory that is inside a git repo:

```toml
[run]
max_parallel = 2

[[task]]
id = "hello"
directory = "/path/to/repo"
prompt = "Say hello in a file called hello.txt"
branch = "feat/hello"
```

Then:

```
shire validate shire.toml
shire dispatch shire.toml
```

Artifacts land in `~/.local/share/shire/runs/<run-id>/`.

## Concurrency

Default `max_parallel` is 4. Override hierarchy: manifest `[run].max_parallel`
beats `ANTHROPIC_MAX_CONCURRENT` env beats the default.

## Manual smoke test for releases

With a real `claude` binary on PATH and ANTHROPIC_API_KEY set:

1. Create two throwaway git repos.
2. Point one manifest at each with a trivial prompt ("write `hi` to a file").
3. `shire dispatch ./manifest.toml` — confirm the progress table updates, both
   Hobbits succeed, and the summary.json contains expected fields.
4. Run again with `halt_on_failure = true` and an intentionally-failing prompt
   in the first task. Confirm the second task is skipped.
5. Run with a long-running prompt and Ctrl-C once → drain completes; Ctrl-C
   twice → tasks report `Cancelled`.

## Development

```
cargo test --workspace
cargo test -p mosaic-core --features test-support
cargo lint
cargo tidy
```

See `docs/superpowers/specs/2026-04-16-agent-shire-design.md` for design.
