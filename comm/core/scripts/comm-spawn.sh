#!/usr/bin/env bash
# comm-spawn.sh — spawn a new agent to work on another package and report back
# over sot-comm. By default the agent is created as a Ship of Tools *workspace*, so
# it appears in the frontend session strip and is switchable (Ctrl+PageDown);
# switching also gives you that package's files/REPL/concept.
#
# Usage:
#   comm-spawn.sh <name> <repo-path> [--expertise "a, b"] [--task "do X"]
#                 [--label LABEL] [--endpoint tcp:H:P|unix:PATH] [--no-workspace]
#
#   <name>        sot-comm handle for the new agent
#   <repo-path>   package the agent works in (workspace project root)
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
#      SOT_COMM_LAUNCH (default 'claude --dangerously-skip-permissions')
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

# Spawner's own handle, captured before arg parsing reuses NAME for the child.
SPAWNER="$NAME"
[ -z "$SPAWNER" ] && SPAWNER="spawner-$HOST"

NAME=""; REPO_PATH=""; EXPERTISE=""; TASK=""; LABEL=""; DISPLAY_LABEL=""; ENDPOINT=""; NO_WS=false
while [ $# -gt 0 ]; do
    case "$1" in
        --expertise)     EXPERTISE="$2"; shift 2 ;;
        --task)          TASK="$2"; shift 2 ;;
        --label)         LABEL="$2"; shift 2 ;;
        --display-label) DISPLAY_LABEL="$2"; shift 2 ;;
        --endpoint)      ENDPOINT="$2"; shift 2 ;;
        --no-workspace)  NO_WS=true; shift ;;
        *)              if [ -z "$NAME" ]; then NAME="$1"
                        elif [ -z "$REPO_PATH" ]; then REPO_PATH="$1"; fi; shift ;;
    esac
done

if [ -z "$NAME" ] || [ -z "$REPO_PATH" ]; then
    echo "usage: comm-spawn.sh <name> <repo-path> [--expertise \"...\"] [--task \"...\"] [--label L] [--no-workspace]" >&2; exit 1
fi
REPO_PATH="${REPO_PATH/#\~/$HOME}"
[ -d "$REPO_PATH" ] || { echo "ERROR: repo path not found: $REPO_PATH" >&2; exit 1; }
[ -z "$LABEL" ] && LABEL="$(basename "$REPO_PATH")"

# Sessions are named after the REPO (maintainer decision, 2026-06-12): the label drives the
# workspace slug and the tmux session name (sot-be-<slug>), and a
# task-named session is unfindable next to its repo-named siblings (a spawn
# labeled 'edge-classify' hid the MyPackage agent from the user). The
# label must be the repo basename, optionally suffixed ('<Repo>-2') for a
# deliberate second workspace on the same repo. Task identity belongs in
# --task / --expertise, never in the label.
REPO_BASE="$(basename "$REPO_PATH")"
if [ -n "$DISPLAY_LABEL" ]; then
    # Explicit FE label that deliberately differs from the repo basename — e.g.
    # the /worktree tool's '.SoT-wt-<short>' grouping prefix. It becomes the
    # workspace label (driving slug + sort + tmux name) while the comm HANDLE
    # ($NAME) stays repo-based, so status/clean/sync still group by repo. Bypasses
    # the repo-base guard below, which exists to stop *task*-named labels (e.g.
    # 'edge-classify') from hiding a session — not structured grouping labels.
    LABEL="$DISPLAY_LABEL"
elif [ "$LABEL" != "$REPO_BASE" ] && [[ "$LABEL" != "$REPO_BASE"-* ]]; then
    echo "ERROR: --label '$LABEL' must be the repo name '$REPO_BASE' (or '${REPO_BASE}-<suffix>' for a second workspace on the same repo)." >&2
    echo "       Sessions are named after the repo; put task identity in --task (or --display-label for deliberate grouping)." >&2
    exit 1
fi

