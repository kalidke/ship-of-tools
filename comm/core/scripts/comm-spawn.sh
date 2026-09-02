#!/usr/bin/env bash
# comm-spawn.sh — spawn a new agent to work on another package and report back
# over sot-comm. By default the agent is created as a Ship of Tools *workspace*, so
# it appears in the frontend session strip and is switchable (Ctrl+PageDown);
# switching also gives you that package's files/REPL/concept.
#
# Usage:
#   comm-spawn.sh <repo-path> [--name NAME] [--expertise "a, b"] [--task "do X"]
#                 [--label LABEL] [--endpoint tcp:H:P|unix:PATH] [--no-workspace]
#   comm-spawn.sh <name> <repo-path> [...]      (legacy explicit-name form)
#
#   <repo-path>   package the agent works in (workspace project root)
#   --name        sot-comm handle for the new agent, used VERBATIM. Optional:
#                 when omitted, the handle is DERIVED from the repo-path
#                 basename + host and auto-disambiguated against the registry
#                 (same algorithm as a plain comm-join.sh — see
#                 docs/adr/0028-remote-comm-autoconnect.md), in FRESH mode: an
#                 existing row for the resolved name is a REFUSAL, never a
#                 reclaim — comm-spawn creates a NEW agent, so it must not
#                 silently absorb an existing one even if it shares a project
#                 root. The legacy two-positional form (`<name> <repo-path>`)
#                 still works and is equivalent to passing --name.
#   --label       FE workspace label (default: basename of repo-path); guarded to
#                 the repo basename so a session stays findable next to its repo.
#   --display-label  FE label that deliberately DIFFERS from the repo basename
#                 (e.g. the /worktree tool's '.SoT-wt-<short>' grouping prefix);
#                 bypasses the repo-base guard. The comm HANDLE (<name>) stays
#                 repo-based, so status/clean/sync still group by repo — only the
#                 displayed label + sort slug change.
#   --no-workspace  skip the daemon; just make a raw tmux session (headless use)
#   --endpoint    daemon address; else $SOT_SPAWN_ENDPOINT / $SOT_SOCKET /
#                 auto-detected from the running sotd
#
# Env: SOT_COMM_SPAWN_WAIT (boot wait, default 6s)
#      SOT_COMM_LAUNCH (default 'claude --permission-mode auto')
#
# Rollback contract (Codex review F9, hardened in round 2 findings 1 & 2):
# an EXIT trap is armed BEFORE either write path can happen (right after
# the provisional row's JSON — including a random nonce — is built, ahead
# of both the derived-claim write and the explicit-name write), so there
# is no window — not even the inbox touch, or an invalid explicit --name
# failing there — where a row can exist unprotected. On any SYNCHRONOUS
# failure this script itself detects — nc missing, the daemon endpoint not
# resolving, the same-label refusal (F5), a rejected/failed
# workspace.create, tmux session verification failing, a synchronous
# launch failure in --no-workspace mode — the trap CONDITIONALLY deletes
# the row: only if it still matches this exact provisional write (root +
# nonce + status:"spawning"), never a row that has since been replaced by
# a real join or an explicit claimant (comm-lib.sh:
# registry_del_if_provisional). The outcome — rolled back, left alone
# because it's no longer ours, or the delete itself failed — is reported
# honestly in each case, not just claimed.
#
# NOT covered, and not coverable from here: the daemon boots claude
# ASYNCHRONOUSLY after workspace.create already returned success (a
# throwaway boot-pty, ADR 0023 §3) — if THAT fails or hangs after this
# script has already reported success and exited, the provisional row
# (tmux:"" — never updated, because the real /sot-session-start join never
# ran) is left behind, and since a derived NAME generally differs from the
# workspace slug (it carries "-HOST"; the slug doesn't), `comm-despawn.sh`
# cannot recover the slug from the handle to clean up the orphaned
# workspace/TOML either. This is a real, currently-unfixed gap for that one
# failure mode; there is no synchronous signal in this script's control
# flow to hook a rollback to. `comm-despawn.sh <slug-or-label>` (not the
# handle) still reaches it manually.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/comm-lib.sh"
eval "$("$SCRIPT_DIR/comm-context.sh")"
ensure_home
# Private tmux socket (security review) — daemon-created sessions live here,
# not on tmux's default server. Resolved once, used on every `tmux` call
# below via `-S`.
SOT_TMUX_SOCK="$(sot_tmux_socket)" \
    || { echo "ERROR: could not resolve/secure the private tmux socket dir — see reason above" >&2; exit 1; }

