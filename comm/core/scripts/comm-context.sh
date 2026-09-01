#!/usr/bin/env bash
# comm-context.sh — detect this session's comm identity. Output is eval-able:
#   eval "$(.../comm-context.sh)"
# Sets HOST PANE_ID TMUX_TARGET REPO PROJECT_ROOT NAME SELF_FILE COMM_HOME REGISTRY INBOX_DIR READ_DIR.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=comm-lib.sh
source "$SCRIPT_DIR/comm-lib.sh"
ensure_home

# SOT_COMM_TEST_HOST lets a caller pin HOST directly, bypassing `hostname -s`
# — mirrors the $SOT_COMM_SELF_FILE test seam below. A test must be hermetic
# in HOST exactly as it is in HOME: the real host's name is unpredictable
# (short/clean on a dev box, but a CI runner's hostname can be long and,
# once it exceeds sot_sanitize_component's clamp, gets rewritten by
# sot_derive_handle's host-alias guard — comm-lib.sh, ADR 0028 addendum
# "host aliasing"). A test that computes its own expected handle from the
# raw hostname without going through that same transformation silently
# diverges on any host long enough to trigger it — this is exactly what
# happened: 8/13 cases (every one asserting a derived handle) failed on a
# GitHub Actions runner while passing locally, because the runner's
# hostname is longer than a dev box's. Pinning through this seam removes
# the dependency entirely, on both the scripts' side and the test's own
# expectations. Unset in normal use.
if [ -n "${SOT_COMM_TEST_HOST:-}" ]; then
    HOST="$SOT_COMM_TEST_HOST"
else
    HOST="$(hostname -s 2>/dev/null || hostname)"
fi

PANE_ID=""
TMUX_TARGET=""
if [ -n "${TMUX_PANE:-}" ]; then
    PANE_ID="$(tmux display-message -t "$TMUX_PANE" -p '#{pane_id}' 2>/dev/null || true)"
    TMUX_TARGET="$(tmux display-message -t "$TMUX_PANE" -p '#{session_name}:#{window_index}.#{pane_index}' 2>/dev/null || true)"
fi

RAW_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
REPO="$(basename "$RAW_ROOT")"
# Canonical (symlink-resolved absolute) root — the identity the derived-name
# disambiguation and the registry/self-file `root` field compare against
# (ADR 0028 addendum). Kept separate from REPO (which stays the RAW
# basename, unchanged, to avoid perturbing every other consumer of REPO) so
# a symlinked leaf directory can't shift existing repo-slug behavior.
# Canonicalized ONCE, here, outside any lock (Codex review F8) — every
# downstream consumer (self-file validation below, comm-join.sh,
# comm-spawn.sh) trusts this value as-is rather than re-resolving it.
# sot_canonical_path fails loudly rather than ever handing back a relative
# path; that failure must not be swallowed into an empty PROJECT_ROOT.
if ! PROJECT_ROOT="$(sot_canonical_path "$RAW_ROOT")"; then
    echo "comm-context: could not establish a canonical project root for '$RAW_ROOT' — see the reason above; aborting rather than proceeding with an unknown identity" >&2
    exit 1
fi

# SOT_COMM_SELF_FILE lets a caller pin the self-file path directly, bypassing
# the HOST/PANE_ID keying below — a test harness has no real tmux pane to key
# on (a fake $TMUX_PANE still fails the `tmux display-message` call and
# collapses to the same "nopane" key for every simulated session), so this is
# the seam that lets it give each simulated session its own identity slot.
# Unset in normal use; mirrors the existing $SOT_COMM_HOME override.
if [ -n "${SOT_COMM_SELF_FILE:-}" ]; then
    SELF_FILE="$SOT_COMM_SELF_FILE"
else
    PANE_SAFE="${PANE_ID//%/}"
    SELF_FILE="$SELF_DIR/${HOST}__${PANE_SAFE:-nopane}.txt"
