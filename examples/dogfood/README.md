# Dogfood Manifests

Repeatable end-to-end manifests that prove pitboss v0.6 features work from the
operator's perspective — driving the real `pitboss dispatch` CLI, not just
unit-testing the library internals.

## Layout

```
examples/dogfood/
├── README.md          (this file)
└── fake/              # Deterministic tests: fake-claude scripts, no real Claude API
    └── 01-smoke-hello-sublead/
        ├── manifest.toml           Depth-2 manifest with allow_subleads=true
        ├── lead-script.jsonl       Fake-claude script for the root lead
        ├── expected-summary.json   Fields to assert after a successful run
        └── run.sh                  Shell demo (builds + dispatches)
```

A `real/` subdirectory (real Claude API, counted against quota) is planned for
v0.6.1+.

## How to run a spotlight manually

```bash
bash examples/dogfood/fake/01-smoke-hello-sublead/run.sh
```

## How to run all dogfood tests via Cargo

```bash
cargo test --test dogfood_fake_flows
```

Or as part of the full suite:

```bash
cargo test --workspace --quiet
```

## Spotlights

| # | Name | What it proves |
|---|------|----------------|
| 01 | smoke-hello-sublead | Depth-2 manifest dispatches cleanly; summary.json has `tasks_failed=0`, `was_interrupted=false`. Manifest schema with `allow_subleads=true`, `max_subleads`, `max_sublead_budget_usd`, and `[lead.sublead_defaults]` all parse and resolve correctly. |
| 02 | TODO | spawn_sublead MCP call lifecycle (reserved budget, subleads map, SubleadSpawned event) |
| 03 | TODO | wait_actor on sub-lead returns after reconcile |
| 04 | TODO | budget envelope returns to root pool after sub-lead terminates |
| 05 | TODO | root kill cascades to sub-lead workers |
| 06 | TODO | run-global lease serializes two sub-leads |

## Real-Claude spotlights (planned v0.6.1+)

| # | Name | What it proves |
|---|------|----------------|
| R1 | TODO | Real Claude API root lead spawns one sub-lead (stub session) |
| R2 | TODO | Approval gate blocks sub-lead spawn until operator approves |
| R3 | TODO | Budget exhaustion returns error to lead gracefully |

## Flagship walkthrough (planned)

A longer manifest + narrated `run.sh` that shows the complete depth-2 lifecycle
as a demo script for release notes and documentation.

## Notes on fake vs real separation

`fake/` spotlights use the `fake-claude` binary (compiled from
`tests-support/fake-claude/`) with pre-baked JSONL scripts. They are fully
deterministic, fast (~1s), and never call the Anthropic API.

`real/` spotlights (coming) use the actual `claude` binary. They require a
valid `ANTHROPIC_API_KEY` and count against your quota.

## MCP socket note for spotlight #01

`spawn_sublead_session` is a no-op stub in v0.6 (Task 2.3 wires real sub-lead
Claude sessions). Spotlight #01 therefore proves the manifest schema and dispatch
pipeline via a MCP-less script path. The `spawn_sublead` MCP call lifecycle is
fully covered by the Rust integration tests in
`crates/pitboss-cli/tests/e2e_sublead_flows.rs`. Spotlight #02 will drive
`spawn_sublead` via MCP once the bridge socket path is injectable into the lead
subprocess environment.
