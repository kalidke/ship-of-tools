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
# The self-file is keyed by PANE ID; tmux reuses pane ids, so a fresh
# session in a recycled pane can otherwise inherit a different session's
# identity. Full rationale for everything below — the root=/repo=/
# registry/nopane read-side matrix, including the round-3 additions
# (malformed third line, registry-read errors) — lives in
# docs/adr/0028-remote-comm-autoconnect.md's "Self-file read-side
# transition" section, THE single home of this logic's rationale. Keep
# comments here to short invariant statements only.
#
# IS_NOPANE: true only for the literal shared "$HOST__nopane.txt" slot,
# detected from SELF_FILE's own name (not from PANE_ID at this particular
# invocation — what matters is whether THIS FILE is the one every no-pane
# shell on the host shares).
case "$SELF_FILE" in
    *__nopane.txt) IS_NOPANE=1 ;;
    *)             IS_NOPANE=0 ;;
esac

NAME=""
if [ -f "$SELF_FILE" ]; then
    # Read ONCE via mapfile — never three separate `sed -n 'Np'` opens —
    # so a concurrent atomic rename can't be observed as a spliced mixed
    # version (ADR 0028).
    mapfile -t SELF_LINES < "$SELF_FILE" 2>/dev/null
    NAME="${SELF_LINES[0]:-}"
    SELF_REPO_LINE="${SELF_LINES[1]:-}"
    SELF_ROOT_LINE="${SELF_LINES[2]:-}"
    case "$SELF_REPO_LINE" in
        repo=*) SELF_REPO="${SELF_REPO_LINE#repo=}"; HAS_REPO_LINE=1 ;;
        *)      SELF_REPO="";                        HAS_REPO_LINE=0 ;;
    esac
    # A third element PRESENT but not a `root=...` line (e.g. a corrupted
    # "rootBROKEN") is MALFORMED, not absent — told apart by array length,
    # not pattern match alone (ADR 0028, round-3 finding 2).
    case "$SELF_ROOT_LINE" in
        root=*) SELF_ROOT="${SELF_ROOT_LINE#root=}"; HAS_ROOT_LINE=1; ROOT_MALFORMED=0 ;;
        *)      SELF_ROOT="";                        HAS_ROOT_LINE=0
                [ "${#SELF_LINES[@]}" -ge 3 ] && ROOT_MALFORMED=1 || ROOT_MALFORMED=0 ;;
    esac

    if [ "$HAS_ROOT_LINE" = 1 ]; then
        # v2 self-file: root= present. Empty/mismatched is unconditionally
        # stale (ADR 0028) — never routed through the legacy-heal path.
        if [ -z "$SELF_ROOT" ] || [ "$SELF_ROOT" != "$PROJECT_ROOT" ]; then
            reason="root='$SELF_ROOT'"
            [ -z "$SELF_ROOT" ] && reason="an empty/malformed root="
            echo "comm-context: self-file identity '$NAME' has $reason which doesn't match this project ('$PROJECT_ROOT') — stale; discarding (forces fresh derivation)" >&2
            NAME=""
        fi
    elif [ "$ROOT_MALFORMED" = 1 ]; then
        echo "comm-context: self-file identity '$NAME' has a malformed third line ('$SELF_ROOT_LINE', not a root=... line) — stale; discarding (corrupted evidence is never routed through the legacy-heal path)" >&2
        NAME=""
    elif [ "$HAS_REPO_LINE" = 1 ] && [ "$SELF_REPO" != "$REPO" ]; then
        echo "comm-context: self-file identity '$NAME' was claimed for repo '$SELF_REPO' but this is '$REPO' — stale (pane id reused, a genuine cd elsewhere, or a shared no-pane self-file read from a different repo/cwd); discarding" >&2
        NAME=""
    elif [ -n "$NAME" ]; then
        # Legacy self-file (no root=; repo= matching or the ancient
        # one-line format). Registry consulted before ever healing — see
        # the ADR matrix for the full rationale; this is just the
        # mechanism.
        IFS=$'\t' read -r reg_status reg_root <<< "$(sot_registry_entry_status "$NAME")"
        heal=0
        if [ "$reg_status" = "error" ]; then
            # Registry unreadable/unparseable (ADR 0028, round-3 finding
            # 1): NO EVIDENCE AND NO WRITE. A transient failure costs
            # this one call; the next call re-reads.
            echo "comm-context: could not read/parse the sot-comm registry while validating self-file identity '$NAME' — refusing to trust or heal a basename match with no verifiable registry evidence; discarding" >&2
            NAME=""
        elif [ "$reg_status" = "present" ] && [ -n "$reg_root" ]; then
            if [ "$reg_root" = "$PROJECT_ROOT" ]; then
                heal=1
            else
                echo "comm-context: self-file identity '$NAME' has no root= (legacy) and the registry's own root for it ('$reg_root') doesn't match this project ('$PROJECT_ROOT') — stale; refusing to self-heal a basename match against contrary registry evidence; discarding" >&2
                NAME=""
            fi
        elif [ "$HAS_REPO_LINE" = 1 ] && [ "$IS_NOPANE" != 1 ]; then
            heal=1   # pane-keyed, repo= matches, no contrary registry evidence — ADR 0028 residual ambiguity
        elif [ "$HAS_REPO_LINE" = 1 ] && [ "$IS_NOPANE" = 1 ]; then
            echo "comm-context: self-file identity '$NAME' is in the SHARED nopane slot and this project's repo='$REPO' match alone is not enough evidence for it (this exact file is shared by every no-pane shell on this host) — stale; refusing to self-heal without a corroborating registry root; discarding" >&2
            NAME=""
        else
            echo "comm-context: self-file identity '$NAME' is the ancient one-line format (no repo=, no root=) and the registry offers no corroborating root for it — stale; refusing to self-heal; discarding" >&2
            NAME=""
        fi

        if [ "$heal" = 1 ] && [ -n "$NAME" ]; then
            # Atomic write (comm-lib.sh sot_write_self_file); a failed
            # heal is not fatal to THIS call but must never claim success.
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
