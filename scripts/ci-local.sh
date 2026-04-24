#!/usr/bin/env bash
#
# Run the exact checks that .github/workflows/ci.yml's "test + lint + fmt"
# job runs. Use this before `git push` to catch CI failures locally.
#
# The flags, features, and RUSTFLAGS here are deliberately kept in sync
# with ci.yml — a plain `cargo clippy` / `cargo test` does not cover the
# same surface:
#
#   - `--all-features` exposes features that are off by default.
#   - `--features pitboss-core/test-support` enables the integration
#     tests in crates/pitboss-cli/tests/ that depend on the FakeSpawner.
#   - `RUSTFLAGS=-D warnings` promotes build warnings to errors (this is
#     set in ci.yml's env block).
#
# Usage:
#   bash scripts/ci-local.sh
#
# Optional install as a pre-push hook:
#   ln -s ../../scripts/ci-local.sh .git/hooks/pre-push

set -euo pipefail

cd "$(dirname "$0")/.."

export RUSTFLAGS="${RUSTFLAGS:-} -D warnings"
export CARGO_TERM_COLOR=always

echo "==> cargo fmt --all -- --check"
cargo fmt --all -- --check

echo "==> cargo clippy --workspace --all-targets --all-features -- -D warnings"
cargo clippy --workspace --all-targets --all-features -- -D warnings

echo "==> cargo test --workspace --features pitboss-core/test-support"
cargo test --workspace --features pitboss-core/test-support

echo "==> all CI checks passed"