# Spawner's own handle, captured before arg parsing reuses NAME for the
# child. Deliberately NOT synthesized into a "spawner-$HOST" placeholder
# when unresolved (Codex review round-2 SHOULD-FIX 3/G): an unroutable
# fallback sender is exactly the bug class this PR removes everywhere
# else (comm-send.sh/comm-relay.sh/comm-bootstrap.sh no longer stamp
# "unknown-$HOST" either). An unjoined spawner can still spawn a
# fire-and-forget agent with no --task; a --task specifically PROMISES a
# reply route back to @SPAWNER, so that combination is refused below,
# once TASK is known, rather than handed a placeholder nothing can reach.
SPAWNER="$NAME"

NAME=""; REPO_PATH=""; EXPERTISE=""; TASK=""; LABEL=""; DISPLAY_LABEL=""; ENDPOINT=""; NO_WS=false
NAME_FLAG=""; POSITIONAL=()
while [ $# -gt 0 ]; do
    case "$1" in
        --name)          NAME_FLAG="$2"; shift 2 ;;
        --name=*)        NAME_FLAG="${1#--name=}"; shift ;;
        --expertise)     EXPERTISE="$2"; shift 2 ;;
        --task)          TASK="$2"; shift 2 ;;
        --label)         LABEL="$2"; shift 2 ;;
        --display-label) DISPLAY_LABEL="$2"; shift 2 ;;
        --endpoint)      ENDPOINT="$2"; shift 2 ;;
        --no-workspace)  NO_WS=true; shift ;;
        *)               POSITIONAL+=("$1"); shift ;;
    esac
done

if [ -n "$TASK" ]; then
    # --task promises a reply route back to @SPAWNER, so SPAWNER's own
    # identity must be ROUTABLE (registry row present, root matches — not
    # just resolved/nonempty) before any socket/spawn work happens (Codex
    # review round-3 finding 4: a valid self-file with a deleted registry
    # row used to pass the old nonempty-only check, spawn "succeeded", and
    # the task silently never reached the child's inbox). Reuses the same
    # check comm-send/relay/bootstrap already enforce; NAME is reused
    # below for the CHILD and is still "" at this point in the script, so
    # this borrows it briefly rather than adding a parallel helper.
    NAME="$SPAWNER"
    sot_require_routable_identity || exit 1
    NAME=""
fi

# <name> is OPTIONAL (ADR 0028 addendum): one positional is <repo-path> alone
# (name derived below); two positionals is the legacy explicit form
# `<name> <repo-path>`, kept for existing callers (e.g. /worktree). --name is
# the equivalent flag form — either way an explicitly-passed name is used
# VERBATIM, exactly as before.
case "${#POSITIONAL[@]}" in
    1) REPO_PATH="${POSITIONAL[0]}" ;;
    2) NAME="${POSITIONAL[0]}"; REPO_PATH="${POSITIONAL[1]}" ;;
    *) echo "usage: comm-spawn.sh [--name NAME] <repo-path> [--expertise \"...\"] [--task \"...\"] [--label L] [--no-workspace]" >&2; exit 1 ;;
esac
if [ -n "$NAME_FLAG" ]; then
    if [ -n "$NAME" ]; then
        echo "ERROR: name given both as --name and positionally — pick one" >&2; exit 1
    fi
    NAME="$NAME_FLAG"
