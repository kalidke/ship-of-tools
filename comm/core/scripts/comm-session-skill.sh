#!/usr/bin/env bash
# comm-session-skill.sh — print THE ONE session-start skill this session should
# run, as a `/slash-command` on stdout. Always exits 0 with a usable answer.
# `--selftest` runs the detection matrix instead (exit 1 on any miss).
#
# Why (2026-07-25, Keith): the lifecycle hooks that tell a context-wiped session
# to re-bootstrap used to list all three skills and let the model pick. It picks
# WRONG — observed this session: a `/clear`ed BACKEND session on the Linux box
# was pointed at `/sot-fe-session-start`, whose steps (win-fe handle, tcp tunnel
# to a local forward port, `fe-inbox.jsonl` Monitor) describe a machine it isn't
# on. Nothing in the FE skill's own steps fails loudly there, so the wrong-skill
# run can leave a session believing it bootstrapped when it didn't. Deciding in
# shell — where the handle, platform, tmux context, and repo are all knowable —
# removes the guess.
#
# Detection, most-specific first (each rule notes the failure it exists to stop):
#
#   0. `$SOT_SESSION_ROLE` (fe|be|generic) — explicit override for a topology the
#      heuristics don't cover. Nothing below can contradict it.
#   1. handle starts with `win-fe` -> FRONTEND. The FE's own Rust
#      `self_comm_handle()` (gpu.rs) formats `win-fe-<lowercased host>`, and the
#      FE skill derives the same handle in lockstep, so this prefix is definitive
#      whenever the session has joined or exported SOT_COMM_NAME.
#   2. no tmux AND an `fe-inbox.jsonl` exists -> FRONTEND. This box runs an FE.
#      Both halves are required: the inbox alone would misfire for a backend tmux
#      session on a box that also runs a local FE; BE sessions always live in
#      tmux (comm-context is keyed by tmux pane id), so the tmux test separates
#      them.
#   3. no tmux AND Windows AND the repo is Ship of Tools -> FRONTEND. Covers the
#      COLD FE — `fe-inbox.jsonl` is created LAZILY, by the first `agent.message`
#      the FE receives (gpu.rs::append_agent_message opens it with `create(true)`
#      on append), so a freshly-installed frontend that has never been messaged
#      has NO inbox file and rule 2 misses it. Without this rule that FE fell
#      through to the repo test and was told to run the BACKEND bootstrap — the
#      exact misroute this script exists to prevent. Scoped to the SoT repo so a
#      plain Windows session in some unrelated checkout stays generic.
#   4. the repo is Ship of Tools -> `/sot-be-session-start`, the sot-flavored
#      backend superset.
#   5. anything else -> `/sot-session-start`, the project-agnostic bootstrap.
#
# "Is it Ship of Tools at all" is decided by repo IDENTITY, not directory name —
# see `_is_sot_repo`.
#
# The fe-inbox path mirrors gpu.rs::sot_state_dir() exactly: `%LOCALAPPDATA%\sot`
# on Windows, `$XDG_STATE_HOME/sot` (or `$HOME/.local/state/sot`) elsewhere.
#
# Source of truth: comm/core/scripts/comm-session-skill.sh in Ship of Tools,
# deployed to ~/.sot-comm/bin by ShipTools.update_comm().
set -uo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

FE_SKILL="/sot-fe-session-start"
BE_SKILL="/sot-be-session-start"
GENERIC_SKILL="/sot-session-start"

_is_windows() {
    case "${OS:-}" in Windows_NT) return 0 ;; esac
    case "${OSTYPE:-}" in msys*|cygwin*|win32) return 0 ;; esac
    case "$(uname -s 2>/dev/null || true)" in MINGW*|MSYS*|CYGWIN*) return 0 ;; esac
    return 1
}

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

