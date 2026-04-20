#!/usr/bin/env bash
set -euo pipefail
cat <<EOF
Spotlight #03: Depth-first cascade cancellation
================================================

This spotlight exercises cascade cancellation via MCP tools (spawn_sublead)
which cannot be driven via subprocess fake-claude in v0.6. It runs as an
in-process integration test instead:

    cargo test --test dogfood_fake_flows dogfood_kill_cascade_drain

See README.md for what this demonstrates and expected-observables.md
for the scenario narrative and timing expectations.
EOF