fi

if [ -z "$REPO_PATH" ]; then
    echo "usage: comm-spawn.sh [--name NAME] <repo-path> [--expertise \"...\"] [--task \"...\"] [--label L] [--no-workspace]" >&2; exit 1
fi
REPO_PATH="${REPO_PATH/#\~/$HOME}"
[ -d "$REPO_PATH" ] || { echo "ERROR: repo path not found: $REPO_PATH" >&2; exit 1; }
# Canonical root — recorded on the provisional registry row below regardless
# of whether NAME ends up derived or explicit (ADR 0028 addendum point 1:
# every claimed handle records its root), and reused for derivation itself.
# Canonicalized ONCE, here, before any claim/lock (Codex review F8) —
# sot_canonical_path fails loudly rather than ever handing back a relative
# path, and that failure must not be swallowed.
if ! CANON_ROOT="$(sot_canonical_path "$REPO_PATH")"; then
    echo "ERROR: could not establish a canonical project root for '$REPO_PATH' — see reason above" >&2
    exit 1
fi
[ -z "$LABEL" ] && LABEL="$(basename "$REPO_PATH")"

# Sessions are named after the REPO (maintainer decision, 2026-06-12): the label drives the
# workspace slug and the tmux session name (sot-be-<slug>), and a
# task-named session is unfindable next to its repo-named siblings (a spawn
# labeled 'edge-classify' hid the MyPackage agent from the user). The
# label must be the repo basename, optionally suffixed ('<Repo>-2') for a
# deliberate second workspace on the same repo. Task identity belongs in
# --task / --expertise, never in the label.
REPO_BASE="$(basename "$REPO_PATH")"
REPO_NAME="$REPO_BASE"

# Validate an EXPLICITLY user-supplied --label BEFORE any registry claim
# (Codex review F9): this format check depends only on args already parsed
# above, never on NAME/derivation — there is no reason to defer it past a
# point where a provisional row could exist, and an invalid --label used
# to write one anyway (claim first, validate after). --display-label
# bypasses this guard entirely (deliberately differing label), same as
# before; an AUTO-composed one (from a derivation qualifier, set below
# once NAME is known) is by construction always "<repo-base>-<qualifier>"
# and always satisfies the pattern, so it needs no separate re-check.
if [ -z "$DISPLAY_LABEL" ] && [ "$LABEL" != "$REPO_BASE" ] && [[ "$LABEL" != "$REPO_BASE"-* ]]; then
    echo "ERROR: --label '$LABEL' must be the repo name '$REPO_BASE' (or '${REPO_BASE}-<suffix>' for a second workspace on the same repo)." >&2
    echo "       Sessions are named after the repo; put task identity in --task (or --display-label for deliberate grouping)." >&2
    exit 1
fi

# The provisional registry row (see the "addressable FROM SPAWN TIME" note
# below) — built here, independent of NAME, so a DERIVED name can be
# decided and written in ONE atomic step (claim_derived_handle) rather than
# derived now and only written later: that gap is exactly the read-then-
# write race the whole feature exists to close, and comm-spawn drives many
# joins back-to-back (bulk workspace bring-up), so the window is real.
#
# PROV_NONCE (Codex review PR #148 round 2, finding 1) tags this exact
# provisional row so rollback (below) can tell "still mine" from "replaced
# by a real join or an explicit claimant" — root+status alone isn't
# enough to rule out a coincidence, and an unconditional `registry_del`
# was proven to delete a genuinely live row the child had already written.
PROV_TS="$(now_iso)"
PROV_NONCE="$$-${RANDOM}-${RANDOM}"
PROV_OBJ="$(jq -n --arg host "$HOST" --arg repo "$REPO_BASE" --arg root "$CANON_ROOT" --arg ts "$PROV_TS" --arg nonce "$PROV_NONCE" \
    '{host:$host, tmux:"", pane_id:"", repo:$repo, root:$root, expertise:[],
      status:"spawning", joined:$ts, last_seen:$ts, nonce:$nonce}')"

