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
resort. An explicit --name (or $SOT_COMM_NAME, or an already-joined
identity) is always used VERBATIM and never disambiguated. See the "derived
vs explicit" section of docs/adr/0028-remote-comm-autoconnect.md.

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

[ -n "$WANT_NAME" ] && NAME="$WANT_NAME"
# Spawn handoff: comm-spawn pins the agent's handle by prefixing the ccb launch
# with SOT_COMM_NAME=<name> (and optionally SOT_COMM_EXPERTISE), so the
# /sot-session-start join inside the spawned session lands on the handle the
# spawner is awaiting. Explicit --name wins; an already-joined NAME (from
# context) wins over the env (a rejoin keeps its identity).
#
# Precedence check (Codex review F1; updated for the round-1 self-heal
# fix — Codex review round-2 SHOULD-FIX 1/F flagged this comment as stale
# against that fix): NAME here can carry an already-joined self-file
# identity that comm-context.sh just validated — either a strict `root=`
# match, OR a legacy (pre-#148, no `root=`) self-file it SELF-HEALED on
# this same read (a `repo=` basename match corroborated by the registry,
# or by basename alone when the registry offers no contrary evidence — see
# comm-context.sh's own comment for the exact ruling matrix). A self-file
# whose `root=` is PRESENT AND WRONG, or a legacy one the heal matrix
# REFUSED (the registry disagrees, or it's the ancient one-line format
# with no corroboration), comes back as NAME="" from context (see its
# guard), so it can never reach this line to wrongly out-rank a
# spawn-pinned $SOT_COMM_NAME below. A genuinely stale/wrong self-file
# overriding a spawn pin is exactly what that guard closes; a rightfully
# self-healed one IS this session's own real identity and correctly keeps
# precedence, same as it always has.
[ -z "$NAME" ] && NAME="${SOT_COMM_NAME:-}"
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
obj="$(jq -n \
    --arg host "$HOST" --arg tmux "$TMUX_TARGET" --arg pane "$PANE_ID" \
    --arg repo "$REPO" --arg root "$PROJECT_ROOT" --argjson exp "$exp_json" --arg ts "$ts" \
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

# No legacy self-file sweep (Codex review PR #148 round 2, simplicity
# audit — deleted ~50 lines that used to live here; kept deleted after the
# round-1 self-heal fix, but the REASONING below was stale until this
# update — Codex review round-2 SHOULD-FIX 1/F). It remains unnecessary,
# for an updated reason: comm-context.sh's read-side guard no longer
# rejects EVERY legacy (no `root=`) self-file unconditionally — it now
# SELF-HEALS one whose `repo=`/registry evidence checks out (see that
# script's own comment for the exact matrix), so a legacy file is not
# "already inert" the way it was pre-hotfix. But it only matters if
# something actually reads THAT exact path again, and nothing here scans
# the self-file directory proactively — an abandoned legacy file nobody's
# session ever revisits stays exactly as harmless clutter as before, just
# for a narrower reason. And a rightful owner's self-file self-heals (or
# is freshly written in full v2 form, as this script's own write just
# above does) the moment that owner's session actually uses it. The
# sweep's only remaining job was pure disk hygiene (deleting ABANDONED
# files nobody will ever rejoin), bought at the cost of a directory glob +
# a stat/read per file, done EAGERLY on every join, while holding the
# single global registry lock — the wrong trade for a non-safety-load-
# bearing cleanup.
have="$(jq -r '.protocol_version // 0' "$REGISTRY")"
if [ "$have" != "$PROTOCOL_VERSION" ]; then
    echo "WARNING: registry protocol v$have != client v$PROTOCOL_VERSION — run ShipTools.update_comm() on all machines" >&2
fi

others="$(jq -r --arg me "$NAME" '.agents | keys[] | select(. != $me)' "$REGISTRY" | paste -sd ", " -)"
echo "Joined sot-comm as @$NAME  ($REPO on $HOST)."
echo "  inbox: $INBOX_DIR/$NAME.jsonl"
echo "Others registered: ${others:-none}"
