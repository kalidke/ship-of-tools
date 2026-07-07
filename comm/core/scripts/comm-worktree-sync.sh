#!/usr/bin/env bash
# comm-worktree-sync.sh — remind a repo's parent + worktree sessions of each other
# and nudge them to compare progress and sync (rebase/merge). Run it from ANY
# session in the family (the parent/main checkout or a worktree).
#
# It finds the family in the sot-comm registry by base repo name: the parent
# (handle `<base>` or `<base>-<host>`) and every worktree (`<base>-wt-*`). The
# `<base>-` prefix match uses a trailing dash so e.g. base `pkg` never sweeps in
# `pkg-analysis`. Each family member (except the caller) is pinged with the roster
# plus a `git worktree list` and an ahead/behind-vs-main summary so they know the
# sync state.
#
# Usage: comm-worktree-sync.sh [--message "extra note"]
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

NOTE=""
while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help)     sed -n '2,14p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        --message)     NOTE="$2"; shift 2 ;;
        --message=*)   NOTE="${1#--message=}"; shift ;;
        *)             echo "comm-worktree-sync.sh: unknown arg '$1' (see --help)" >&2; exit 2 ;;
    esac
done

# Context: REPO (current checkout basename), NAME (self handle), REGISTRY path.
eval "$("$SCRIPT_DIR/comm-context.sh")"

git rev-parse --show-toplevel >/dev/null 2>&1 \
    || { echo "comm-worktree-sync.sh: not inside a git repo" >&2; exit 1; }

# Base repo name: strip a `-wt-<short>` suffix if we're inside a worktree.
BASE="${REPO%%-wt-*}"
SELF="${NAME:-}"

command -v jq >/dev/null 2>&1 || { echo "comm-worktree-sync.sh: jq required" >&2; exit 1; }
[ -f "$REGISTRY" ] || { echo "comm-worktree-sync.sh: no registry at $REGISTRY" >&2; exit 1; }

# Family = exact base OR `<base>-...` (dash-guarded so pkg != pkg-analysis).
mapfile -t FAMILY < <(jq -r --arg b "$BASE" '
    .agents // {} | keys[] | select(. == $b or startswith($b + "-"))
' "$REGISTRY" 2>/dev/null || true)

if [ "${#FAMILY[@]}" -eq 0 ]; then
    echo "comm-worktree-sync.sh: no sessions found for base repo '$BASE' in the registry" >&2
    exit 1
fi

# Sync-state context: the worktree layout + ahead/behind vs the base branch.
WT_LIST="$(git worktree list 2>/dev/null || true)"
AHEAD_BEHIND=""
for ref in main master; do
    if git rev-parse --verify --quiet "$ref" >/dev/null; then
        if counts="$(git rev-list --left-right --count "$ref"...HEAD 2>/dev/null)"; then
            AHEAD_BEHIND="vs $ref: $(awk '{print $1" behind, "$2" ahead"}' <<<"$counts")"
        fi
        break
    fi
done

ROSTER="$(printf '%s ' "${FAMILY[@]}")"
MSG="[worktree-sync] $BASE family: ${ROSTER}— from @${SELF:-?}. Let's compare progress and flag any rebase/merge/sync needs.${NOTE:+ $NOTE}
${AHEAD_BEHIND:+($AHEAD_BEHIND)
}worktrees:
$WT_LIST"

pinged=0
for h in "${FAMILY[@]}"; do
    [ -n "$h" ] || continue
    [ "$h" = "$SELF" ] && continue
    "$SCRIPT_DIR/comm-relay.sh" send "@$h" "$MSG" 2>/dev/null || true
    pinged=$((pinged + 1))
done

echo "worktree-sync: base '$BASE', family [${ROSTER}], pinged $pinged (excluding @${SELF:-self})"
