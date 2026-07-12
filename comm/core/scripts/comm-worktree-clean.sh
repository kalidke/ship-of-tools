#!/usr/bin/env bash
# comm-worktree-clean.sh — tear down a FINISHED worktree: remove the worktree,
# delete its branch, and despawn its session. Run from the parent checkout (or any
# worktree of the same repo).
#
# It does NOT merge for you — merge the branch to the base branch first (a normal
# merge/PR). By default it REFUSES if the branch isn't fully merged into the base
# branch (so you can't silently drop unmerged commits); pass --force to remove
# anyway (you'll lose unmerged work + the branch is force-deleted).
#
# Usage: comm-worktree-clean.sh <shortname> [--force] [--keep-session]
#   --force          remove even if the branch isn't merged (git worktree remove
#                    --force, git branch -D).
#   --keep-session   don't despawn the worktree's sot-comm session.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
eval "$("$SCRIPT_DIR/comm-context.sh")"   # REPO, HOST

SHORT=""; FORCE=false; KEEP_SESSION=false
while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help)       sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        --force)         FORCE=true; shift ;;
        --keep-session)  KEEP_SESSION=true; shift ;;
        -*)              echo "comm-worktree-clean.sh: unknown option '$1' (see --help)" >&2; exit 2 ;;
        *)               [ -z "$SHORT" ] && SHORT="$1"; shift ;;
    esac
done
[ -n "$SHORT" ] || { echo "comm-worktree-clean.sh: missing <shortname> (see --help)" >&2; exit 2; }

TOP="$(git rev-parse --show-toplevel 2>/dev/null)" || { echo "comm-worktree-clean.sh: not in a git repo" >&2; exit 1; }
BASE="${REPO%%-wt-*}"
HANDLE="${BASE}-wt-${SHORT}"

# The sessions-list workspace LABEL may deliberately differ from the comm HANDLE
# (display-prefix decoupling — comm-worktree-new.sh). Derive it the SAME way here
# so teardown can target the workspace by label as a fallback. Default prefix is
# the repo basename; a committed .sot/worktree.toml `display_prefix` (e.g. ".SoT")
# pins it durably. comm-despawn.sh now recovers the slug from the agent's registry
# row too, so the primary despawn-by-HANDLE already works; this LABEL is the
# belt-and-suspenders path for when the registry row is already gone.
DISPLAY_PREFIX="$BASE"
if [ -f "$TOP/.sot/worktree.toml" ]; then
    _rl="$(grep -E '^[[:space:]]*display_prefix[[:space:]]*=' "$TOP/.sot/worktree.toml" \
        | head -1 | sed -E 's/^[^=]*=[[:space:]]*//; s/^"//; s/"[[:space:]]*$//; s/[[:space:]]*$//')"
    [ -n "$_rl" ] && DISPLAY_PREFIX="$_rl"
fi
LABEL="${DISPLAY_PREFIX}-wt-${SHORT}"

# Locate the worktree by its dir basename (robust to where it lives) via the
# porcelain list; capture its branch too.
WT=""; BRANCH=""
path=""; br=""
while IFS= read -r line; do
    case "$line" in
        "worktree "*) path="${line#worktree }" ;;
        "branch "*)   br="${line#branch refs/heads/}" ;;
        "detached")   br="" ;;
        "")
            if [ "$(basename "${path:-}")" = "$HANDLE" ]; then WT="$path"; BRANCH="$br"; fi
            path=""; br=""
            ;;
    esac
done < <(git worktree list --porcelain; printf '\n')

[ -n "$WT" ] || { echo "comm-worktree-clean.sh: no worktree named '$HANDLE' (git worktree list)" >&2; exit 2; }

# Base branch + merged check.
BASEBR=""
for b in main master; do git show-ref --verify --quiet "refs/heads/$b" && { BASEBR="$b"; break; }; done

if [ "$FORCE" != true ]; then
    [ -n "$BRANCH" ] || { echo "comm-worktree-clean.sh: '$HANDLE' is detached (no branch) — pass --force to remove" >&2; exit 2; }
    [ -n "$BASEBR" ] || { echo "comm-worktree-clean.sh: no main/master base branch to check merge against — pass --force if you're sure" >&2; exit 2; }
    if ! git merge-base --is-ancestor "$BRANCH" "$BASEBR" 2>/dev/null; then
        echo "comm-worktree-clean.sh: branch '$BRANCH' is NOT merged into '$BASEBR' — refusing." >&2
        echo "  merge it first (git merge $BRANCH / PR), then re-run; or --force to drop unmerged work." >&2
        exit 2
    fi
fi

echo "removing worktree $WT (branch '${BRANCH:-detached}', session @$HANDLE)…"
if [ "$FORCE" = true ]; then
    git worktree remove --force "$WT"
else
    git worktree remove "$WT"
fi
if [ -n "$BRANCH" ]; then
    if [ "$FORCE" = true ]; then git branch -D "$BRANCH" 2>&1 || true; else git branch -d "$BRANCH" 2>&1 || true; fi
fi
if [ "$KEEP_SESSION" != true ]; then
    # Primary: despawn by HANDLE — deregisters the comm agent AND (via
    # comm-despawn's registry-slug recovery) destroys the workspace even when the
    # label/slug differs from the handle.
    "$SCRIPT_DIR/comm-despawn.sh" "$HANDLE" 2>&1 | tail -3 || echo "  (despawn @$HANDLE: not running / already gone)"
    # Belt-and-suspenders: if the LABEL differs from the HANDLE, also target it
    # directly, covering the edge where the agent's registry row was already gone
    # so the handle pass couldn't recover the slug. Idempotent — a no-op ("Nothing
    # to destroy") if the handle pass already removed it.
    if [ "$LABEL" != "$HANDLE" ]; then
        "$SCRIPT_DIR/comm-despawn.sh" "$LABEL" 2>&1 | tail -2 || true
    fi
fi
echo "cleaned: worktree removed${BRANCH:+, branch $BRANCH deleted}$([ "$KEEP_SESSION" = true ] && echo "" || echo ", session @$HANDLE despawned")."
