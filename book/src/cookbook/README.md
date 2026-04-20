# Cookbook — Spotlights overview

The dogfood spotlights are repeatable end-to-end tests that prove pitboss v0.6 features work from the operator's perspective. They drive the real `pitboss dispatch` CLI or in-process integration tests — not just unit tests of the library.

All spotlight source files live under [`examples/dogfood/`](https://github.com/SDS-Mode/pitboss/tree/main/examples/dogfood) in the repository.

## Running spotlights

**Shell script (subprocess demo):**
```bash
bash examples/dogfood/fake/01-smoke-hello-sublead/run.sh
```

**All dogfood tests via Cargo:**
```bash
cargo test --test dogfood_fake_flows
```

**Full suite:**
```bash
cargo test --workspace --quiet
```

## Fake vs. real spotlights

**`fake/` spotlights** use the `fake-claude` binary with pre-baked JSONL scripts. Fully deterministic, fast (~1s), no Anthropic API calls.

**`real/` spotlights** (R1–R3) use the actual `claude` binary. They require `PITBOSS_DOGFOOD_REAL=1` and a valid Anthropic API key. They count against your quota.

## Spotlight index

### Fake spotlights

| # | Name | What it proves | Cargo test |
|---|------|---------------|------------|
| 01 | [smoke-hello-sublead](https://github.com/SDS-Mode/pitboss/tree/main/examples/dogfood/fake/01-smoke-hello-sublead) | Depth-2 manifest dispatches; `allow_subleads`, `max_subleads`, `[lead.sublead_defaults]` all parse and resolve. `summary.json` shows `tasks_failed=0`. | `cargo test --test dogfood_fake_flows` |
| 02 | [Strict-tree isolation](./strict-tree-isolation.md) | Per-layer KV isolation; root cannot read sub-tree state without `read_down`; strict peer visibility. | `dogfood_isolation_strict_tree` |
| 03 | [Kill-cascade drain](./kill-cascade-drain.md) | Root cancel cascades depth-first through all sub-leads and workers within 200ms. | `dogfood_kill_cascade_drain` |
| 04 | [Run-global lease contention](./run-lease-contention.md) | Two sub-leads competing for the same `run_lease_acquire` key serialize correctly. | `dogfood_run_lease_contention` |
| 05 | [Approval policy auto-filter](./policy-auto-filter.md) | `[[approval_policy]]` rules auto-approve matching requests before they reach the operator queue. | `dogfood_policy_auto_filter` |
| 06 | [Envelope cap enforcement](./envelope-cap-enforcement.md) | `max_sublead_budget_usd` cap rejects oversized spawn attempts pre-state; clean retry succeeds. | `dogfood_envelope_cap_rejection` |

### Real spotlights (API-gated)

| # | Name | Notes |
|---|------|-------|
| R1 | real-root-spawns-sublead | Real haiku lead calls `spawn_sublead` at least once. ~$0.05. |
| R2 | real-kill-with-reason | Kill-with-reason stub (full orchestration deferred). |
| R3 | real-reject-with-reason | Lead adapts output format after `auto_reject` approval response. |

Real spotlights are in [`examples/dogfood/real/`](https://github.com/SDS-Mode/pitboss/tree/main/examples/dogfood/real). Run with:

```bash
PITBOSS_DOGFOOD_REAL=1 cargo test --test dogfood_real_flows -- --ignored
```
