#!/usr/bin/env bash
# comm-worktree-new.sh — create a git worktree of the current repo and spawn a
# parallel sot-comm session bound to it.
#
# The spawned session's comm HANDLE and on-disk worktree DIR are both
# `<repo>-wt-<shortname>`. Its frontend workspace LABEL is `<prefix>-wt-<shortname>`
# where <prefix> defaults to the repo basename but can be pinned via a committed
# `.sot/worktree.toml` (`display_prefix`) or `--display-prefix` — e.g. ".SoT". The
# daemon slugs the label (paths::slug → lowercased, dashes kept, '.'→'_') and the
# sessions list sorts by slug, so the worktree groups next to its parent with NO
# frontend change. Grouping is achieved purely by naming; handle/dir stay
# repo-based so status/clean/sync still group by the real repo.
#
# The spawned session is told its parent + sibling worktrees so they can sync;
# the parent is also notified. Run `comm-worktree-sync.sh` from any session in the
# family to re-remind everyone to share progress + sync.
#
# Usage:
#   comm-worktree-new.sh <shortname> [--base <ref>] [--branch <name>]
#                        [--task "..."] [--expertise "a, b"] [--no-spawn]
#
#   <shortname>     lowercase [a-z0-9-], <=64 chars. Names the worktree; the
#                   session becomes <repo>-wt-<shortname>.
#   --base <ref>    git ref to branch FROM (default: HEAD of the current repo).
#   --branch <name> branch to CREATE for the worktree (default: wt/<shortname>).
#   --task "..."    initial instruction handed to the spawned session.
#   --expertise "..." comma-separated expertise tags for the spawned session.
#   --no-spawn      create the worktree only; don't spawn a session.
#   --no-symlinks   don't replicate the source checkout's symlinks (see below).
#   --display-prefix L  override the sessions-list LABEL prefix (default: repo
#                   basename; a committed .sot/worktree.toml `display_prefix` makes it
#                   durable). e.g. ".SoT" -> label ".SoT-wt-<short>" sorts left.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() { sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }

SHORT=""; BASE_REF=""; BRANCH=""; TASK=""; EXPERTISE=""; SPAWN=true; SYMLINKS=true; DISPLAY_PREFIX_FLAG=""
while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help)        usage; exit 0 ;;
        --no-symlinks)    SYMLINKS=false; shift ;;
        --display-prefix)     DISPLAY_PREFIX_FLAG="$2"; shift 2 ;;
        --display-prefix=*)   DISPLAY_PREFIX_FLAG="${1#--display-prefix=}"; shift ;;
        --base)         BASE_REF="$2"; shift 2 ;;
        --base=*)       BASE_REF="${1#--base=}"; shift ;;
        --branch)       BRANCH="$2"; shift 2 ;;
        --branch=*)     BRANCH="${1#--branch=}"; shift ;;
        --task)         TASK="$2"; shift 2 ;;
        --task=*)       TASK="${1#--task=}"; shift ;;
        --expertise)    EXPERTISE="$2"; shift 2 ;;
        --expertise=*)  EXPERTISE="${1#--expertise=}"; shift ;;
        --no-spawn)     SPAWN=false; shift ;;
        -*)             echo "comm-worktree-new.sh: unknown option '$1' (see --help)" >&2; exit 2 ;;
        *)              [ -z "$SHORT" ] && SHORT="$1"; shift ;;
    esac
done

[ -n "$SHORT" ] || { echo "comm-worktree-new.sh: missing <shortname> (see --help)" >&2; exit 2; }

# Context: NAME (this/spawner session handle, host-agnostic), HOST. The base repo
# is the current checkout (CWD).
eval "$("$SCRIPT_DIR/comm-context.sh")"

TOP="$(git rev-parse --show-toplevel 2>/dev/null)" \
    || { echo "comm-worktree-new.sh: not inside a git repo" >&2; exit 1; }
REPO="$(basename "$TOP")"
PARENT="$(dirname "$TOP")"

# Validate <shortname> strictly — it becomes a handle, a frontend label/slug, a
# directory component, and a branch suffix. Lowercase keeps the slug predictable
# on case-insensitive filesystems.
case "$SHORT" in
    *[!a-z0-9-]* | [!a-z0-9]* | "")
        echo "comm-worktree-new.sh: <shortname> must match ^[a-z0-9][a-z0-9-]*$ (lowercase)" >&2; exit 2 ;;
esac
[ "${#SHORT}" -le 64 ] || { echo "comm-worktree-new.sh: <shortname> too long (<=64)" >&2; exit 2; }

HANDLE="${REPO}-wt-${SHORT}"
WT="${PARENT}/worktrees/${HANDLE}"

# Worktree workspace LABEL prefix — the name shown in the sessions list and its
# sort slug. Defaults to the repo basename, but a repo can pin a DURABLE label
# via a committed `.sot/worktree.toml` (`display_prefix = "..."`), or a one-off
# `--display-prefix`. e.g. display_prefix=".SoT" -> label ".SoT-wt-<short>" -> slug
# "_sot-wt-<short>" (leading '_' < any letter) -> sorts left, by the default row.
# The comm HANDLE and worktree DIR stay repo-based on purpose, so status/clean/
# sync still group by the real repo — only the displayed label changes.
DISPLAY_PREFIX="$REPO"
if [ -n "$DISPLAY_PREFIX_FLAG" ]; then
    DISPLAY_PREFIX="$DISPLAY_PREFIX_FLAG"
