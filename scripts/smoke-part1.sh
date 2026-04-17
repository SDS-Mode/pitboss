#!/usr/bin/env bash
# smoke-part1.sh — runs the 10 offline tests from docs/v0.1-smoke-test.md Part 1.
#
# Usage:
#   scripts/smoke-part1.sh
#   SHIRE=/path/to/shire scripts/smoke-part1.sh
#
# No API calls; exercises CLI plumbing, manifest validation, exit codes,
# and concurrency precedence. Finishes in ~5 seconds on a warm build.
#
# Exit 0 → all 10 tests passed. Exit 1 → one or more failed.

set -u  # deliberately not -e: we want to run all tests even if some fail

SHIRE="${SHIRE:-shire}"
if ! command -v "$SHIRE" >/dev/null 2>&1 && [ ! -x "$SHIRE" ]; then
    echo "ERROR: shire binary not found (tried: $SHIRE)" >&2
    echo "Build and try again:" >&2
    echo "  cargo build --release -p shire-cli" >&2
    echo "  export PATH=\"\$(pwd)/target/release:\$PATH\"" >&2
    exit 2
fi

SCRATCH="$(mktemp -d -t shire-smoke-XXXXXX)"
REPO="$SCRATCH/repo-a"
NOT_GIT="$SCRATCH/not-git"

cleanup() { rm -rf "$SCRATCH"; }
trap cleanup EXIT

# Build a real throwaway git repo we can point manifests at
mkdir -p "$REPO" "$NOT_GIT"
(cd "$REPO" \
    && git init -q \
    && git config user.email "t@t.x" \
    && git config user.name "t" \
    && echo "x" > README.md \
    && git add . \
    && git commit -q -m init)

# Results tracking
declare -a NAMES
declare -a RESULTS
declare -a NOTES

record() {
    NAMES+=("$1")
    RESULTS+=("$2")
    NOTES+=("$3")
}

say() {
    local status="$1" name="$2" note="${3:-}"
    local color reset="\033[0m"
    case "$status" in
        PASS) color="\033[32m" ;;
        FAIL) color="\033[31m" ;;
        SKIP) color="\033[33m" ;;
        *)    color="" ;;
    esac
    printf "  [${color}%4s${reset}] %s" "$status" "$name"
    [ -n "$note" ] && printf " — %s" "$note"
    printf "\n"
}

expect_exit() {
    local want="$1" got="$2" name="$3" note="${4:-}"
    if [ "$got" = "$want" ]; then
        say PASS "$name" "$note"
        record "$name" PASS "$note"
    else
        say FAIL "$name" "wanted exit $want, got $got. $note"
        record "$name" FAIL "wanted exit $want, got $got. $note"
    fi
}

expect_contains() {
    local haystack="$1" needle="$2" name="$3"
    if echo "$haystack" | grep -qF "$needle"; then
        say PASS "$name" "contains '$needle'"
        record "$name" PASS "contains '$needle'"
    else
        say FAIL "$name" "missing '$needle' in output"
        record "$name" FAIL "missing '$needle' in output"
    fi
}

# --------------------------------------------------------------------
echo "=== Agent Shire v0.1 — Part 1 offline tests ==="
echo "binary:  $("$SHIRE" version 2>/dev/null || echo '?')"
echo "scratch: $SCRATCH"
echo

# --------------------------------------------------------------------
# 1.1 Version
OUT=$("$SHIRE" version 2>&1); CODE=$?
if [ "$CODE" = "0" ] && [ "$OUT" = "shire 0.1.0" ]; then
    say PASS "1.1 version"
    record "1.1 version" PASS ""
else
    say FAIL "1.1 version" "got: $OUT (exit $CODE)"
    record "1.1 version" FAIL "got: $OUT"
fi

# --------------------------------------------------------------------
# 1.2 Help
"$SHIRE" --help >/dev/null 2>&1; A=$?
"$SHIRE" dispatch --help >/dev/null 2>&1; B=$?
"$SHIRE" validate --help >/dev/null 2>&1; C=$?
if [ "$A" = "0" ] && [ "$B" = "0" ] && [ "$C" = "0" ]; then
    say PASS "1.2 help"
    record "1.2 help" PASS ""
else
    say FAIL "1.2 help" "exit codes: top=$A dispatch=$B validate=$C"
    record "1.2 help" FAIL "exit codes: $A/$B/$C"
fi

# --------------------------------------------------------------------
# 1.3 Validate happy path
cat > "$SCRATCH/happy.toml" <<EOF
[[task]]
id = "smoke"
directory = "$REPO"
prompt = "hello"
EOF
OUT=$("$SHIRE" validate "$SCRATCH/happy.toml" 2>&1); CODE=$?
if [ "$CODE" = "0" ] && echo "$OUT" | grep -q "OK"; then
    say PASS "1.3 validate happy"
    record "1.3 validate happy" PASS ""
else
    say FAIL "1.3 validate happy" "exit $CODE: $OUT"
    record "1.3 validate happy" FAIL "exit $CODE"
fi

# --------------------------------------------------------------------
# 1.4 Validate unknown field
cat > "$SCRATCH/bad-key.toml" <<EOF
bogus_field = "surprise"
[[task]]
id = "x"
directory = "$REPO"
prompt = "p"
EOF
"$SHIRE" dispatch "$SCRATCH/bad-key.toml" >/dev/null 2>&1; CODE=$?
expect_exit 2 "$CODE" "1.4 validate unknown field" "must reject at parse"

