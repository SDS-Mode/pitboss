#!/usr/bin/env bash
# R2 smoke — real haiku lead spawns a worker; operator kills it with reason;
# lead's next turn references the reason.
#
# NOTE: R2 is currently a stub in the Rust integration tests. This script
# documents the expected manual flow but does NOT drive the side-channel
# cancel_worker call automatically. A future implementation should:
#   1. Start dispatch in the background
#   2. Wait for the worker to appear
#   3. Call cancel_worker with the reason via the MCP socket
#   4. Let dispatch finish and inspect the lead's final message
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

# Preflight
if ! command -v claude >/dev/null 2>&1; then
    echo "ERROR: 'claude' CLI not found in PATH." >&2
    echo "Install it: see https://docs.claude.com/en/docs/claude-code" >&2
    exit 1
fi

if [[ -z "${ANTHROPIC_API_KEY:-}" ]]; then
    echo "ERROR: ANTHROPIC_API_KEY not set." >&2
    exit 1
fi

cd "$REPO_ROOT"
cargo build --workspace --release --quiet

echo "R2 NOTE: This spotlight requires a side-channel operator (cancel_worker"
echo "with a reason) while the lead is mid-flight. Full automation is pending."
echo "Running the base dispatch (no side-channel) to verify the manifest is valid..."
echo "Manifest: $SCRIPT_DIR/manifest.toml"
echo "Expected cost: ~\$0.10-\$0.20 (haiku)"
echo

./target/release/pitboss dispatch "$SCRIPT_DIR/manifest.toml"

echo
echo "Done. Recent run directories:"
ls -lt ~/.local/share/pitboss/runs 2>/dev/null | head -3 || true
echo
echo "To run R2 fully (with side-channel cancel), use the Rust integration test:"
echo "  PITBOSS_DOGFOOD_REAL=1 cargo test --test dogfood_real_flows real_kill_with_reason -- --ignored"
