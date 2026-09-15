#!/usr/bin/env bash
# comm-session-skill.sh — print THE ONE session-start skill this session
# should run, as a `/slash-command` on stdout. Always exits 0. `--is-sot-repo`
# instead answers, by exit code only (no stdout), whether this session's
# project is a Ship of Tools checkout — the one piece of the old routing
# logic still load-bearing (comm-session-start.sh's gate for folding in the
# sot-specific layer). `--selftest` runs that detection matrix (exit 1 on
# any miss).
#
# Why (2026-07-25, Keith): the lifecycle hooks that tell a context-wiped session
# to re-bootstrap used to list all three skills and let the model pick. It picks
# WRONG — deciding in shell, where the workspace row and repo are knowable,
# removes the guess.
#
# 2026-09-15: collapsed further. The old backend-repo alias skill named only
# a retired launcher, which is gone too — every session now runs the same
# skill. The repo-identity check didn't go away with it: comm-session-start.sh
# still needs it, via `--is-sot-repo`, to decide whether a session gets the
# Ship-of-Tools layer (FE ping, bus sync).
#
# A session's identity is its row's handle everywhere, including on a Windows
# box (a frontend is a client, addressed by its own label for directed
# frontend commands, never a comm peer — retired 2026-09-14; see
# docs/adr/0042-first-class-local-sessions.md).
#
# Source of truth: comm/core/scripts/comm-session-skill.sh in Ship of Tools,
# deployed to ~/.sot-comm/bin by ShipTools.update_comm().
set -uo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

SKILL="/sot-session-start"

# The session's project directory. `$CLAUDE_PROJECT_DIR` is exported by Claude
# Code for hook processes and names the SESSION's project regardless of the
# hook's cwd — without it a session whose cwd wandered out of the checkout
# (verified: cwd=/tmp) is under-detected as generic.
_repo_dir() {
    if [ -n "${CLAUDE_PROJECT_DIR:-}" ]; then
        printf '%s' "$CLAUDE_PROJECT_DIR"
        return 0
    fi
    git rev-parse --show-toplevel 2>/dev/null || pwd
}

# Is DIR a Ship of Tools checkout? By repo IDENTITY, not directory name — the
# dirname test this replaced answered "is it SoT at all" WRONG for two real
# layouts (verified 2026-07-25):
#   - an agent worktree at `.claude/worktrees/agent-<hash>/`, whose basename
#     carries no repo name at all, resolved to the GENERIC skill despite being a
#     full SoT checkout sharing the SoT remote;
#   - any clone into a differently-named directory does the same.
# So ask git for the remote and compare the repo NAME exactly (after stripping
# `.git` and the owner/host), which is stable across worktrees, renames, forks,
# and both URL syntaxes. Exact-match, not substring: `ship-of-tools-ops` CONTAINS
# `ship-of-tools`, and the two are different repos that must be told apart. The
# ops sidecar is deliberately in the accepted set — it's the same project (the
# `/bus-sync` git bus lives there), so a session in it wants the sot-flavored
# backend bootstrap too.
# Fallbacks, in order: marker files (a remote-less or vendored checkout still
# has them), then the old dirname test (so nothing that used to be detected
# stops being detected).
_is_sot_repo() {
    local dir="${1:-}" url base
    [ -n "$dir" ] || return 1

    url="$(git -C "$dir" remote get-url origin 2>/dev/null || true)"
    if [ -z "$url" ]; then
        # No `origin` — take whatever remote exists (a worktree of a clone with
        # a differently-named remote, e.g. `upstream`).
        url="$(git -C "$dir" remote 2>/dev/null | head -n1 | while read -r r; do
                   git -C "$dir" remote get-url "$r" 2>/dev/null; done || true)"
    fi
    if [ -n "$url" ]; then
        base="${url%.git}"      # strip a trailing .git
        base="${base%/}"        # tolerate a trailing slash
        base="${base##*/}"      # https://host/owner/REPO  ->  REPO
        base="${base##*:}"      # git@host:REPO            ->  REPO
        case "$base" in
            ship-of-tools|ship-of-tools-ops) return 0 ;;
        esac
    fi

    # Marker files: the product repo's two load-bearing top-level docs.
    if [ -f "$dir/requirements.md" ] && [ -f "$dir/comm/PROTOCOL.md" ]; then
        return 0
    fi

    case "$(basename "$dir")" in
        ship-of-tools|ship-of-tools-wt-*|ship-of-tools-ops) return 0 ;;
    esac
    return 1
}

# `--is-sot-repo` — exit 0 if this session's project is a Ship of Tools
# checkout, 1 otherwise; no stdout. `$SOT_SESSION_ROLE` overrides, same as
# the old routing override did, for a topology the heuristics don't cover.
if [ "${1:-}" = "--is-sot-repo" ]; then
    case "${SOT_SESSION_ROLE:-}" in
        be|BE)           exit 0 ;;
        generic|GENERIC) exit 1 ;;
    esac
    _is_sot_repo "$(_repo_dir)"
    exit $?
fi

