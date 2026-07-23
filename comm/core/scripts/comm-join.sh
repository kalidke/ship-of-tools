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
[ -z "$NAME" ] && NAME="${SOT_COMM_NAME:-}"
[ -z "$EXPERTISE" ] && EXPERTISE="${SOT_COMM_EXPERTISE:-}"
[ -z "$NAME" ] && NAME="${REPO}-${HOST}"

ts="$(now_iso)"
exp_json="$(printf '%s' "$EXPERTISE" \
    | jq -R 'split(",") | map(gsub("^[[:space:]]+|[[:space:]]+$";"")) | map(select(length > 0))')"
[ -z "$exp_json" ] && exp_json="[]"

obj="$(jq -n \
    --arg host "$HOST" --arg tmux "$TMUX_TARGET" --arg pane "$PANE_ID" \
    --arg repo "$REPO" --argjson exp "$exp_json" --arg ts "$ts" \
    '{host:$host, tmux:$tmux, pane_id:$pane, repo:$repo, expertise:$exp,
      status:"idle", joined:$ts, last_seen:$ts}')"

with_lock registry_put "$NAME" "$obj"
# v2 self-file: identity + the repo it was claimed for. comm-context uses the
# repo line to detect a stale identity in a RECYCLED tmux pane (pane ids are
# reused after a server restart) and discard it instead of letting a fresh
# session inherit another session's handle.
printf '%s\nrepo=%s\n' "$NAME" "$REPO" > "$SELF_FILE"
# A joined handle always has an inbox: durable comm-send targets it, and a
# first-ever selftest otherwise probes a nonexistent file (noisy redirect
# errors that derail diagnosis — 2026-06-11 fresh-join report). Append-touch
# so an existing inbox is never truncated.
: >> "$INBOX_DIR/$NAME.jsonl"

# Sweep LEGACY (one-line, pre-provenance) self-files. The v2 repo-line guard
# can't validate a legacy file, and a stale legacy file's owner is gone — it
# never upgrades itself, so each one is a mine: any fresh session whose pane
# id happens to match inherits that identity (three sessions hit this in one
# day once pane ids recycled after a tmux server restart — wrong Step-0
# verdicts, one session SENDING as another's handle). Ground truth is the
# REGISTRY row, which its owner's every join refreshes with the real
# host/pane: a legacy file whose name's row points at this exact file is the
# rightful owner's — upgrade it to v2 in place (with the row's repo); any
# other legacy file (no row, or the row lives at another pane) is stale —
# delete it. v2 files are never touched; the whole sweep runs under the
# registry lock and is idempotent, so concurrent joins are safe. Runs on
# every join — after the first sweep per install it's a no-op scan.
sweep_legacy_selffiles() {
    local rows f key name row_key row_repo migrated=0 removed=0
    rows="$(jq -r '.agents | to_entries[]
        | [.key, (.value.host // ""), ((.value.pane_id // "") | ltrimstr("%")), (.value.repo // "")]
        | @tsv' "$REGISTRY" 2>/dev/null)" || return 0
    for f in "$SELF_DIR"/*.txt; do
        [ -f "$f" ] || continue
        [ "$(wc -l < "$f")" -ge 2 ] && continue   # v2 — has provenance, not ours to touch
        key="$(basename "$f" .txt)"
        name="$(sed -n '1p' "$f")"
        row_key=""; row_repo=""
        while IFS=$'\t' read -r rname rhost rpane rrepo; do
            [ "$rname" = "$name" ] || continue
            row_key="${rhost}__${rpane:-nopane}"; row_repo="$rrepo"; break
        done <<< "$rows"
        if [ -n "$row_key" ] && [ "$row_key" = "$key" ]; then
            printf '%s\nrepo=%s\n' "$name" "$row_repo" > "$f"
            migrated=$((migrated + 1))
        else
            rm -f "$f"
            removed=$((removed + 1))
        fi
    done
    [ $((migrated + removed)) -gt 0 ] &&
        echo "comm-join: legacy self-file sweep — upgraded $migrated rightful identities, removed $removed stale ones" >&2
    return 0
}
with_lock sweep_legacy_selffiles

have="$(jq -r '.protocol_version // 0' "$REGISTRY")"
if [ "$have" != "$PROTOCOL_VERSION" ]; then
    echo "WARNING: registry protocol v$have != client v$PROTOCOL_VERSION — run ShipTools.update_comm() on all machines" >&2
fi

others="$(jq -r --arg me "$NAME" '.agents | keys[] | select(. != $me)' "$REGISTRY" | paste -sd ", " -)"
echo "Joined sot-comm as @$NAME  ($REPO on $HOST)."
echo "  inbox: $INBOX_DIR/$NAME.jsonl"
echo "Others registered: ${others:-none}"