elif [ -f "$TOP/.sot/worktree.toml" ]; then
    _rl="$(grep -E '^[[:space:]]*display_prefix[[:space:]]*=' "$TOP/.sot/worktree.toml" \
        | head -1 | sed -E 's/^[^=]*=[[:space:]]*//; s/^"//; s/"[[:space:]]*$//; s/[[:space:]]*$//')"
    [ -n "$_rl" ] && DISPLAY_PREFIX="$_rl"
fi
LABEL="${DISPLAY_PREFIX}-wt-${SHORT}"
BRANCH="${BRANCH:-wt/${SHORT}}"
BASE_REF="${BASE_REF:-HEAD}"

# Validate the branch name and the base ref BEFORE touching anything.
git check-ref-format --branch "$BRANCH" >/dev/null 2>&1 \
    || { echo "comm-worktree-new.sh: invalid branch name '$BRANCH'" >&2; exit 2; }
BASE_COMMIT="$(git rev-parse --verify --quiet "${BASE_REF}^{commit}")" \
    || { echo "comm-worktree-new.sh: base ref '$BASE_REF' does not resolve to a commit" >&2; exit 2; }

# Fail loudly (never --force) on collisions — let the user inspect/clean up.
if git show-ref --verify --quiet "refs/heads/$BRANCH"; then
    echo "comm-worktree-new.sh: branch '$BRANCH' already exists — pass --branch or remove it (git branch -D $BRANCH)" >&2
    exit 2
fi
if [ -e "$WT" ]; then
    echo "comm-worktree-new.sh: worktree path already exists: $WT (git worktree list)" >&2
    exit 2
fi

# Uncommitted changes in the base checkout are NOT carried into the new worktree.
if ! git diff --quiet --ignore-submodules HEAD 2>/dev/null; then
    echo "note: uncommitted changes in $REPO are NOT carried into the worktree (it starts at ${BASE_REF}=${BASE_COMMIT:0:12})" >&2
fi

mkdir -p "${PARENT}/worktrees"
git worktree add "$WT" -b "$BRANCH" "$BASE_REF"
echo "worktree: $WT  (branch '$BRANCH' @ ${BASE_COMMIT:0:12})"

# Replicate the source checkout's working-tree symlinks into the new worktree.
# Data-analysis repos symlink e.g. data/results -> external storage, data/raw/* -> external storage; these
# are GITIGNORED and set up per-checkout, so `git worktree add` does NOT carry
# them — without them a worktree session can't reach external storage data or write results
# where the main repo does. Recreate any symlink that exists in the source but is
# missing in the worktree, preserving the LITERAL target (absolute external storage targets
# resolve the same; relative targets stay relative to their dir). (--no-symlinks
# to skip.)
if [ "$SYMLINKS" = true ]; then
    linked=0
    while IFS= read -r -d '' src; do
        rel="${src#"$TOP"/}"
        dest="$WT/$rel"
        if [ -e "$dest" ] || [ -L "$dest" ]; then continue; fi   # git already made it
        tgt="$(readlink "$src")" || continue
        mkdir -p "$(dirname "$dest")"
        if ln -s "$tgt" "$dest" 2>/dev/null; then
            linked=$((linked + 1))
            echo "  symlink: $rel -> $tgt"
        fi
    done < <(find "$TOP" -type l \
        -not -path '*/.git/*' -not -path '*/node_modules/*' -not -path '*/target/*' \
        -not -path '*/worktrees/*' -not -path '*/.claude/*' \
        -print0 2>/dev/null)
    if [ "$linked" -gt 0 ]; then
        echo "replicated $linked symlink(s) from the source checkout (e.g. external storage data/results)"
    fi
fi

[ "$LABEL" != "$HANDLE" ] && echo "label: $LABEL  (sessions-list display + sort; comm handle stays $HANDLE)"

if [ "$SPAWN" != true ]; then
    echo "(--no-spawn) session not started; to spawn later:"
    echo "  comm-spawn.sh '$HANDLE' '$WT' --display-label '$LABEL'"
    exit 0
fi

# The parent/main session this worktree should report to is whoever spawned it.
# Host-agnostic: use the real handle if we're a joined session, else the bare repo.
PARENT_HANDLE="${NAME:-$REPO}"
FULL_TASK="${TASK:+$TASK

}You are a WORKTREE session of repo '$REPO' (branch '$BRANCH', dir $WT). Your PARENT/main session is @${PARENT_HANDLE}. Coordinate progress and syncing (rebase/merge) with it and any sibling @${REPO}-wt-* sessions. Run comm-worktree-sync.sh (the /worktree sync skill) to remind the family to compare progress and flag merge/rebase needs. Commit on '$BRANCH'; surface merge-readiness to @${PARENT_HANDLE}."

# --display-label (not --label): the worktree LABEL is a deliberate
# '<prefix>-wt-<short>' that may differ from the repo-based HANDLE (e.g. the
# '.SoT-wt-<short>' grouping prefix). comm-spawn's --label is guarded to the repo
# basename; --display-label is the explicit decouple (handle stays repo-based).
"$SCRIPT_DIR/comm-spawn.sh" "$HANDLE" "$WT" --display-label "$LABEL" \
    ${EXPERTISE:+--expertise "$EXPERTISE"} --task "$FULL_TASK"
echo "spawned session @$HANDLE bound to $WT"

# Notify the parent/main (skip if the spawner IS the parent and equals HANDLE,
# or if there is no joined identity to message from).
if [ -n "${NAME:-}" ] && [ "$NAME" != "$HANDLE" ]; then
    "$SCRIPT_DIR/comm-relay.sh" send "@$PARENT_HANDLE" \
        "[worktree] spawned @$HANDLE at $WT (branch '$BRANCH' @ ${BASE_COMMIT:0:12}). It will coordinate progress/syncing with you; run /worktree sync to re-remind the family." \
        2>/dev/null || true
fi
