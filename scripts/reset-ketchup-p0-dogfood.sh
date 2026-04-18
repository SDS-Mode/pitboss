#!/usr/bin/env bash
# Reset the ketchup-pitboss dogfood to the baseline tag so we can re-run
# examples/ketchup-p0-execute.toml from a clean slate.
#
# What this does:
#   1. Removes every `ketchup-pitboss-{worker,lead}-*` worktree pitboss
#      has spawned under /run/media/system/Dos/Projects/.
#   2. Deletes any `demo/ketchup-p0-*` branches those worktrees pointed at.
#   3. Resets the main ketchup checkout to the baseline tag
#      `dogfood/p0-baseline`.
#   4. Leaves the tag in place so subsequent resets keep working.
#
# Safe to run repeatedly. Nothing is pushed to origin.
#
# After this, re-dispatch with:
#   ./target/release/pitboss dispatch examples/ketchup-p0-execute.toml
#
# Usage:
#   scripts/reset-ketchup-p0-dogfood.sh

set -euo pipefail

KETCHUP_DIR="/run/media/system/Dos/Projects/ketchup"
BASELINE_TAG="dogfood/p0-baseline"
# Any worktree whose checked-out branch starts with one of these is a
# pitboss-dogfood worktree. Multiple prefixes to cover old + current
# naming conventions (`demo/ketchup-p0-*` today; possible future adds).
BRANCH_PREFIXES=("demo/ketchup-p0-")

if [[ ! -d "$KETCHUP_DIR/.git" ]]; then
    echo "error: $KETCHUP_DIR is not a git repository" >&2
    exit 1
fi

cd "$KETCHUP_DIR"

if ! git rev-parse -q --verify "refs/tags/$BASELINE_TAG" >/dev/null; then
    echo "error: tag $BASELINE_TAG does not exist in $KETCHUP_DIR" >&2
    echo "       create it first with: git tag $BASELINE_TAG <commit>" >&2
    exit 1
fi

echo "== ketchup baseline: $(git rev-parse --short "$BASELINE_TAG")"

# Discover + remove pitboss-managed worktrees. Match by BRANCH prefix
# rather than path — old dogfood runs used different path conventions
# (ketchup-p0-N-lead vs ketchup-pitboss-*) and we want to clean up all
# of them. --force so uncommitted worker changes are blown away;
# that's the whole point of the reset.
mapfile -t worktrees < <(git worktree list --porcelain | awk -v prefixes="$(IFS='|'; echo "${BRANCH_PREFIXES[*]}")" '
    BEGIN { split(prefixes, ps, "|") }
    /^worktree / { path = $2 }
    /^branch refs\/heads\// {
        br = substr($0, length("branch refs/heads/") + 1)
        for (i in ps) if (index(br, ps[i]) == 1) { print path; break }
    }
')

if [[ ${#worktrees[@]} -eq 0 ]]; then
    echo "no pitboss worktrees to remove"
else
    for wt in "${worktrees[@]}"; do
        echo "-- removing worktree: $wt"
        git worktree remove --force "$wt" 2>/dev/null || {
            echo "   fallback: rm -rf $wt"
            rm -rf "$wt"
        }
    done
    git worktree prune
fi

# Delete demo/ketchup-p0-* local branches. -D since the branches diverge
# from main via uncommitted-in-worktree changes that are now gone.
# Use for-each-ref to get clean branch names across all prefixes.
mapfile -t branches < <(
    for prefix in "${BRANCH_PREFIXES[@]}"; do
        git for-each-ref --format='%(refname:short)' "refs/heads/${prefix}*"
    done
)
if [[ ${#branches[@]} -eq 0 ]]; then
    echo "no demo/ketchup-p0-* branches to delete"
else
    for b in "${branches[@]}"; do
        echo "-- deleting branch: $b"
        git branch -D "$b"
    done
fi

# Reset the main checkout (whatever branch it's on) back to baseline.
# If it's on a non-main branch like feature/pitboss-refactor-analysis,
# this still works — we hard-reset that branch to the tag.
current_branch=$(git branch --show-current)
if [[ -z "$current_branch" ]]; then
    echo "warn: ketchup HEAD is detached; leaving as-is"
else
    echo "-- resetting $current_branch to $BASELINE_TAG"
    git reset --hard "$BASELINE_TAG"
fi

echo
echo "== reset complete"
git worktree list