# Rollback (Codex review PR #148 round 2, finding 2): armed HERE, BEFORE
# either write path below (a derived claim writes immediately at the next
# block; an explicit name writes later, at the "Provisional registry row"
# section) — not after both, which left a window (the inbox touch, an
# invalid explicit --name whose parent dir doesn't exist) where a row
# could exist with no rollback protection at all. NAME can still be empty
# here (the derived path hasn't resolved it yet); the reporter below
# no-ops until there is something to protect.
#
# Deletion is CONDITIONAL (registry_del_if_provisional, comm-lib.sh,
# finding 1): only a row that still matches root+nonce+status:"spawning"
# is removed. A row that has since been replaced — the child joined for
# real, or (for an explicit name) something else claimed it — is left
# untouched, and that outcome is reported as such, not silently praised as
# "rolled back". A genuine deletion failure is reported as a failure too;
# nothing here claims success it didn't verify.
SPAWN_SUCCEEDED=false
_spawn_rollback_report() {
    [ "$SPAWN_SUCCEEDED" = true ] && return 0
    [ -n "${NAME:-}" ] || return 0
    local rc
    with_lock registry_del_if_provisional "$NAME" "$CANON_ROOT" "$PROV_NONCE"
    rc=$?
    case "$rc" in
        0) echo "comm-spawn: rolled back provisional registry row for @$NAME after a failed spawn" >&2 ;;
        2) echo "comm-spawn: did NOT roll back @$NAME — its row no longer matches what this spawn wrote (something else has since claimed/updated it); left untouched" >&2 ;;
        *) echo "comm-spawn: FAILED to roll back the provisional row for @$NAME (rc=$rc) — check by hand: jq '.agents[\"$NAME\"]' $REGISTRY" >&2 ;;
    esac
}
trap _spawn_rollback_report EXIT

# NAME omitted -> derive it AND write the provisional row atomically, same
# algorithm + same locked-claim path as a plain comm-join.sh (ADR 0028
# addendum; comm-lib.sh: sot_derive_handle / claim_derived_handle) — but in
# FRESH mode (Codex review F3): an existing row for the resolved candidate,
# even one sharing my own root, is a REFUSAL at that tier, not a reclaim.
# comm-spawn creates a NEW agent; silently absorbing an existing row would
# erase a LIVE agent's tmux/pane/status fields. `set -e` makes an outright
# derivation failure (every tier already taken by something else; Codex
# review F6) abort here with sot_derive_handle's own clear stderr reason.
#
# When the derivation had to qualify past the bare <repo>-<host> tier, also
# give the new FE workspace a qualified --display-label
# (<basename>-<qualifier>) so the session-strip rows for the two same-named
# repos stay visually distinguishable — unless the caller already gave an
# explicit --display-label, which wins. AUTO_DISPLAY_LABEL tracks whether
# THIS script composed it (vs. the caller), because only a composed one
# needs the same-workspace-label refusal below (Codex review F5) — an
# explicit --display-label is the caller's own informed choice.
DERIVED_CLAIM=false
AUTO_DISPLAY_LABEL=false
if [ -z "$NAME" ]; then
    DERIVED_CLAIM=true
    claim_derived_handle fresh "$CANON_ROOT" "$HOST" "$PROV_OBJ"
    NAME="$CLAIMED_NAME"
    if [ -n "$CLAIMED_QUALIFIER" ] && [ -z "$DISPLAY_LABEL" ]; then
        DISPLAY_LABEL="${REPO_BASE}-${CLAIMED_QUALIFIER}"
        AUTO_DISPLAY_LABEL=true
    fi
fi

