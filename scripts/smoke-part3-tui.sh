#!/usr/bin/env bash
# smoke-part3-tui.sh — automates the non-interactive portions of the Mosaic TUI
# smoke test (docs/v0.2-tui-smoke-test.md, tests 1-4, 6, 7, 9).
#
# Usage:
#   scripts/smoke-part3-tui.sh
#   MOSAIC=/path/to/mosaic scripts/smoke-part3-tui.sh
#
# The interactive tests (5, 8) can't be automated — run those by hand after
# this script passes.

set -u

MOSAIC="${MOSAIC:-$(pwd)/target/debug/mosaic}"
if [ ! -x "$MOSAIC" ]; then
    echo "ERROR: mosaic binary not found at $MOSAIC" >&2
    echo "Run: cargo build -p mosaic-tui" >&2
    exit 2
fi

# -------------------------------------------------------------------
# Results tracking
declare -a NAMES
declare -a RESULTS
declare -a NOTES

record() {
    NAMES+=("$1"); RESULTS+=("$2"); NOTES+=("${3:-}")
    local color reset="\033[0m"
    case "$2" in
        PASS) color="\033[32m" ;;
        FAIL) color="\033[31m" ;;
        SKIP) color="\033[33m" ;;
    esac
    printf "  [${color}%4s${reset}] %s" "$2" "$1"
    [ -n "${3:-}" ] && printf " — %s" "$3"
    printf "\n"
}

echo "=== Mosaic TUI Part 3 — non-interactive smoke tests ==="
echo "binary: $MOSAIC"
echo

# -------------------------------------------------------------------
# 1 — --help
OUT=$("$MOSAIC" --help 2>&1); CODE=$?
if [ "$CODE" = "0" ] && echo "$OUT" | grep -q "Usage:"; then
    record "1 --help" PASS
else
    record "1 --help" FAIL "exit $CODE"
fi

# -------------------------------------------------------------------
# 2 — --version
OUT=$("$MOSAIC" --version 2>&1); CODE=$?
if [ "$CODE" = "0" ] && echo "$OUT" | grep -qE "mosaic .*0\.1\.0"; then
    record "2 --version" PASS "$OUT"
else
    record "2 --version" FAIL "exit $CODE: $OUT"
fi

# -------------------------------------------------------------------
# 3 — list (may be empty; both outcomes are OK)
OUT=$("$MOSAIC" list 2>&1); CODE=$?
if [ "$CODE" = "0" ]; then
    if echo "$OUT" | grep -q "RUN ID"; then
        ROWS=$(echo "$OUT" | tail -n +3 | wc -l)
        record "3 list (populated)" PASS "$ROWS runs"
    else
        record "3 list (empty)" PASS "no runs yet"
    fi
else
    record "3 list" FAIL "exit $CODE"
fi

# -------------------------------------------------------------------
# 4 — No-runs message via HOME override
OUT=$(HOME=/tmp/definitely-no-shire-runs-here "$MOSAIC" list 2>&1); CODE=$?
if [ "$CODE" = "0" ] && echo "$OUT" | grep -qi "No runs"; then
    record "4 no-runs message" PASS
else
    record "4 no-runs message" FAIL "exit $CODE: $OUT"
fi

# -------------------------------------------------------------------
# 6 — Prefix match (only if at least one run exists)
POPULATED=$("$MOSAIC" list 2>&1 | tail -n +3 | head -1)
if [ -n "$POPULATED" ]; then
    PREFIX=$(echo "$POPULATED" | awk '{print substr($1,1,8)}')
    # We can't actually launch the TUI non-interactively, but find_run_by_id
    # gets called before TUI init — a non-TTY failure means it DID find the run.
    OUT=$("$MOSAIC" "$PREFIX" 2>&1); CODE=$?
    # Expect error because no TTY, but error should NOT be "Run not found".
    if echo "$OUT" | grep -qi "Run .* not found"; then
        record "6 prefix match" FAIL "run lookup failed: $OUT"
    else
        record "6 prefix match" PASS "lookup succeeded (TUI init failed w/o TTY, expected)"
    fi
else
    record "6 prefix match" SKIP "no runs to prefix-match against"
fi

# -------------------------------------------------------------------
# 7 — Nonexistent run id
OUT=$("$MOSAIC" zzzzzzz-not-a-real-run 2>&1); CODE=$?
if [ "$CODE" != "0" ] && echo "$OUT" | grep -qi "not found"; then
    record "7 nonexistent run" PASS "exit $CODE as expected"
else
    record "7 nonexistent run" FAIL "exit $CODE: $OUT"
fi

# -------------------------------------------------------------------
# 9 — Non-TTY rejection
OUT=$(echo "" | "$MOSAIC" 2>&1); CODE=$?
if [ "$CODE" != "0" ]; then
    record "9 non-TTY fails explicitly" PASS "exit $CODE"
else
    record "9 non-TTY fails explicitly" FAIL "expected nonzero exit, got 0"
fi

# -------------------------------------------------------------------
# Interactive tests (5, 8) — cannot automate
record "5 open most recent (interactive)" SKIP "run mosaic manually in a real TTY"
record "8 live updates (interactive)"    SKIP "pair with a running shire dispatch"

# -------------------------------------------------------------------
echo
echo "=== Summary ==="
PASS_COUNT=0; FAIL_COUNT=0; SKIP_COUNT=0
for i in "${!NAMES[@]}"; do
    case "${RESULTS[$i]}" in
        PASS) PASS_COUNT=$((PASS_COUNT+1)) ;;
        FAIL) FAIL_COUNT=$((FAIL_COUNT+1)) ;;
        SKIP) SKIP_COUNT=$((SKIP_COUNT+1)) ;;
    esac
done
printf "%d passed, %d failed, %d skipped (%d total)\n" \
    "$PASS_COUNT" "$FAIL_COUNT" "$SKIP_COUNT" "${#NAMES[@]}"

if [ "$FAIL_COUNT" -gt 0 ]; then
    echo
    echo "Failures:"
    for i in "${!NAMES[@]}"; do
        if [ "${RESULTS[$i]}" = "FAIL" ]; then
            printf "  - %s: %s\n" "${NAMES[$i]}" "${NOTES[$i]}"
        fi
    done
    exit 1
fi

echo
echo "Non-interactive Part 3 green. Run tests 5 + 8 manually in a real terminal:"
echo "  $MOSAIC              # test 5: grid renders, keybindings work"
echo "  # (in parallel with a running shire dispatch — test 8)"
exit 0
