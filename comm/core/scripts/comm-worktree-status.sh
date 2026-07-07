#!/usr/bin/env bash
# comm-worktree-status.sh — show the current repo's worktree family and whether
# each is current / done / ready to clean up. Run from the parent checkout OR any
# worktree (git worktree list sees them all — they share one .git).
#
# Per worktree it shows: branch, behind/ahead vs the base branch, whether the
# branch is MERGED into base (so removing it loses nothing), and the owning
# session's work-state from the registry.
#
# READY TO CLEAN UP = merged=yes AND the session is idle/done. To clean up: merge
# the branch to the base branch yourself (a normal merge/PR), then
# `comm-worktree-clean.sh <shortname>` (which removes the worktree + branch +
# despawns the session).
#
# Usage: comm-worktree-status.sh
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
eval "$("$SCRIPT_DIR/comm-context.sh")"   # REPO, NAME, HOST, REGISTRY

git rev-parse --show-toplevel >/dev/null 2>&1 || { echo "comm-worktree-status.sh: not in a git repo" >&2; exit 1; }
BASE="${REPO%%-wt-*}"

# Base branch to compare against.
BASEBR=""
for b in main master; do
    git show-ref --verify --quiet "refs/heads/$b" && { BASEBR="$b"; break; }
done

HAVE_JQ=0; command -v jq >/dev/null 2>&1 && HAVE_JQ=1
reg_state() {  # handle -> "state · summary" (or "-")
    { [ "$HAVE_JQ" = 1 ] && [ -f "$REGISTRY" ]; } || { printf -- '-'; return; }
    jq -r --arg h "$1" '
        .agents[$h] // empty
        | ((.state // "-") + (if (.summary // "") != "" then " · " + .summary else "" end))
    ' "$REGISTRY" 2>/dev/null | head -1 | grep . || printf 'not-joined'
}

printf "worktree family for '%s'  (base branch: %s)\n" "$BASE" "${BASEBR:-none}"
printf "%-30s %-16s %-12s %-7s %s\n" HANDLE BRANCH BEHIND/AHEAD MERGED SESSION
printf "%-30s %-16s %-12s %-7s %s\n" "------" "------" "-----------" "------" "-------"

found=0
path=""; branch=""
while IFS= read -r line; do
    case "$line" in
        "worktree "*) path="${line#worktree }" ;;
        "branch "*)   branch="${line#branch refs/heads/}" ;;
        "detached")   branch="(detached)" ;;
        "")  # end of one porcelain record
            [ -n "$path" ] || continue
            name="$(basename "$path")"
            case "$name" in
                "$BASE"-wt-*)
                    found=$((found + 1))
                    ba="-"; merged="?"
                    if [ -n "$BASEBR" ] && [ "$branch" != "(detached)" ] && [ -n "$branch" ]; then
                        if c="$(git rev-list --left-right --count "$BASEBR...$branch" 2>/dev/null)"; then
                            ba="$(awk '{print $1"/"$2}' <<<"$c")"
                        fi
                        if git merge-base --is-ancestor "$branch" "$BASEBR" 2>/dev/null; then
                            merged="yes"; else merged="no"; fi
                    fi
                    printf "%-30s %-16s %-12s %-7s %s\n" "$name" "${branch:-?}" "$ba" "$merged" "$(reg_state "$name")"
                    ;;
            esac
            path=""; branch=""
            ;;
    esac
done < <(git worktree list --porcelain; printf '\n')

if [ "$found" -eq 0 ]; then
    echo "(no worktrees — create one with comm-worktree-new.sh <shortname>)"
else
    echo
    echo "ready to clean = MERGED=yes + session idle/done → merge that branch to ${BASEBR:-main}, then: comm-worktree-clean.sh <shortname>"
fi
