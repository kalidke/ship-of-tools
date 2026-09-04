#!/usr/bin/env bash
# comm-join.sh — register this session in the sot-comm network.
# Usage: comm-join.sh [--name NAME] [--expertise "a, b, c"]
#   With NO args it joins as the canonical default handle <repo>-<host> —
#   "just run it". See --help.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"

usage() {
    cat <<'EOF'
comm-join.sh — register this session in the sot-comm network.

Usage:
  comm-join.sh                       just run it: joins as the canonical
                                     default handle <repo>-<host>
  comm-join.sh --name NAME           join as an explicit handle
  comm-join.sh --name=NAME           (equals form also accepted)
  comm-join.sh --expertise "a, b"    optional comma-separated expertise tags
  comm-join.sh --expertise="a, b"    (equals form also accepted)
  comm-join.sh -h | --help           this help

Handles are MIXED-CASE-canonical: the default <repo>-<host> is used verbatim,
case preserved (NOT lowercased). Existing all-lowercase registry rows are
legacy and still valid; new handles follow the host/repo casing as-is.

Derived vs explicit: the default <repo>-<host> handle is auto-disambiguated
against the registry's recorded project root when two different projects
share a basename+host (e.g. two same-named repos in different directories) —
you get <repo>-<parentdir>-<host>, or a hash-qualified handle as a last
resort. An explicit --name, $SOT_COMM_NAME, or an already-joined identity is
always used VERBATIM and never disambiguated — in that PRECEDENCE order:
--name beats a pinned $SOT_COMM_NAME beats an already-joined self-file
identity, and a pinned name (either of the first two) NEVER yields to
whatever a self-file resolves, even a validated one. See the "derived vs
explicit" section of docs/adr/0028-remote-comm-autoconnect.md.

On success prints "Joined sot-comm as @<handle>" — that line IS your
identity confirmation.
EOF
}

WANT_NAME=""; EXPERTISE=""
while [ $# -gt 0 ]; do
    case "$1" in
        -h|--help)     usage; exit 0 ;;
        --name)        WANT_NAME="$2"; shift 2 ;;
        --name=*)      WANT_NAME="${1#--name=}"; shift ;;
        --expertise)   EXPERTISE="$2"; shift 2 ;;
        --expertise=*) EXPERTISE="${1#--expertise=}"; shift ;;
        # A handle can never start with '-'; an unknown dash-option once fell
        # through the catch-all and registered itself AS the handle (e.g.
        # `comm-join.sh --help` joined as @--help). Reject explicitly.
        -*)            echo "comm-join.sh: unknown option '$1' (a handle can't start with '-'; see --help)" >&2; exit 2 ;;
        *)             [ -z "$WANT_NAME" ] && WANT_NAME="$1"; shift ;;
    esac
done

eval "$("$SCRIPT_DIR/comm-context.sh")"
ensure_home

# Spawn handoff: comm-spawn (and the daemon's capsule producer env, see
# capsule_workspace::capsule_supervisor_env) pin the agent's handle by
# setting SOT_COMM_NAME=<name> (and optionally SOT_COMM_EXPERTISE) before
# the /sot-session-start join runs, so it lands on the handle the
# spawner/daemon is awaiting.
#
# Precedence, HIGH to LOW: --name (explicit, this call) > $SOT_COMM_NAME
# (pinned by whoever launched this process) > an already-joined self-file
# identity (a plain rejoin with neither of the above keeps its identity)
# > fresh derivation. A PINNED name (either of the first two) is used
# VERBATIM and must NEVER be overridden by whatever a self-file happens
# to resolve — capsule-comm-identity fix: a self-file can pass
# comm-context.sh's own root=/repo= validation while still holding the
# WRONG identity for this exact process (the shared per-host `__nopane`
# slot's whole failure mode, before SOT_COMM_SELF_FILE gave each
# workspace its own slot — this precedence fix is the second, belt-and-
# braces line of defense: even a same-slot self-file must never outrank
# a pin). Only when NEITHER --name nor $SOT_COMM_NAME was given does an
# already-joined self-file identity (from comm-context.sh's own
# read-side matrix) survive to here — anything it discarded as
# stale/unhealable comes back as NAME="" regardless.
if [ -n "$WANT_NAME" ]; then
    NAME="$WANT_NAME"