if [ -n "$DISPLAY_LABEL" ]; then
    # Explicit FE label that deliberately differs from the repo basename — e.g.
    # the /worktree tool's '.SoT-wt-<short>' grouping prefix, or the
    # qualifier-composed one just above. It becomes the workspace label
    # (driving slug + sort + tmux name) while the comm HANDLE ($NAME) stays
    # repo-based, so status/clean/sync still group by repo. Bypasses the
    # repo-base guard (already checked above, before any claim) — that
    # guard exists to stop *task*-named labels (e.g. 'edge-classify') from
    # hiding a session, not structured grouping labels.
    LABEL="$DISPLAY_LABEL"
fi

# A DERIVED name was already atomically claimed above (registry row + all)
# — checking "already in registry" again here would always fire (it's
# there because we just put it) and abort every derived spawn. The
# duplicate check only applies to an EXPLICIT name, where the row hasn't
# been written yet.
if [ "$DERIVED_CLAIM" = false ] && jq -e --arg n "$NAME" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1; then
    echo "ERROR: agent '@$NAME' already in registry — pick another name or comm-leave it first" >&2; exit 1
fi
# The agent launches via ccb (maintainer decision, 2026-06-12): its first turn is
# /sot-session-start, so the session joins + listens + arms its own inbox
# Monitor with no hand-rolled join instructions. The handle is pinned by
# prefixing the launch with SOT_COMM_NAME=<name> (comm-join env default).
# ABSOLUTE path because the daemon-created tmux session runs a login shell
# whose PATH may not include ~/.local/bin — a bare `ccb` silently falls
# through to bash. SOT_COMM_LAUNCH remains the escape hatch.
#
# %q-quoted (Codex review F4): this string is later TYPED into a shell pane
# via `tmux send-keys` (the --no-workspace path below) and re-parsed by
# that shell. NAME reaching here has been through sot_sanitize_component
# when derived (safe already), but an EXPLICIT --name is verbatim by
# contract and could contain shell metacharacters — unquoted interpolation
# into a string that gets typed as keystrokes is a command-injection vector
# regardless of where NAME came from, so this is fixed unconditionally.
if [ -n "${SOT_COMM_LAUNCH:-}" ]; then
    LAUNCH="$SOT_COMM_LAUNCH"
else
    LAUNCH="SOT_COMM_NAME=$(printf '%q' "$NAME")"
    [ -n "$EXPERTISE" ] && LAUNCH="$LAUNCH SOT_COMM_EXPERTISE=$(printf '%q' "$EXPERTISE")"
    LAUNCH="$LAUNCH $HOME/.local/bin/ccb"
fi
WAIT="${SOT_COMM_SPAWN_WAIT:-6}"
BIN="$COMM_HOME/bin"

# NO spawn brief (maintainer decision, 2026-06-17). A spawned agent gets its context from its
# repo's own CLAUDE.md and joins comm via /sot-session-start (handle pinned by
# SOT_COMM_NAME) — we do NOT inject a "you are an agent, your task is…" startup
# paste. That brief was unwanted, redundant with the repo CLAUDE.md, and the FE
# re-injected it on every workspace re-attach. The workspace `task` field is left
# EMPTY so the FE has nothing to deliver. If --task was given it is sent AFTER
# spawn as an ordinary durable comm message to the agent's inbox — the normal
# channel, read on the agent's /sot-session-start backlog poll.
TASKMSG=""
[ -n "$TASK" ] && TASKMSG="Task from @${SPAWNER}: ${TASK} — reply to @${SPAWNER} via ${BIN}/comm-send.sh when done or blocked (your local text is invisible to peers)."

# --- resolve the daemon endpoint (workspace mode only) ---
resolve_endpoint() {
    sot_daemon_endpoint "${ENDPOINT:-${SOT_SPAWN_ENDPOINT:-}}"
}

