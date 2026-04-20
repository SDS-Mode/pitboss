#!/usr/bin/env bash
set -euo pipefail
cat <<EOF
Spotlight #02: Strict-tree isolation
====================================

This spotlight exercises MCP tools (spawn_sublead, kv_set, kv_get)
which cannot be driven via subprocess fake-claude in v0.6. It runs
as an in-process integration test instead:

    cargo test --test dogfood_fake_flows dogfood_isolation_strict_tree

See README.md for what this demonstrates and expected-observables.md
for the scenario narrative.
EOF