elif [ -n "${SOT_COMM_NAME:-}" ]; then
    NAME="$SOT_COMM_NAME"
fi
[ -z "$EXPERTISE" ] && EXPERTISE="${SOT_COMM_EXPERTISE:-}"
# Reached only when NAME came from none of the verbatim sources above
# (--name, $SOT_COMM_NAME, an already-joined self-file identity) — this is
# the DERIVED case. Note what does NOT happen here: the name is NOT decided
# yet. Deciding it now (sot_derive_handle) and only later locking to write
# it would reopen the exact read-then-write race the feature exists to
# close — a concurrent derived join for a different root could observe the
# same "still free" registry state in between. The decision and the write
# happen together, atomically, below (claim_derived_handle).
NEED_DERIVE=false
[ -z "$NAME" ] && NEED_DERIVE=true

ts="$(now_iso)"
exp_json="$(printf '%s' "$EXPERTISE" \
    | jq -R 'split(",") | map(gsub("^[[:space:]]+|[[:space:]]+$";"")) | map(select(length > 0))')"
[ -z "$exp_json" ] && exp_json="[]"

# Independent of NAME (the row's contents don't name themselves) — safe to
# build before NAME is finalized either way.
#
# MSYS2 argv-conversion guard (comm-lib.sh's sot_jq_rawfile): PROJECT_ROOT
# is a filesystem path and can legitimately start with "/" (the POSIX
# form MSYS/git-bash's own `pwd` fallback produces) — must never reach jq
# via --arg. See that helper's comment for the mechanism. The EXIT trap
# (Codex round finding 9, mirrors comm-send.sh's own MSG_FILE cleanup) is
# what actually guarantees this temp file never leaks: a `jq` failure
# under `set -e` exits the script immediately, skipping a bare `rm -f`
# placed after it.
root_file="$(sot_jq_rawfile "$PROJECT_ROOT")" || exit 1
trap 'rm -f "$root_file"' EXIT
obj="$(jq -n \
    --arg host "$HOST" --arg tmux "$TMUX_TARGET" --arg pane "$PANE_ID" \
    --arg repo "$REPO" --rawfile root "$root_file" --argjson exp "$exp_json" --arg ts "$ts" \
    '{host:$host, tmux:$tmux, pane_id:$pane, repo:$repo, root:$root, expertise:$exp,
      status:"idle", joined:$ts, last_seen:$ts}')"

if [ "$NEED_DERIVE" = true ]; then
    # reclaim mode (Codex review F3): a plain join treats an existing
    # same-root row as mine to reclaim — today's rejoin behavior. `set -e`
    # makes a derivation failure (all three tiers taken by other roots;
    # Codex review F6) abort here with sot_derive_handle's own clear
    # stderr reason, rather than continuing with an empty/invalid NAME.
    claim_derived_handle reclaim "$PROJECT_ROOT" "$HOST" "$obj"
    NAME="$CLAIMED_NAME"

    # Stranding guard (field regression): escalating AWAY from the bare
    # tier-1 handle (CLAIMED_QUALIFIER non-empty, so NAME != CLAIMED_TIER1)
    # is the normal, correct outcome for a REAL collision with another
    # project sharing this basename+host. But it is ALSO exactly what
    # happens when THIS session's own tier-1 row was just evicted (a stale
    # self-file discarded by comm-context.sh, pre-self-heal or otherwise) —
    # the registry still shows tier-1 as held by "an unknown project", so
    # derivation treats it as someone else's and hands back a DIFFERENT
    # handle, silently. A live listener bridge for the bare handle, under
    # this same uid, is strong evidence that "unknown project" is actually
    # THIS session's own prior identity — a real collision from an
    # unrelated project has no reason to be running a bridge named after
    # OUR root's basename+host. Warn loudly so the operator/session can no
    # longer strand silently; still proceed with the qualified join (the
    # bridge alone doesn't prove ownership — a genuinely different, still
    # -live session for the same repo+host could be the one running it —
    # so this NEVER auto-reclaims).
    if [ -n "$CLAIMED_QUALIFIER" ] && [ -n "$CLAIMED_TIER1" ] && [ "$CLAIMED_TIER1" != "$NAME" ] \
       && sot_bridge_running_for "$CLAIMED_TIER1"; then
        # The printed recipe below uses THIS install's own SCRIPT_DIR —
        # never a hardcoded ~/.sot-comm/bin/, wrong under a non-default
        # $SOT_COMM_HOME (supported) — and %q-quotes every interpolated
        # handle AND the executable path itself (Codex review round-3
        # finding 7: an unquoted $SCRIPT_DIR breaks under a spaced install
        # path).
        QNAME="$(printf '%q' "$NAME")"
        QTIER1="$(printf '%q' "$CLAIMED_TIER1")"
        QSCRIPT_DIR="$(printf '%q' "$SCRIPT_DIR")"
        cat >&2 <<WARN

