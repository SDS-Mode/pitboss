#!/usr/bin/env bash
# Dogfood spotlight #01 — "hello sub-lead" smoke test.
#
# Drives pitboss dispatch with a fake-claude binary and a pre-baked JSONL script
# so the test is fully deterministic and does not call the real Claude API.
#
# Usage:
#   bash examples/dogfood/fake/01-smoke-hello-sublead/run.sh
#
# On success:
#   - pitboss exits 0
#   - summary.json is written under ~/.local/share/pitboss/runs/<run-id>/
#   - summary.json shows tasks_failed=0
#
# NOTE on MCP connectivity: fake-claude emits stream-json events directly
# without connecting to the pitboss MCP socket. Real spawn_sublead MCP calls
# require the socket path to be known ahead of time; the Rust integration tests
# in e2e_sublead_flows.rs cover that path end-to-end by wiring the socket
# explicitly. This script proves the dispatch pipeline and manifest schema.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

echo "Building workspace (debug profile)..."
cd "$REPO_ROOT"
cargo build -p pitboss-cli -p fake-claude --quiet

echo
echo "Running dogfood spotlight #01: hello sub-lead..."
echo "Manifest: $SCRIPT_DIR/manifest.toml"
echo

PITBOSS_CLAUDE_BINARY="$REPO_ROOT/target/debug/fake-claude" \
PITBOSS_FAKE_SCRIPT="$SCRIPT_DIR/lead-script.jsonl" \
"$REPO_ROOT/target/debug/pitboss" dispatch "$SCRIPT_DIR/manifest.toml"

echo
echo "Done. Recent run directories:"
ls -lt "${HOME}/.local/share/pitboss/runs" 2>/dev/null | head -4 || echo "(no runs directory found)"
