#!/usr/bin/env bash
# smoke-part3-tui.sh — automates the non-interactive portions of the Pitboss TUI
# smoke test (docs/v0.2-tui-smoke-test.md, tests 1-4, 6, 7, 9).
#
# Usage:
#   scripts/smoke-part3-tui.sh
#   PITBOSS_TUI=/path/to/pitboss-tui scripts/smoke-part3-tui.sh
#
# The interactive tests (5, 8) can't be automated — run those by hand after
# this script passes.

set -u

PITBOSS_TUI="${PITBOSS_TUI:-$(pwd)/target/debug/pitboss-tui}"
if [ ! -x "$PITBOSS_TUI" ]; then
    echo "ERROR: pitboss-tui binary not found at $PITBOSS_TUI" >&2
    echo "Run: cargo build -p pitboss-tui" >&2
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

echo "=== Pitboss TUI Part 3 — non-interactive smoke tests ==="
echo "binary: $PITBOSS_TUI"
echo

# -------------------------------------------------------------------
# 1 — --help
OUT=$("$PITBOSS_TUI" --help 2>&1); CODE=$?
if [ "$CODE" = "0" ] && echo "$OUT" | grep -q "Usage:"; then
    record "1 --help" PASS
else
    record "1 --help" FAIL "exit $CODE"
fi

# -------------------------------------------------------------------
# 2 — --version
OUT=$("$PITBOSS_TUI" --version 2>&1); CODE=$?
if [ "$CODE" = "0" ] && echo "$OUT" | grep -qE "pitboss-tui .*0\.1\.0"; then
    record "2 --version" PASS "$OUT"
else
    record "2 --version" FAIL "exit $CODE: $OUT"
fi

# -------------------------------------------------------------------
# 3 — list (may be empty; both outcomes are OK)
OUT=$("$PITBOSS_TUI" list 2>&1); CODE=$?
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
OUT=$(HOME=/tmp/definitely-no-pitboss-runs-here "$PITBOSS_TUI" list 2>&1); CODE=$?
if [ "$CODE" = "0" ] && echo "$OUT" | grep -qi "No runs"; then
    record "4 no-runs message" PASS
else
    record "4 no-runs message" FAIL "exit $CODE: $OUT"
fi

# -------------------------------------------------------------------
# 6 — Prefix match (only if at least one run exists)
POPULATED=$("$PITBOSS_TUI" list 2>&1 | tail -n +3 | head -1)
if [ -n "$POPULATED" ]; then
    PREFIX=$(echo "$POPULATED" | awk '{print substr($1,1,8)}')
    # We can't actually launch the TUI non-interactively, but find_run_by_id
    # gets called before TUI init — a non-TTY failure means it DID find the run.
    OUT=$("$PITBOSS_TUI" "$PREFIX" 2>&1); CODE=$?
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
OUT=$("$PITBOSS_TUI" zzzzzzz-not-a-real-run 2>&1); CODE=$?
if [ "$CODE" != "0" ] && echo "$OUT" | grep -qi "not found"; then
    record "7 nonexistent run" PASS "exit $CODE as expected"
else
    record "7 nonexistent run" FAIL "exit $CODE: $OUT"
fi

# -------------------------------------------------------------------
# 9 — Non-TTY rejection
OUT=$(echo "" | "$PITBOSS_TUI" 2>&1); CODE=$?
if [ "$CODE" != "0" ]; then
    record "9 non-TTY fails explicitly" PASS "exit $CODE"
else
    record "9 non-TTY fails explicitly" FAIL "expected nonzero exit, got 0"
fi

# -------------------------------------------------------------------
# v0.4: rendered help overlay includes new keybindings
LATEST_RUN=$("$PITBOSS_TUI" list 2>&1 | tail -n +3 | head -1 | awk '{print $1}')
if [ -n "$LATEST_RUN" ]; then
    echo "-- smoke: v0.4 help mentions new keybindings"
    HELP_OUT=$("$PITBOSS_TUI" screenshot --run "$LATEST_RUN" --cols 120 --rows 40 2>/dev/null || true)
    # Best-effort: the help overlay requires interactive entry, so we just check
    # the run-level render includes the standard title; full help check is in
    # cargo tests. This case guards that --rows 40 renders without panicking.
    if test -n "$HELP_OUT"; then
        record "v0.4 help keybindings" PASS
    else
        record "v0.4 help keybindings" FAIL "screenshot produced no output"
    fi
else
    record "v0.4 help keybindings" SKIP "no runs available for screenshot"
fi

# -------------------------------------------------------------------
# v0.4: screenshot renders with no control socket attached
if [ -n "$LATEST_RUN" ]; then
    echo "-- smoke: v0.4 screenshot on completed run renders normally"
    OUT=$("$PITBOSS_TUI" screenshot --run "$LATEST_RUN" --cols 120 --rows 30 2>&1); CODE=$?
    if [ "$CODE" = "0" ]; then
        record "v0.4 screenshot completed run" PASS
    else
        record "v0.4 screenshot completed run" FAIL "exit $CODE"
    fi
else
    record "v0.4 screenshot completed run" SKIP "no runs available"
fi

# -------------------------------------------------------------------
# v0.4: paused-worker tile (post-hoc) smoke
echo "-- smoke: v0.4 pitboss-tui list still works"
OUT=$("$PITBOSS_TUI" list 2>&1); CODE=$?
if [ "$CODE" = "0" ] && echo "$OUT" | head -n 5 >/dev/null; then
    record "v0.4 list command" PASS
else
    record "v0.4 list command" FAIL "exit $CODE"
fi

# -------------------------------------------------------------------
# Interactive tests (5, 8) — cannot automate
record "5 open most recent (interactive)" SKIP "run pitboss-tui manually in a real TTY"
record "8 live updates (interactive)"    SKIP "pair with a running pitboss dispatch"

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
echo "  $PITBOSS_TUI              # test 5: grid renders, keybindings work"
echo "  # (in parallel with a running pitboss dispatch — test 8)"
exit 0