# Send a frame to the daemon, return the first response line matching op $2.
# App-level auth (ADR 0010 hardening): daemon requires a token-valid hello first.
_sot_hello() {
    local tok; tok="${SOT_TOKEN:-$(cat "${XDG_CONFIG_HOME:-$HOME/.config}/sot/token" 2>/dev/null || true)}"
    printf '{"v":1,"id":1,"kind":"req","op":"hello","payload":{"client_id":"sot-comm","last_seen_revision":0,"protocol":1,"app_version":"comm","token":"%s"}}\n' "$tok"
}
sot_send() {
    local frame="$1" op="$2" hp
    case "$ENDPOINT" in
        tcp:*)  hp="${ENDPOINT#tcp:}"
                { _sot_hello; printf '%s\n' "$frame"; } | timeout 6 nc "${hp%:*}" "${hp##*:}" 2>/dev/null | grep -m1 "\"op\":\"$op\"" ;;
        unix:*) sot_oneshot_request "$frame" "$op" ;;
        *)      return 1 ;;
    esac
}

TARGET=""   # tmux target to launch claude into

# Provisional registry row + inbox, so the agent is addressable FROM SPAWN TIME:
# comm-send refuses unregistered handles, and without this the spawner had to
# sit out the agent's whole boot before its first message. With the row + inbox
# in place, anyone can comm-send @<name> immediately — the line queues durably,
# and the agent's /sot-session-start bootstrap reads the backlog (comm-poll,
# step 4) and replies once it's up (~1 min). The real join later overwrites
# this row with full pane/expertise info; comm-despawn cleans it if the spawn
# never boots. For a DERIVED name, PROV_OBJ was already written atomically
# above (claim_derived_handle) — only an EXPLICIT name still needs the
# write here (its collision, if any, was already ruled out above).
if [ "$DERIVED_CLAIM" = false ]; then
    with_lock registry_put "$NAME" "$PROV_OBJ"
fi
: >> "$INBOX_DIR/$NAME.jsonl"

# Rollback protection is already armed (see the trap set right after
# PROV_OBJ was built, above) — before either write path, including this
# one and the inbox touch just above it.

if [ "$NO_WS" = true ]; then
    SESSION="$NAME"
    if tmux -S "$SOT_TMUX_SOCK" has-session -t "$SESSION" 2>/dev/null; then
        echo "ERROR: tmux session '$SESSION' already exists" >&2; exit 1
    fi
    tmux -S "$SOT_TMUX_SOCK" new-session -d -s "$SESSION" -c "$REPO_PATH"
    TARGET="$SESSION"
    echo "Created raw tmux session '$SESSION' at $REPO_PATH (no workspace; not in FE strip)"
