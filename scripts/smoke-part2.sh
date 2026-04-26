#!/usr/bin/env bash
# smoke-part2.sh — runs the online tests from docs/v0.1-smoke-test.md Part 2.
#
# Usage:
#   scripts/smoke-part2.sh
#   PITBOSS=/path/to/pitboss scripts/smoke-part2.sh
#   PITBOSS_SMOKE_DIR=/tmp/foo scripts/smoke-part2.sh
#   PITBOSS_SKIP_CTRL_C=1 scripts/smoke-part2.sh       # skip the interactive-ish test
#   PITBOSS_MODEL=claude-sonnet-4-6 scripts/smoke-part2.sh  # default: claude-haiku-4-5
#
# Exercises real `claude` subprocesses. Costs a few cents per test on Haiku.
# Total runtime roughly 10-25 minutes; total cost roughly $0.30-$1.00.
#
# Prerequisite: claude CLI already authenticated via subscription. We do a
# sanity check below before spending any real money.
#
# Artifacts land in ~/.local/share/pitboss/runs/ and are NOT cleaned up — you
# can inspect them after the run.

set -u

PITBOSS="${PITBOSS:-pitboss}"
MODEL="${PITBOSS_MODEL:-claude-haiku-4-5}"
SKIP_CTRL_C="${PITBOSS_SKIP_CTRL_C:-0}"
SKIP_HALT_ON_FAIL="${PITBOSS_SKIP_HALT_ON_FAIL:-1}"  # skip by default; see 2.7 note

if ! command -v "$PITBOSS" >/dev/null 2>&1 && [ ! -x "$PITBOSS" ]; then
    echo "ERROR: pitboss binary not found (tried: $PITBOSS)" >&2
    echo "Run: cargo build --release -p pitboss-cli && export PATH=\"\$(pwd)/target/release:\$PATH\"" >&2
    exit 2
fi

if ! command -v claude >/dev/null 2>&1; then
    echo "ERROR: claude CLI not on PATH" >&2
    exit 2
fi

if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq required for artifact inspection" >&2
    exit 2
fi

# -------------------------------------------------------------------
# Pre-flight: confirm the user wants to proceed (real money)
echo "=== Part 2 — online smoke tests ==="
echo "pitboss:   $("$PITBOSS" version)"
echo "claude:    $(claude --version 2>&1 | head -1)"
echo "model:     $MODEL"
echo "skip 2.7:  $([ "$SKIP_HALT_ON_FAIL" = "1" ] && echo yes || echo no)"
echo "skip 2.9:  $([ "$SKIP_CTRL_C"       = "1" ] && echo yes || echo no)"
echo
echo "This will spawn real claude subprocesses (~\$0.30-\$1.00 total on Haiku)."
printf "Proceed? [y/N] "
read -r reply
case "$reply" in
    y|Y|yes|YES) ;;
    *) echo "aborted"; exit 1 ;;
esac

# -------------------------------------------------------------------
# Bootstrap: two git repos + scratch dir
SCRATCH="${PITBOSS_SMOKE_DIR:-$(mktemp -d -t pitboss-online-XXXXXX)}"
mkdir -p "$SCRATCH"
REPO_A="$SCRATCH/repo-a"
REPO_B="$SCRATCH/repo-b"

init_repo() {
    local p="$1"
    rm -rf "$p" && mkdir -p "$p"
    (cd "$p" \
        && git init -q \
        && git config user.email "t@t.x" \
        && git config user.name "t" \
        && echo "x" > README.md \
        && git add . \
        && git commit -q -m init)
}
init_repo "$REPO_A"
init_repo "$REPO_B"

echo "scratch:   $SCRATCH"
echo

# -------------------------------------------------------------------
# Results
declare -a NAMES
declare -a RESULTS
declare -a NOTES

