#!/usr/bin/env bash
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

echo "Running R1 smoke — real haiku root spawns sub-leads..."
echo "Manifest: $SCRIPT_DIR/manifest.toml"
echo "Expected cost: ~\$0.05 (haiku)"
echo

./target/release/pitboss dispatch "$SCRIPT_DIR/manifest.toml"

echo
echo "Done. Recent run directories:"
ls -lt ~/.local/share/pitboss/runs 2>/dev/null | head -3 || true