else
    if ! command -v nc >/dev/null 2>&1; then
        echo "ERROR: nc not found — needed to reach the daemon. Use --no-workspace for a raw session." >&2; exit 1
    fi
    if ! ENDPOINT="$(resolve_endpoint)"; then
        echo "ERROR: could not find the sotd daemon. Set --endpoint unix:/path or tcp:HOST:PORT, or use --no-workspace." >&2; exit 1
    fi
    # Same-label refusal (Codex review F5, hardened in round 2 finding 3):
    # an auto-composed display label (base-qualifier) must not be able to
    # collide with an EXISTING workspace — e.g. a worktree's
    # '<repo>-wt-<short>' grouping label happening to equal our
    # qualifier-composed one. workspace.create itself gives no usable
    # signal for this: same-slug is, BY DESIGN, an id-preserving metadata
    # refresh (`Workspaces::insert`, `rust/backend/src/workspaces.rs`) that
    # boot/spawn flows rely on for idempotence, and the duplicate-root gate
    # explicitly treats a same-slug match as invisible
    # (`find_other_workspace_with_root`'s doc comment,
    # `rust/backend/src/handlers.rs`) — a colliding create would silently
    # rebind the existing workspace's project_root/tmux_session rather than
    # erroring. So this is a pre-check, not a reply-reaction: list existing
    # workspaces and refuse before ever calling workspace.create if our
    # composed label would collide. Only for an AUTO-composed label — an
    # explicit --display-label is the caller's own informed choice and
    # keeps today's behavior.
    #
    # FAILS CLOSED: a workspace.list that doesn't answer, or answers with
    # something unparseable, refuses the spawn outright rather than
    # treating "we couldn't check" as "no collision" — the daemon being
    # down costs nothing extra here since the create below would fail
    # anyway, but a TRANSIENT list-only hiccup followed by a working
    # create must not bypass the guard silently.
    #
    # Compared by NORMALIZED SLUG (sot_slug, comm-lib.sh — a verified bash
    # mirror of `rust/backend/src/paths.rs::slug`), not the raw label
    # string: workspace.create refreshes by slug, so two labels that only
    # differ by case, or by a dot vs underscore, resolve to the SAME
    # workspace and must be caught too, not just a byte-identical match.
    # An existing entry's OWN `.slug` field is used directly (not
    # re-derived from its label) — the daemon's own computed value is more
    # trustworthy than re-slugifying it a second time client-side.
    #
    # This remains a list-then-create TOCTOU, not an atomic guarantee — a
    # BEST-EFFORT human-UX guard against the common case (another comm-spawn
    # or a worktree create landing moments apart), not a correctness
    # guarantee against true concurrent creates. An atomic daemon
    # create-if-absent is the real fix for that and is out of scope here.
    if [ "$AUTO_DISPLAY_LABEL" = true ]; then
        if ! LIST="$(sot_send '{"v":1,"id":1,"kind":"req","op":"workspace.list","payload":{}}' workspace.list)"; then
            echo "ERROR: could not confirm the auto-derived display label '$LABEL' is collision-free (workspace.list did not answer) — refusing to spawn. Pass --name or --display-label, or retry once the daemon answers." >&2
            exit 1
        fi
        if ! printf '%s' "$LIST" | jq -e '.payload.workspaces' >/dev/null 2>&1; then
            echo "ERROR: workspace.list returned a malformed reply — refusing to spawn with an unverified auto-derived display label '$LABEL'. Pass --name or --display-label, or retry." >&2
            exit 1
        fi
        CANDIDATE_SLUG="$(sot_slug "$LABEL")"
        if printf '%s' "$LIST" | jq -e --arg s "$CANDIDATE_SLUG" '.payload.workspaces[] | select(.slug == $s)' >/dev/null 2>&1; then
            echo "ERROR: the auto-derived display label '$LABEL' (slug '$CANDIDATE_SLUG') already names an existing workspace — refusing to risk rebinding it. Pass --name or --display-label to pick a distinct one." >&2
            exit 1
        fi
    fi
    # task:"" — no brief on the wire; the FE has nothing to paste on attach. Any
    # --task is sent below as an ordinary durable comm message instead.
    # boot:true (ADR 0023 §3) — the DAEMON boots claude via a throwaway boot-pty
    # (no FE attach / no session switch needed), so a background spawn comes up
    # running claude even if no frontend ever navigates to it. autostart_claude
    # stays true as the FE-attach fallback (the foreground guard de-dupes).
    REQ="$(jq -nc --arg l "$LABEL" --arg p "$REPO_PATH" --arg an "$NAME" \
        '{v:1,id:1,kind:"req",op:"workspace.create",payload:{label:$l,project_root:$p,autostart_claude:true,agent_name:$an,task:"",boot:true}}')"
    RESP="$(sot_send "$REQ" workspace.create || true)"
    SLUG="$(printf '%s' "$RESP" | jq -r '.payload.slug // empty' 2>/dev/null || true)"
    TARGET="$(printf '%s' "$RESP" | jq -r '.payload.tmux_session // empty' 2>/dev/null || true)"
    if [ -z "$SLUG" ] || [ -z "$TARGET" ]; then
        echo "ERROR: workspace.create failed via $ENDPOINT" >&2
        [ -n "$RESP" ] && printf '  daemon said: %s\n' "$(printf '%s' "$RESP" | jq -c '.payload' 2>/dev/null || printf '%s' "$RESP")" >&2
        exit 1
    fi
    echo "Created workspace '$LABEL' (slug=$SLUG, tmux=$TARGET) via $ENDPOINT"
    tmux -S "$SOT_TMUX_SOCK" has-session -t "$TARGET" 2>/dev/null || { echo "ERROR: daemon reported $TARGET but tmux session is missing" >&2; exit 1; }
    # Workspace mode's actionable work is done and verified — nothing left
    # in this branch can fail (the reporting below is tolerant/WARN-only,
    # matching the async-boot gap documented at the top of this file). Set
    # here, not shared with the --no-workspace branch below (Codex review
    # PR #148 round 2, finding 2): --no-workspace's real "did it launch"
    # moment is `tmux send-keys`, further down, not tmux session creation.
    SPAWN_SUCCEEDED=true