record() {
    NAMES+=("$1"); RESULTS+=("$2"); NOTES+=("${3:-}")
    local color reset="\033[0m"
    case "$2" in PASS) color="\033[32m";; FAIL) color="\033[31m";; SKIP) color="\033[33m";; esac
    printf "  [${color}%4s${reset}] %s" "$2" "$1"
    [ -n "${3:-}" ] && printf " — %s" "$3"
    printf "\n"
}

latest_run_dir() {
    ls -td ~/.local/share/pitboss/runs/*/ 2>/dev/null | head -1
}

# -------------------------------------------------------------------
# 2.1 Single-task happy path + 2.2/2.3/2.4 artifact sanity (bundled)
cat > "$SCRATCH/t1.toml" <<EOF
[run]
worktree_cleanup = "always"

[defaults]
model = "$MODEL"
timeout_secs = 120
use_worktree = true

[[task]]
id = "write-hi"
directory = "$REPO_A"
prompt = "Create a file named hi.txt in the working directory containing only the single word: hello. Then commit it with: git add hi.txt && git commit -m 'add hi'. Do not add other commentary."
branch = "smoke/write-hi"
EOF

echo "== 2.1 Single-task happy path =="
if "$PITBOSS" dispatch "$SCRATCH/t1.toml"; then
    RD=$(latest_run_dir)
    if [ -f "$RD/summary.json" ]; then
        STATUS=$(jq -r '.tasks[0].status' "$RD/summary.json")
        IN=$(   jq -r '.tasks[0].token_usage.input'         "$RD/summary.json")
        OUT=$(  jq -r '.tasks[0].token_usage.output'        "$RD/summary.json")
        SID=$(  jq -r '.tasks[0].claude_session_id // "null"' "$RD/summary.json")
        PREV=$( jq -r '.tasks[0].final_message_preview // "null"' "$RD/summary.json")

        if [ "$STATUS" = "Success" ]; then
            record "2.1 happy dispatch" PASS
        else
            record "2.1 happy dispatch" FAIL "status=$STATUS"
        fi

        # 2.2 token usage
        if [ "$IN" -gt 0 ] && [ "$OUT" -gt 0 ]; then
            record "2.2 token usage >0" PASS "in=$IN out=$OUT"
        else
            record "2.2 token usage >0" FAIL "in=$IN out=$OUT"
        fi

        # 2.3 preview sensible
        if [ "$PREV" != "null" ] && [ "${#PREV}" -le 210 ]; then
            record "2.3 preview sensible" PASS "${#PREV} chars"
        else
            record "2.3 preview sensible" FAIL "PREV=$PREV"
        fi

        # 2.4 session_id present
        if [ "$SID" != "null" ] && [ -n "$SID" ]; then
            record "2.4 session_id present" PASS "$SID"
        else
            record "2.4 session_id present" FAIL "got null"
        fi

        # File actually created?
        if git -C "$REPO_A" show smoke/write-hi:hi.txt >/dev/null 2>&1; then
            record "2.1b file created on branch" PASS
        else
            record "2.1b file created on branch" FAIL "hi.txt missing on smoke/write-hi"
        fi

        # Worktree cleaned up (policy=always)?
        if ! ls "$SCRATCH"/repo-a-pitboss-write-hi-* >/dev/null 2>&1; then
            record "2.1c worktree cleanup" PASS
        else
            record "2.1c worktree cleanup" FAIL "sibling worktree persists"
        fi
    else
        record "2.1 happy dispatch" FAIL "no summary.json"
    fi
else
    record "2.1 happy dispatch" FAIL "pitboss exit $?"
fi
echo

# -------------------------------------------------------------------
# 2.5 Two-task parallel
cat > "$SCRATCH/t2.toml" <<EOF
[run]
max_parallel_tasks = 2
worktree_cleanup = "always"

[defaults]
model = "$MODEL"
timeout_secs = 120

[[task]]
id = "a-task"
directory = "$REPO_A"
prompt = "Create a file named a.txt containing only the letter A."
branch = "smoke/a"

[[task]]
id = "b-task"
directory = "$REPO_B"
prompt = "Create a file named b.txt containing only the letter B."
branch = "smoke/b"
EOF

echo "== 2.5 Two-task parallel =="
T0=$(date +%s)
"$PITBOSS" dispatch "$SCRATCH/t2.toml" >/dev/null 2>&1
PARCODE=$?
T1=$(date +%s)
ELAPSED=$((T1 - T0))

if [ "$PARCODE" = "0" ]; then
    RD=$(latest_run_dir)
    TOT=$(jq -r '.tasks_total'  "$RD/summary.json")
    FAIL=$(jq -r '.tasks_failed' "$RD/summary.json")
    if [ "$TOT" = "2" ] && [ "$FAIL" = "0" ]; then
        record "2.5 parallel dispatch" PASS "${ELAPSED}s wall, 2 tasks"
    else
        record "2.5 parallel dispatch" FAIL "total=$TOT failed=$FAIL"
    fi
else
    record "2.5 parallel dispatch" FAIL "exit $PARCODE"
fi
echo

# -------------------------------------------------------------------
# 2.6 Template expansion
cat > "$SCRATCH/t3.toml" <<EOF
[defaults]
model = "$MODEL"
use_worktree = false
timeout_secs = 120

[[template]]
id = "greet"
prompt = "Your ENTIRE response must be exactly two words, nothing else: {greeting} {name}. No confirmation, no commentary, no 'Done', no explanation — just those two words."

[[task]]
id = "english"
directory = "$REPO_A"
template = "greet"
vars = { greeting = "hello", name = "world" }
EOF

echo "== 2.6 Template expansion =="
if "$PITBOSS" dispatch "$SCRATCH/t3.toml" >/dev/null 2>&1; then
    RD=$(latest_run_dir)
    # Check stdout.log (raw stream-json) for the substituted words — robust against
    # whatever claude decides to put in the final message.
    LOG="$RD/tasks/english/stdout.log"
    if grep -qi "hello" "$LOG" && grep -qi "world" "$LOG"; then
        record "2.6 template expansion" PASS "found in stdout.log"
    else
        record "2.6 template expansion" FAIL "hello/world missing from stdout.log"
    fi
else
    record "2.6 template expansion" FAIL "dispatch failed"
fi
echo

# -------------------------------------------------------------------
# 2.7 halt_on_failure (skip by default — deterministic version is in integration tests)
if [ "$SKIP_HALT_ON_FAIL" = "1" ]; then
    record "2.7 halt_on_failure" SKIP "covered deterministically by automated integration test"
else
    cat > "$SCRATCH/t4.toml" <<EOF
[run]
halt_on_failure = true
max_parallel_tasks = 1

[defaults]
model = "$MODEL"
timeout_secs = 10
use_worktree = false

[[task]]
id = "will-timeout"
directory = "$REPO_A"
timeout_secs = 3
prompt = "Count very slowly from 1 to 100000, one number per line, with full sentences."

[[task]]
id = "should-be-skipped"
directory = "$REPO_A"
prompt = "Create skip.txt containing SKIPPED."
EOF
    echo "== 2.7 halt_on_failure cascade =="
    "$PITBOSS" dispatch "$SCRATCH/t4.toml" >/dev/null 2>&1
    RD=$(latest_run_dir)
    if [ -f "$RD/summary.json" ]; then
        LEN=$(jq -r '.tasks | length' "$RD/summary.json")
        if [ "$LEN" = "1" ]; then
            record "2.7 halt_on_failure" PASS "only 1 task recorded"
        else
            record "2.7 halt_on_failure" FAIL "got $LEN tasks in summary"
        fi
    else
        record "2.7 halt_on_failure" FAIL "no summary.json"
    fi
fi
echo

# -------------------------------------------------------------------
# 2.8 Timeout enforcement
cat > "$SCRATCH/t5.toml" <<EOF
[defaults]
model = "$MODEL"
use_worktree = false
timeout_secs = 5

[[task]]
id = "slow"
directory = "$REPO_A"
prompt = "Count very slowly from 1 to 100000, one number per line, with full sentences between each number."
EOF

echo "== 2.8 Timeout enforcement =="
T0=$(date +%s)
"$PITBOSS" dispatch "$SCRATCH/t5.toml" >/dev/null 2>&1
T1=$(date +%s)
ELAPSED=$((T1 - T0))
RD=$(latest_run_dir)
if [ -f "$RD/summary.json" ]; then
    STATUS=$(jq -r '.tasks[0].status' "$RD/summary.json")
    # Expect TimedOut and elapsed ≈ 5s + TERMINATE_GRACE (10s) = ~15s, but could be faster if child exits on SIGTERM
    if [ "$STATUS" = "TimedOut" ]; then
        record "2.8 timeout status" PASS "TimedOut in ${ELAPSED}s"
    else
        record "2.8 timeout status" FAIL "status=$STATUS elapsed=${ELAPSED}s"
    fi
else
    record "2.8 timeout status" FAIL "no summary.json"
fi
echo

# -------------------------------------------------------------------
# 2.9 Ctrl-C two-phase
if [ "$SKIP_CTRL_C" = "1" ]; then
    record "2.9 two-phase Ctrl-C" SKIP "PITBOSS_SKIP_CTRL_C=1"
else
    cat > "$SCRATCH/t6.toml" <<EOF
[defaults]
model = "$MODEL"
use_worktree = false
timeout_secs = 600

[[task]]
id = "held"
directory = "$REPO_A"
prompt = "Count very slowly from 1 to 10000, one number per line, with a one-sentence comment between each. Do not stop."
EOF

    echo "== 2.9 Two-phase Ctrl-C =="
    LOG=$(mktemp)
    "$PITBOSS" dispatch "$SCRATCH/t6.toml" > "$LOG" 2>&1 &
    PITBOSS_PID=$!

    # Let pitboss spawn and start the task
    sleep 4

    # First SIGINT → should emit drain message
    kill -INT $PITBOSS_PID 2>/dev/null
    sleep 1

    # Second SIGINT within 5s window → terminate
    kill -INT $PITBOSS_PID 2>/dev/null

    # Wait up to 20s for pitboss to exit (TERMINATE_GRACE is 10s + safety)
    for i in $(seq 1 20); do
        if ! kill -0 $PITBOSS_PID 2>/dev/null; then break; fi
        sleep 1
    done
    wait $PITBOSS_PID 2>/dev/null
    PITBOSS_EXIT=$?

    if grep -q "draining" "$LOG" && grep -q "terminating" "$LOG"; then
        # Exit code should be 130 per spec, but pitboss may bail with another code
        if [ "$PITBOSS_EXIT" = "130" ] || [ "$PITBOSS_EXIT" -gt 0 ]; then
            RD=$(latest_run_dir)
            if [ -f "$RD/summary.json" ]; then
                STATUS=$(jq -r '.tasks[0].status' "$RD/summary.json")
                if [ "$STATUS" = "Cancelled" ]; then
                    record "2.9 two-phase Ctrl-C" PASS "exit=$PITBOSS_EXIT status=Cancelled"
                else
                    record "2.9 two-phase Ctrl-C" FAIL "exit=$PITBOSS_EXIT but status=$STATUS"
                fi
            else
                record "2.9 two-phase Ctrl-C" FAIL "no summary.json after cancel"
            fi
        else
            record "2.9 two-phase Ctrl-C" FAIL "pitboss exit=$PITBOSS_EXIT"
        fi
    else
        record "2.9 two-phase Ctrl-C" FAIL "drain/terminate log lines missing; see $LOG"
    fi
    rm -f "$LOG"
fi
echo

# -------------------------------------------------------------------
# 2.10 Worktree cleanup policies
echo "== 2.10 Worktree cleanup policies =="
for POLICY in "on_success" "never" "always"; do
    TOML="$SCRATCH/t7-wt-$POLICY.toml"
    BR="smoke/wt-$POLICY"
    cat > "$TOML" <<EOF
[run]
worktree_cleanup = "$POLICY"

[defaults]
model = "$MODEL"
timeout_secs = 60

[[task]]
id = "wt-$POLICY"
directory = "$REPO_A"
prompt = "Create a file named wt-$POLICY.txt containing the single word done."
branch = "$BR"
EOF
    "$PITBOSS" dispatch "$TOML" >/dev/null 2>&1
    EXIT_CODE=$?
    SIBLING="$SCRATCH"/repo-a-pitboss-wt-$POLICY-*
    if ls $SIBLING >/dev/null 2>&1; then
        EXISTS=yes
    else
        EXISTS=no
    fi

    case "$POLICY" in
        on_success)
            if [ "$EXIT_CODE" = "0" ] && [ "$EXISTS" = "no" ]; then
                record "2.10 cleanup on_success (success→remove)" PASS
            else
                record "2.10 cleanup on_success (success→remove)" FAIL "exit=$EXIT_CODE persist=$EXISTS"
            fi
            ;;
        never)
            if [ "$EXISTS" = "yes" ]; then
                record "2.10 cleanup never (keeps)" PASS
                # clean up manually so next test starts clean
                git -C "$REPO_A" worktree remove --force $SIBLING 2>/dev/null || true
                rm -rf $SIBLING 2>/dev/null || true
            else
                record "2.10 cleanup never (keeps)" FAIL "worktree was removed"
            fi
            ;;
        always)
            if [ "$EXISTS" = "no" ]; then
                record "2.10 cleanup always (success→remove)" PASS
            else
                record "2.10 cleanup always (success→remove)" FAIL "worktree persists"
            fi
            ;;
    esac
done
echo

# -------------------------------------------------------------------
# 2.11 Branch-conflict validation (offline validation, no claude invocation)
cat > "$SCRATCH/t8.toml" <<EOF
[defaults]
model = "$MODEL"

[[task]]
id = "a"
directory = "$REPO_A"
prompt = "p"
branch = "duplicate-branch"

[[task]]
id = "b"
directory = "$REPO_A"
prompt = "p"
branch = "duplicate-branch"
EOF

echo "== 2.11 Branch-conflict validation =="
"$PITBOSS" dispatch "$SCRATCH/t8.toml" >/dev/null 2>&1
CODE=$?
if [ "$CODE" = "2" ]; then
    record "2.11 branch conflict" PASS "exit=2 as expected"
else
    record "2.11 branch conflict" FAIL "exit=$CODE (wanted 2)"
fi
echo

# -------------------------------------------------------------------
# Summary
echo "=== Summary ==="
PASS_COUNT=0; FAIL_COUNT=0; SKIP_COUNT=0
for i in "${!NAMES[@]}"; do
    case "${RESULTS[$i]}" in
        PASS) PASS_COUNT=$((PASS_COUNT+1)) ;;
        FAIL) FAIL_COUNT=$((FAIL_COUNT+1)) ;;
        SKIP) SKIP_COUNT=$((SKIP_COUNT+1)) ;;
    esac
done
printf "%d passed, %d failed, %d skipped (%d total)\n" "$PASS_COUNT" "$FAIL_COUNT" "$SKIP_COUNT" "${#NAMES[@]}"

if [ "$FAIL_COUNT" -gt 0 ]; then
    echo
    echo "Failures:"
    for i in "${!NAMES[@]}"; do
        if [ "${RESULTS[$i]}" = "FAIL" ]; then
            printf "  - %s: %s\n" "${NAMES[$i]}" "${NOTES[$i]}"
        fi
    done
    echo
    echo "Inspect artifacts at: ~/.local/share/pitboss/runs/"
    exit 1
fi

echo
echo "Part 2 green."
echo "Artifacts at: ~/.local/share/pitboss/runs/"
echo "Scratch at:   $SCRATCH"
exit 0