# `--selftest` — run the detection matrix against THIS copy of the script and
# exit nonzero on any miss. Same idiom as `comm-listen.sh --selftest`: runnable
# on any machine, so a topology that breaks a rule shows up as a failing case
# instead of a silently misrouted session. Every case below is a layout that
# either occurs in the fleet or previously misrouted (the two marked REGRESSION
# were real bugs, verified 2026-07-25). Checks `--is-sot-repo`'s exit code —
# the printed skill name no longer varies, so it's the live half of the old
# routing logic.
if [ "${1:-}" = "--selftest" ]; then
    self="$SELF_DIR/$(basename "${BASH_SOURCE[0]}")"
    # Resolve a SoT checkout to test against BY IDENTITY: the repo layout
    # (scripts -> core -> comm -> repo) when running from source, else the
    # current checkout — the deployed copy lives in ~/.sot-comm/bin, where the
    # relative walk lands on $HOME and would silently test the wrong tree.
    sot="$(cd "$SELF_DIR/../../.." 2>/dev/null && pwd || true)"
    _is_sot_repo "${sot:-}" || sot="$(git rev-parse --show-toplevel 2>/dev/null || true)"
    if ! _is_sot_repo "${sot:-}"; then
        echo "comm-session-skill --selftest: run this from inside a Ship of Tools checkout" >&2
        echo "  (the repo-dependent cases need one; cwd=$(pwd))" >&2
        exit 2
    fi
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    mkdir -p "$tmp/empty-state" "$tmp/renamed" "$tmp/decoy"
    git -C "$tmp/renamed" init -q 2>/dev/null
    git -C "$tmp/renamed" remote add origin https://example.invalid/any-owner/ship-of-tools.git 2>/dev/null
    git -C "$tmp/decoy" init -q 2>/dev/null
    git -C "$tmp/decoy" remote add origin https://example.invalid/any-owner/ship-of-tools-plugins.git 2>/dev/null
    _p=0; _f=0
    _case() { # desc expected actual — actual is an --is-sot-repo exit code
        # ("0"/"1") for every case but the last, which checks the (now
        # constant) printed skill name instead.
        if [ "$2" = "$3" ]; then _p=$((_p+1)); printf '  ok    %-52s -> %s\n' "$1" "$3"
        else _f=$((_f+1)); printf '  FAIL  %-52s -> %s (want %s)\n' "$1" "$3" "$2"; fi
    }
    echo "comm-session-skill selftest (repo: $sot)"
    _case "SoT: repo" 0 "$( (cd "$sot" && "$self" --is-sot-repo); echo $? )"
    # A REAL secondary worktree if this checkout has one — its basename can carry
    # no repo name at all (`.claude/worktrees/agent-<hash>`) yet it must still
    # be detected as SoT. Skipped, not faked, when none exists: a made-up
    # path would exercise nothing (no gitdir, no markers).
    _wt="$(git -C "$sot" worktree list --porcelain 2>/dev/null | awk '/^worktree /{print $2}' | sed -n '2p')"
    if [ -n "${_wt:-}" ] && [ -d "${_wt:-}" ]; then
        _case "SoT: real worktree, odd basename (REGRESSION)" 0 \
              "$( (CLAUDE_PROJECT_DIR="$_wt" "$self" --is-sot-repo); echo $? )"
    else
        printf '  skip  %-52s (no secondary worktree here)\n' "SoT: real worktree, odd basename"
    fi
    _case "SoT: clone in renamed dir (remote identity)" 0 \
          "$( (CLAUDE_PROJECT_DIR="$tmp/renamed" "$self" --is-sot-repo); echo $? )"
    _case "SoT: capsule row on a Windows box (REGRESSION)" 0 \
          "$( (cd "$sot" && SOT_WORKSPACE_ID=ws-row-1 OS=Windows_NT XDG_STATE_HOME="$tmp/empty-state" "$self" --is-sot-repo); echo $? )"
    # A Windows session in a capsule row is just a session, row or no row —
    # there is no more frontend-driver role to fall into (retired
    # 2026-09-14: a frontend is a client, never a comm peer).
    _case "SoT: Windows, no row (REGRESSION)" 0 \
          "$( (cd "$sot" && env -u SOT_WORKSPACE_ID OS=Windows_NT XDG_STATE_HOME="$tmp/empty-state" "$self" --is-sot-repo); echo $? )"
    _case "non-SoT: windows, no row, decoy repo" 1 \
          "$( (cd "$tmp/decoy" && env -u SOT_WORKSPACE_ID OS=Windows_NT XDG_STATE_HOME="$tmp/empty-state" "$self" --is-sot-repo); echo $? )"
    _case "non-SoT: plain non-repo dir" 1 "$( (cd "$tmp" && "$self" --is-sot-repo); echo $? )"
    _case "non-SoT: decoy repo ship-of-tools-plugins" 1 \
          "$( (CLAUDE_PROJECT_DIR="$tmp/decoy" "$self" --is-sot-repo); echo $? )"
    _case "override: SOT_SESSION_ROLE=generic in SoT" 1 \
          "$( (cd "$sot" && SOT_SESSION_ROLE=generic "$self" --is-sot-repo); echo $? )"
    _case "skill name is constant regardless of repo" "$SKILL" "$(cd "$tmp/decoy" && "$self")"
    echo
    echo "comm-session-skill selftest: passed=$_p failed=$_f"
    [ "$_f" -eq 0 ] || exit 1
    exit 0
fi

echo "$SKILL"