# --------------------------------------------------------------------
# 1.5 Validate missing directory
cat > "$SCRATCH/bad-dir.toml" <<EOF
[[task]]
id = "x"
directory = "/absolutely/does/not/exist"
prompt = "p"
EOF
"$SHIRE" dispatch "$SCRATCH/bad-dir.toml" >/dev/null 2>&1; CODE=$?
expect_exit 2 "$CODE" "1.5 missing directory"

# --------------------------------------------------------------------
# 1.6 Non-git dir with default use_worktree=true
cat > "$SCRATCH/bad-nongit.toml" <<EOF
[[task]]
id = "x"
directory = "$NOT_GIT"
prompt = "p"
EOF
"$SHIRE" dispatch "$SCRATCH/bad-nongit.toml" >/dev/null 2>&1; CODE=$?
expect_exit 2 "$CODE" "1.6 non-git dir"

# --------------------------------------------------------------------
# 1.7 Duplicate task ids
cat > "$SCRATCH/bad-dups.toml" <<EOF
[[task]]
id = "same"
directory = "$REPO"
prompt = "a"

[[task]]
id = "same"
directory = "$REPO"
prompt = "b"
EOF
"$SHIRE" dispatch "$SCRATCH/bad-dups.toml" >/dev/null 2>&1; CODE=$?
expect_exit 2 "$CODE" "1.7 duplicate task ids"

# --------------------------------------------------------------------
# 1.8 Dry-run
OUT=$("$SHIRE" dispatch --dry-run "$SCRATCH/happy.toml" 2>&1); CODE=$?
if [ "$CODE" = "0" ] && echo "$OUT" | grep -q "DRY-RUN"; then
    say PASS "1.8 dry-run"
    record "1.8 dry-run" PASS ""
else
    say FAIL "1.8 dry-run" "exit $CODE: $OUT"
    record "1.8 dry-run" FAIL "exit $CODE"
fi

# --------------------------------------------------------------------
# 1.9 Missing claude binary — probe should fail with exit 2
SHIRE_CLAUDE_BINARY=/nope/claude "$SHIRE" dispatch "$SCRATCH/happy.toml" >/dev/null 2>&1; CODE=$?
expect_exit 2 "$CODE" "1.9 missing claude binary"

# --------------------------------------------------------------------
# 1.10 Concurrency precedence — manifest > env > default
#   Test by parsing "max_parallel=N" out of validate's OK line.
cat > "$SCRATCH/conc-default.toml" <<EOF
[[task]]
id = "a"
directory = "$REPO"
prompt = "p"
EOF
DEFAULT_OUT=$("$SHIRE" validate "$SCRATCH/conc-default.toml" 2>&1)
ENV_OUT=$(ANTHROPIC_MAX_CONCURRENT=7 "$SHIRE" validate "$SCRATCH/conc-default.toml" 2>&1)

cat > "$SCRATCH/conc-manifest.toml" <<EOF
[run]
max_parallel = 2
[[task]]
id = "a"
directory = "$REPO"
prompt = "p"
EOF
MANI_OUT=$(ANTHROPIC_MAX_CONCURRENT=7 "$SHIRE" validate "$SCRATCH/conc-manifest.toml" 2>&1)

DEFAULT_N=$(echo "$DEFAULT_OUT" | grep -oE 'max_parallel=[0-9]+' | head -1 | cut -d= -f2)
ENV_N=$(    echo "$ENV_OUT"     | grep -oE 'max_parallel=[0-9]+' | head -1 | cut -d= -f2)
MANI_N=$(   echo "$MANI_OUT"    | grep -oE 'max_parallel=[0-9]+' | head -1 | cut -d= -f2)

if [ "$DEFAULT_N" = "4" ] && [ "$ENV_N" = "7" ] && [ "$MANI_N" = "2" ]; then
    say PASS "1.10 concurrency precedence" "4/7/2 as expected"
    record "1.10 concurrency precedence" PASS "default=4 env=7 manifest=2"
else
    say FAIL "1.10 concurrency precedence" "got default=$DEFAULT_N env=$ENV_N manifest=$MANI_N"
    record "1.10 concurrency precedence" FAIL "got default=$DEFAULT_N env=$ENV_N manifest=$MANI_N"
fi

# --------------------------------------------------------------------
# Summary
echo
echo "=== Summary ==="
PASS_COUNT=0
FAIL_COUNT=0
for i in "${!NAMES[@]}"; do
    if [ "${RESULTS[$i]}" = "PASS" ]; then
        PASS_COUNT=$((PASS_COUNT + 1))
    else
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
done
printf "%d passed, %d failed (%d total)\n" "$PASS_COUNT" "$FAIL_COUNT" "${#NAMES[@]}"

if [ "$FAIL_COUNT" -gt 0 ]; then
    echo
    echo "Failures:"
    for i in "${!NAMES[@]}"; do
        if [ "${RESULTS[$i]}" != "PASS" ]; then
            printf "  - %s: %s\n" "${NAMES[$i]}" "${NOTES[$i]}"
        fi
    done
    exit 1
fi

echo
echo "Part 1 green. Proceed to Part 2 in docs/v0.1-smoke-test.md."
exit 0