# `--selftest` — run the detection matrix against THIS copy of the script and
# exit nonzero on any miss. Same idiom as `comm-listen.sh --selftest`: runnable
# on any machine, so a topology that breaks a rule shows up as a failing case
# instead of a silently misrouted session. Every case below is a layout that
# either occurs in the fleet or previously misrouted (the two marked REGRESSION
# were real bugs, verified 2026-07-25).
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
    mkdir -p "$tmp/empty-state" "$tmp/fe-state/sot" "$tmp/renamed" "$tmp/decoy"
    : > "$tmp/fe-state/sot/fe-inbox.jsonl"
    git -C "$tmp/renamed" init -q 2>/dev/null
    git -C "$tmp/renamed" remote add origin https://example.invalid/any-owner/ship-of-tools.git 2>/dev/null
    git -C "$tmp/decoy" init -q 2>/dev/null
    git -C "$tmp/decoy" remote add origin https://example.invalid/any-owner/ship-of-tools-plugins.git 2>/dev/null
    _p=0; _f=0
    _case() { # desc expected actual
        if [ "$2" = "$3" ]; then _p=$((_p+1)); printf '  ok    %-52s -> %s\n' "$1" "$3"
        else _f=$((_f+1)); printf '  FAIL  %-52s -> %s (want %s)\n' "$1" "$3" "$2"; fi
    }
    echo "comm-session-skill selftest (repo: $sot)"
    _case "BE: SoT repo" "$BE_SKILL" "$(cd "$sot" && "$self")"
    # A REAL secondary worktree if this checkout has one — its basename can carry
    # no repo name at all (`.claude/worktrees/agent-<hash>`) yet it must still
    # resolve to the backend skill. Skipped, not faked, when none exists: a
    # made-up path would exercise nothing (no gitdir, no markers).
    _wt="$(git -C "$sot" worktree list --porcelain 2>/dev/null | awk '/^worktree /{print $2}' | sed -n '2p')"
    if [ -n "${_wt:-}" ] && [ -d "${_wt:-}" ]; then
        _case "BE: real worktree, odd basename (REGRESSION)" "$BE_SKILL" \
              "$(CLAUDE_PROJECT_DIR="$_wt" "$self")"
    else
        printf '  skip  %-52s (no secondary worktree here)\n' "BE: real worktree, odd basename"
    fi
    _case "BE: clone in renamed dir (remote identity)" "$BE_SKILL" \
          "$(CLAUDE_PROJECT_DIR="$tmp/renamed" "$self")"
    _case "BE: tmux wins on a box that runs an FE" "$BE_SKILL" \
          "$(cd "$sot" && TMUX="${TMUX:-fake}" XDG_STATE_HOME="$tmp/fe-state" "$self")"
    _case "FE: joined win-fe handle" "$FE_SKILL" "$(SOT_COMM_NAME=win-fe-devbox "$self")"
    _case "FE: no tmux, fe-inbox exists" "$FE_SKILL" \
          "$(cd "$sot" && env -u TMUX XDG_STATE_HOME="$tmp/fe-state" "$self")"
    _case "FE: COLD windows FE, no inbox yet (REGRESSION)" "$FE_SKILL" \
          "$(cd "$sot" && env -u TMUX OS=Windows_NT XDG_STATE_HOME="$tmp/empty-state" "$self")"
    _case "generic: windows, no tmux, NON-SoT repo" "$GENERIC_SKILL" \
          "$(cd "$tmp/decoy" && env -u TMUX OS=Windows_NT XDG_STATE_HOME="$tmp/empty-state" "$self")"
    _case "generic: plain non-repo dir" "$GENERIC_SKILL" "$(cd "$tmp" && "$self")"
    _case "generic: decoy repo ship-of-tools-plugins" "$GENERIC_SKILL" \
          "$(CLAUDE_PROJECT_DIR="$tmp/decoy" "$self")"
    _case "override: SOT_SESSION_ROLE=generic in SoT" "$GENERIC_SKILL" \
          "$(cd "$sot" && SOT_SESSION_ROLE=generic "$self")"
    _case "override: SOT_SESSION_ROLE=fe in SoT" "$FE_SKILL" \
          "$(cd "$sot" && SOT_SESSION_ROLE=fe "$self")"
    echo
    echo "comm-session-skill selftest: passed=$_p failed=$_f"
    [ "$_f" -eq 0 ] || exit 1
    exit 0
fi

# --- 0. explicit override ----------------------------------------------------
case "${SOT_SESSION_ROLE:-}" in
    fe|FE)           echo "$FE_SKILL"; exit 0 ;;
    be|BE)           echo "$BE_SKILL"; exit 0 ;;
    generic|GENERIC) echo "$GENERIC_SKILL"; exit 0 ;;
esac

# comm-context.sh emits %q-quoted `KEY=value` lines — eval it, never sed-scrape
# (a scrape can capture the literal quotes as a bogus non-empty handle). It also
# self-invalidates a stale pane-keyed identity, so NAME comes back empty rather
# than wrong when tmux has recycled the pane id.
NAME=""
REPO=""
if [ -x "$SELF_DIR/comm-context.sh" ]; then
    eval "$("$SELF_DIR/comm-context.sh" 2>/dev/null)" 2>/dev/null || true
fi

handle="${SOT_COMM_NAME:-${NAME:-}}"

# --- 1. definitive frontend handle -------------------------------------------
case "$handle" in
    win-fe*) echo "$FE_SKILL"; exit 0 ;;
esac

if [ -n "${LOCALAPPDATA:-}" ]; then
    fe_inbox="$LOCALAPPDATA/sot/fe-inbox.jsonl"
else
    fe_inbox="${XDG_STATE_HOME:-$HOME/.local/state}/sot/fe-inbox.jsonl"
fi

repo_dir="$(_repo_dir)"

# --- 2. this box runs a frontend, and we're not a tmux backend ----------------
if [ -z "${TMUX:-}" ] && [ -f "$fe_inbox" ]; then
    echo "$FE_SKILL"
    exit 0
fi

# --- 3. cold frontend: Windows, no tmux, in the SoT checkout, no inbox yet ----
if [ -z "${TMUX:-}" ] && _is_windows && _is_sot_repo "$repo_dir"; then
    echo "$FE_SKILL"
    exit 0
fi

# --- 4/5. backend superset vs project-agnostic bootstrap ---------------------
if _is_sot_repo "$repo_dir"; then
    echo "$BE_SKILL"
else
    echo "$GENERIC_SKILL"
fi