fi

if [ "$NO_WS" = true ]; then
    # Headless / no daemon: launch ccb directly. No brief paste — the agent reads
    # its repo CLAUDE.md and joins comm via /sot-session-start; any --task is
    # delivered below as a durable comm message, not a startup paste.
    sleep 0.5
    tmux -S "$SOT_TMUX_SOCK" send-keys -t "$TARGET" "$LAUNCH" Enter
    # Only NOW has --no-workspace's actionable work (the launch itself)
    # happened — a `send-keys` failure above must still roll back.
    SPAWN_SUCCEEDED=true
    echo "Launched: $LAUNCH  (waiting ${WAIT}s for boot)"
    echo "Spawned (raw) @${NAME} on ${REPO_NAME} in session '$TARGET'."
else
    # Workspace mode: the workspace carries autostart_claude=true + agent_name on
    # the wire — task is EMPTY, no brief. The FE reads them off workspace.list
    # and, on first attach, launches ccb with SOT_COMM_NAME=<agent_name> (it
    # owns the terminal; a detached session can't init claude). The agent joins
    # comm + reads its repo CLAUDE.md; nothing is pasted.
    [ -n "$SPAWNER" ] && with_lock registry_touch "$SPAWNER" 2>/dev/null || true
    echo "Spawned @${NAME} as workspace '${SLUG}' on ${REPO_NAME} (autostart_claude=true; NO brief — agent uses its repo CLAUDE.md)."
    if [ -n "$SPAWNER" ]; then
        echo "The FE auto-starts ccb on first attach; the agent joins comm (~1 min) and reports to @${SPAWNER}."
    else
        echo "The FE auto-starts ccb on first attach; the agent joins comm (~1 min). This spawning session has no resolved identity of its own, so give the agent an explicit reply target if one is needed."
    fi
fi
# Deliver any --task as an ordinary durable comm message (NOT a startup brief):
# it queues in the agent's inbox now and is read on its /sot-session-start poll.
if [ -n "$TASKMSG" ]; then
    if "$BIN/comm-send.sh" @"$NAME" "$TASKMSG" >/dev/null 2>&1; then
        echo "Task queued to @${NAME}'s inbox (durable; read on bootstrap)."
    else
        # A failed enqueue must fail the command, not just warn (Codex
        # review round-3 finding 4) — the routability pre-check above
        # covers the common case, but a successful queue is still a
        # required postcondition for a promised task, not an assumption.
        # The agent itself already spawned successfully (SPAWN_SUCCEEDED
        # is set) so the exit trap will NOT roll it back — only the task
        # promise failed.
        echo "ERROR: --task was given but could not be queued to @${NAME}'s inbox — send it yourself: ${BIN}/comm-send.sh @${NAME} \"...\"" >&2
        exit 1
    fi
fi
echo "@${NAME} is addressable NOW: ${BIN}/comm-send.sh @${NAME} \"...\" queues durably in its inbox,"
echo "and the agent reads the backlog + replies once its comm bootstrap finishes (~1 min after first attach)."
echo "Watch: ${BIN}/comm-list.sh  /  ${BIN}/comm-poll.sh"