fi
# The self-file is keyed by PANE ID, and tmux REUSES pane ids (%1, %2, …)
# after a server restart — so a fresh session in a recycled pane can inherit
# a DIFFERENT session's identity. That poisons everything downstream: the
# session-start Step-0 watcher check pgreps the wrong handle (finds the other
# session's live watcher → false "survived compaction" → stays deaf), and a
# no-args rejoin keeps the stolen name (two sessions executing as one handle).
#
# Guard: validated against `root=` (Codex review F1), NOT `repo=` — two
# DIFFERENT projects sharing a repo basename (e.g. two same-named repos in
# different directories, exactly what the derived-handle disambiguation
# exists to tell apart) would pass a `repo=`-only check and recreate the
# very alias this feature closes: `/a/foo` joins, a reused pane (or another
# non-tmux shell sharing the same "nopane" key) in `/b/foo` would read back
# `repo=foo` matching and inherit `/a/foo`'s identity verbatim — including
# overriding a spawn-pinned $SOT_COMM_NAME, since a valid self-file identity
# is read into NAME before comm-join.sh even looks at the env (see its
# precedence comment). Root comparison closes that: two different roots
# never match regardless of shared basename.
#
# A self-file with a `root=` line gets the strict check above's sibling
# here: present-and-mismatched is ALWAYS stale (two different roots never
# match regardless of shared basename — the whole point of root=).
#
# A self-file with NO `root=` line is a DIFFERENT case: legacy, predating
# PR #148 (which added root=) — the field regression this block now fixes.
# PR #148 originally discarded these UNCONDITIONALLY, on the theory that
# "unknown root is a collision, not a free pass" (the same fail-safe stance
# ADR 0028 applies to registry rows). In production that discarded EVERY
# self-file written before #148 shipped — every long-running session's
# identity — on its very next comm call ("Not joined — run comm-join.sh
# first"), because nothing about those sessions changed; only the on-disk
# format did. That is strictly worse than the pane-recycling bug root= was
# added to close.
#
# The fix: fall back to the check root= REPLACED — the `repo=` comparison
# that guarded pane recycling from PR #68 until #148 (see that revision of
# this file in git history), NOW ALSO corroborated against the registry
# before ever healing (Codex review round-1 finding 2 — see the ruling
# matrix in the code block below). A repo= MISMATCH is left exactly as
# fail-safe as root= mismatch: discarded, forcing fresh derivation — this
# is the original pane-recycling protection (a recycled tmux pane id, a
# genuine `cd` elsewhere, OR a no-pane self-file shared across unrelated
# cwds — see the nopane note below) and must not be loosened.
#
# Nopane sharing (verified field case): a shell with NO tmux pane context
# (no $TMUX_PANE, or a failing `tmux display-message`) collapses to the
# SAME "nopane" key for every such shell on this host — SELF_FILE above is
# literally "$HOST__nopane.txt", repo-agnostic in its own name, for all of
# them. The repo=/root= comparison here is what makes that sharing safe: a
# background shell in a different repo, or one cd'd OUTSIDE any repo
# entirely (REPO then reads as the cwd's own basename, e.g. a scratchpad
# dir), reads back the same file but never matches it, so the identity is
# discarded exactly like a recycled-pane collision — NEVER self-healed
# across repos/cwds. Do not special-case the nopane slot to skip this
# check; that would let unrelated shells alias onto one identity, the same
# class of bug root= exists to close.
NAME=""
if [ -f "$SELF_FILE" ]; then
    NAME="$(sed -n '1p' "$SELF_FILE")"
    SELF_REPO_LINE="$(sed -n '2p' "$SELF_FILE")"
    SELF_ROOT_LINE="$(sed -n '3p' "$SELF_FILE")"
    # Existence of the `repo=`/`root=` PREFIX is checked separately from
    # the extracted VALUE (Codex review round-1 finding 2: "distinguish an
    # absent root= line from a present-but-empty/malformed one") — the old
    # `sed -n 's/^root=//p'` scrape made both cases read back as the same
    # empty string, so a corrupted `root=` line (present, but empty or
    # malformed) was silently treated as "no root= line at all" and fell
    # through to the more permissive legacy path below instead of being
    # rejected outright.
    case "$SELF_REPO_LINE" in
        repo=*) SELF_REPO="${SELF_REPO_LINE#repo=}"; HAS_REPO_LINE=1 ;;
        *)      SELF_REPO="";                        HAS_REPO_LINE=0 ;;
    esac
    case "$SELF_ROOT_LINE" in
        root=*) SELF_ROOT="${SELF_ROOT_LINE#root=}"; HAS_ROOT_LINE=1 ;;
        *)      SELF_ROOT="";                        HAS_ROOT_LINE=0 ;;
    esac

    if [ "$HAS_ROOT_LINE" = 1 ]; then
        # v2 self-file. Present-and-wrong is unconditionally stale (the
        # whole point of root=). Present-but-EMPTY/malformed carries no
        # evidence either way, so it gets the SAME fail-safe rejection —
        # never the more permissive legacy-heal path below, which is for
        # files that predate root= entirely, not ones that have a broken
        # one.
        if [ -z "$SELF_ROOT" ] || [ "$SELF_ROOT" != "$PROJECT_ROOT" ]; then
            reason="root='$SELF_ROOT'"
            [ -z "$SELF_ROOT" ] && reason="an empty/malformed root="
            echo "comm-context: self-file identity '$NAME' has $reason which doesn't match this project ('$PROJECT_ROOT') — stale; discarding (forces fresh derivation)" >&2
            NAME=""
        fi
    elif [ "$HAS_REPO_LINE" = 1 ] && [ "$SELF_REPO" != "$REPO" ]; then
        echo "comm-context: self-file identity '$NAME' was claimed for repo '$SELF_REPO' but this is '$REPO' — stale (pane id reused, a genuine cd elsewhere, or a shared no-pane self-file read from a different repo/cwd); discarding" >&2
        NAME=""
    elif [ -n "$NAME" ]; then
        # Legacy (pre-#148) self-file: no root= line, and repo= either
        # matches this project's basename or is absent entirely (the
        # ANCIENT one-line format, pre-#68). Basename alone is no longer
        # trusted on its own (Codex review round-1 finding 2 reproduced
        # exactly the failure mode that made basename-only trust unsafe: a
        # legacy file read from the WRONG checkout of a same-basename repo
        # got healed onto that wrong checkout's root, recreating the exact
        # alias root= was added to kill) — the registry, the one other
        # piece of independent evidence available, is consulted FIRST,
        # before anything is ever written:
        #
        #   registry row has a root, root MATCHES this project  -> heal
        #   registry row has a root, root DISAGREES               -> DISCARD
        #     (this is the reproduced wrong-checkout case: a basename
        #     match can never override a registry root disagreement)
        #   registry row has no root (legacy row), or no row at all:
        #     repo= present and matches                            -> heal
        #       (documented residual ambiguity below)
        #     ancient one-line (no repo= at all)                   -> DISCARD
        #       (carries literally no evidence of its own to check)
        IFS=$'\t' read -r reg_status reg_root <<< "$(sot_registry_entry_status "$NAME")"
        heal=0
        if [ "$reg_status" = "present" ] && [ -n "$reg_root" ]; then
            if [ "$reg_root" = "$PROJECT_ROOT" ]; then
                heal=1
            else
                echo "comm-context: self-file identity '$NAME' has no root= (legacy) and the registry's own root for it ('$reg_root') doesn't match this project ('$PROJECT_ROOT') — stale; refusing to self-heal a basename match against contrary registry evidence; discarding" >&2
                NAME=""
            fi
        elif [ "$HAS_REPO_LINE" = 1 ]; then
            # repo= present and matches, but the registry offers nothing
            # to corroborate with (no row, or a legacy row with no root
            # key of its own). Heal on the repo-basename match alone —
            # this slot's exact pre-#148 behavior. Residual, deliberately
            # accepted ambiguity: a same-basename, DIFFERENT-directory
            # repo sharing this exact HOST/pane slot during the
            # pre-#148-to-post-#148 transition, with no registry row to
            # catch it either, would also heal here. That needs a
            # basename collision AND a coincident missing/unknown
            # registry row — rare, and time-bounded (every legacy
            # self-file heals to v2 the first time anyone reads it, so
            # this branch stops mattering once the fleet has cycled once
            # post-upgrade). The alternative — refusing every legacy
            # self-file whose registry row lacks a root — is exactly the
            # fleet-deafening regression this PR exists to fix.
            heal=1
        else
            # Ancient one-line format: no repo= line either, so this
            # self-file carries NO evidence of its own — not even a
            # basename to match against. Heal ONLY when the registry
            # corroborates (handled above); with no row, or an
            # unknown-root row, refuse rather than stamp this project's
            # root into a file that could belong to any repository.
            echo "comm-context: self-file identity '$NAME' is the ancient one-line format (no repo=, no root=) and the registry offers no corroborating root for it — stale; refusing to self-heal; discarding" >&2
            NAME=""
        fi

        if [ "$heal" = 1 ] && [ -n "$NAME" ]; then
            # sot_write_self_file (comm-lib.sh) writes via a
            # same-directory temp file + checked `mv`, never an in-place
            # `>` truncation (Codex review round-1 finding 3: the old
            # in-place write silently no-op'd on a read-only self-file —
            # the redirection failed, nothing checked its exit status, and
            # this script printed "self-healed" anyway). A failed heal is
            # NOT fatal to this call — the identity was already validated
            # above and is good for the current invocation — but it must
            # never be reported as healed, and the file stays legacy for
            # the next read to retry.
            if sot_write_self_file "$SELF_FILE" "$NAME" "$REPO" "$PROJECT_ROOT"; then
                echo "comm-context: self-healed legacy self-file for '$NAME' (pre-#148 format had no root=) — added root='$PROJECT_ROOT'; every read after this one is fully validated" >&2
            else
                echo "comm-context: FAILED to self-heal legacy self-file for '$NAME' at '$SELF_FILE' (see reason above) — proceeding with this identity for THIS call, but the file remains legacy and will be re-evaluated (and re-attempted) on the next read" >&2
            fi
        fi
    fi
fi

# %q on an EMPTY value emits a literal '' — fine for the eval contract (both
# eval to empty), but a textual scraper (`sed -n 's/^NAME=//p'`, as session-start
# skills have used) captures the two quote chars as a NON-empty value, defeating
# ${NAME:-fallback}. Emit a bare KEY= when empty so both consumers are safe.
emit() { if [ -n "$2" ]; then printf '%s=%q\n' "$1" "$2"; else printf '%s=\n' "$1"; fi; }

emit HOST        "$HOST"
emit PANE_ID     "$PANE_ID"
emit TMUX_TARGET "$TMUX_TARGET"
emit REPO        "$REPO"
emit PROJECT_ROOT "$PROJECT_ROOT"
emit NAME        "$NAME"
emit SELF_FILE   "$SELF_FILE"
emit COMM_HOME   "$COMM_HOME"
emit REGISTRY    "$REGISTRY"
emit INBOX_DIR   "$INBOX_DIR"
emit READ_DIR    "$READ_DIR"