if jq -e --arg n "$NAME" '.agents[$n]' "$REGISTRY" >/dev/null 2>&1; then
    echo "ERROR: agent '@$NAME' already in registry — pick another name or comm-leave it first" >&2; exit 1
fi

REPO_NAME="$(basename "$REPO_PATH")"
# The agent launches via ccb (maintainer decision, 2026-06-12): its first turn is
# /sot-session-start, so the session joins + listens + arms its own inbox
# Monitor with no hand-rolled join instructions. The handle is pinned by
# prefixing the launch with SOT_COMM_NAME=<name> (comm-join env default).
# ABSOLUTE path because the daemon-created tmux session runs a login shell
# whose PATH may not include ~/.local/bin — a bare `ccb` silently falls
# through to bash. SOT_COMM_LAUNCH remains the escape hatch.
if [ -n "${SOT_COMM_LAUNCH:-}" ]; then
    LAUNCH="$SOT_COMM_LAUNCH"
else
    LAUNCH="SOT_COMM_NAME=${NAME}${EXPERTISE:+ SOT_COMM_EXPERTISE=\"${EXPERTISE}\"} $HOME/.local/bin/ccb"
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
        unix:*) { _sot_hello; printf '%s\n' "$frame"; } | timeout 6 nc -U "${ENDPOINT#unix:}" 2>/dev/null | grep -m1 "\"op\":\"$op\"" ;;
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
# never boots.
ts="$(now_iso)"
prov="$(jq -n --arg host "$HOST" --arg repo "$REPO_NAME" --arg ts "$ts" \
    '{host:$host, tmux:"", pane_id:"", repo:$repo, expertise:[],
      status:"spawning", joined:$ts, last_seen:$ts}')"
with_lock registry_put "$NAME" "$prov"
: >> "$INBOX_DIR/$NAME.jsonl"

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
fi

if [ "$NO_WS" = true ]; then
    # Headless / no daemon: launch ccb directly. No brief paste — the agent reads
    # its repo CLAUDE.md and joins comm via /sot-session-start; any --task is
    # delivered below as a durable comm message, not a startup paste.
    sleep 0.5
    tmux -S "$SOT_TMUX_SOCK" send-keys -t "$TARGET" "$LAUNCH" Enter
    echo "Launched: $LAUNCH  (waiting ${WAIT}s for boot)"
    echo "Spawned (raw) @${NAME} on ${REPO_NAME} in session '$TARGET'."
else
    # Workspace mode: the workspace carries autostart_claude=true + agent_name on
    # the wire — task is EMPTY, no brief. The FE reads them off workspace.list
    # and, on first attach, launches ccb with SOT_COMM_NAME=<agent_name> (it
    # owns the terminal; a detached session can't init claude). The agent joins
    # comm + reads its repo CLAUDE.md; nothing is pasted.
    with_lock registry_touch "$SPAWNER" 2>/dev/null || true
    echo "Spawned @${NAME} as workspace '${SLUG}' on ${REPO_NAME} (autostart_claude=true; NO brief — agent uses its repo CLAUDE.md)."
    echo "The FE auto-starts ccb on first attach; the agent joins comm (~1 min) and reports to @${SPAWNER}."
fi
# Deliver any --task as an ordinary durable comm message (NOT a startup brief):
# it queues in the agent's inbox now and is read on its /sot-session-start poll.
if [ -n "$TASKMSG" ]; then
    if "$BIN/comm-send.sh" @"$NAME" "$TASKMSG" >/dev/null 2>&1; then
        echo "Task queued to @${NAME}'s inbox (durable; read on bootstrap)."
    else
        echo "WARN: could not queue task — send it yourself: ${BIN}/comm-send.sh @${NAME} \"...\""
    fi
fi
echo "@${NAME} is addressable NOW: ${BIN}/comm-send.sh @${NAME} \"...\" queues durably in its inbox,"
echo "and the agent reads the backlog + replies once its comm bootstrap finishes (~1 min after first attach)."
echo "Watch: ${BIN}/comm-list.sh  /  ${BIN}/comm-poll.sh"