*** WARNING: joined as '@$NAME', but a relay listener bridge for
*** '@$CLAIMED_TIER1' (this project's bare handle) is ALREADY RUNNING under
*** this user. That is almost certainly YOUR OWN earlier identity, not a
*** real collision with another project — most likely this session's own
*** '@$CLAIMED_TIER1' row was evicted as stale (see comm-context.sh) and
*** this join escalated away from it instead of reclaiming it. Proceeding
*** with the qualified join as '@$NAME' — a running bridge alone doesn't
*** prove ownership, so this is never auto-reclaimed — but if this IS your
*** own handle, you can now strand yourself silently: your listener bridge
*** and any armed Monitor go on serving '@$CLAIMED_TIER1''s inbox while
*** everyone else now addresses you as '@$NAME'.
***
*** If you confirm sole ownership (one live session with this repo as cwd,
*** whose bridge creation time matches when THIS session started), reclaim
*** the bare handle instead of staying on '@$NAME':
***   $QSCRIPT_DIR/comm-leave.sh --name $QNAME
***   $QSCRIPT_DIR/comm-join.sh --name $QTIER1
WARN
    fi
else
    with_lock registry_put "$NAME" "$obj"
fi
# v2 self-file: identity + the repo it was claimed for, plus the canonical
# project root (ADR 0028 addendum — additive; a self-file without a root=
# line predates this feature and is treated as unknown-root on read). The
# repo line is used by comm-context to detect a stale identity in a
# RECYCLED tmux pane (pane ids are reused after a server restart) and
# discard it instead of letting a fresh session inherit another session's
# handle. Written via the shared atomic writer (comm-lib.sh) — same
# same-directory-tmp-plus-mv contract comm-context.sh's self-heal uses
# (Codex review round-1 finding 3) — and its failure is FATAL here: the
# registry row above is already claimed, but with no local self-file this
# shell has no record of it and every future comm call from it will read
# as "not joined".
if ! sot_write_self_file "$SELF_FILE" "$NAME" "$REPO" "$PROJECT_ROOT"; then
    echo "comm-join.sh: FATAL — joined as @$NAME in the registry, but could not write the local self-file at '$SELF_FILE' (see reason above). This shell has no identity record; every comm-* call from it will say 'not joined'. Fix the self-file directory's permissions and re-run comm-join.sh --name $NAME." >&2
    exit 1
fi
# A joined handle always has an inbox: durable comm-send targets it, and a
# first-ever selftest otherwise probes a nonexistent file (noisy redirect
# errors that derail diagnosis — 2026-06-11 fresh-join report). Append-touch
# so an existing inbox is never truncated.
: >> "$INBOX_DIR/$NAME.jsonl"

# No legacy self-file sweep: unnecessary disk hygiene, not safety-load-
# bearing — an abandoned legacy file only matters if its exact path is
# read again, and a rightful owner's self-file self-heals (or is freshly
# written in full v2 form, as above) the moment it's actually used. See
# ADR 0028's "Self-file read-side transition" for why a legacy file is no
# longer "already inert" the way it was pre-hotfix.
have="$(jq -r '.protocol_version // 0' "$REGISTRY")"
if [ "$have" != "$PROTOCOL_VERSION" ]; then
    echo "WARNING: registry protocol v$have != client v$PROTOCOL_VERSION — run ShipTools.update_comm() on all machines" >&2
fi

others="$(jq -r --arg me "$NAME" '.agents | keys[] | select(. != $me)' "$REGISTRY" | paste -sd ", " -)"
echo "Joined sot-comm as @$NAME  ($REPO on $HOST)."
echo "  inbox: $INBOX_DIR/$NAME.jsonl"
echo "Others registered: ${others:-none}"
