#!/usr/bin/env bash
set -euo pipefail
cat <<EOF
Spotlight #04: Run-global lease contention
============================================

This spotlight exercises cross-tree resource coordination via run-global leases
(run_lease_acquire / run_lease_release), which cannot be driven via subprocess
fake-claude in v0.6. It runs as an in-process integration test instead:

    cargo test --test dogfood_fake_flows dogfood_run_lease_contention

See README.md for what this demonstrates and expected-observables.md
for the scenario narrative and lease state transitions.
EOF
