# sot-fe's daemon request: resolve the endpoint, send one frame, the fe.command verbs, workspace lookup.
# Sourced by sot-fe; defines functions only.

# --- resolve the daemon endpoint ---
resolve_endpoint() {
    sot_daemon_endpoint "${ENDPOINT:-${SOT_FE_ENDPOINT:-${SOT_SPAWN_ENDPOINT:-}}}"
}

# Send a frame to the daemon, return the first response line matching op $2.
# Uses nc when present; otherwise falls back to bash /dev/tcp for tcp endpoints
# (so this works in git-bash on Windows). A unix-socket endpoint still needs
# nc -U. The exec lives in a subshell: a redirect-only exec whose redirect fails
# would otherwise EXIT a non-interactive shell outright (same class as the
# comm-relay.sh /dev/tcp fix).
# Hello: the daemon reads each connection's first frame for the protocol
# version and ignores its token field — `sot_oneshot_request` prepends
# `sot_hello_frame` (comm-lib.sh, ADR 0046 decision 1) to every connection.
# sot_send takes the reply by its op; `sot_oneshot_request` names a refused hello when no reply came.
sot_send() {
    # Delegates to comm-lib's hardened one-shot (poll-file read, writer
    # linger) — the old inline `writer | nc | grep -m1` raced the response
    # on a busy daemon. See sot_oneshot_request in comm-lib.sh.
    local frame="$1" op="$2"
    sot_oneshot_request "$frame" "$op"
}

# Frame an `fe.command.send` req from a cmd verb ($1) + an args JSON object ($2),
# attaching `target` from --fe when set, and send it to the daemon.
send_fe_command() {
    local fe_cmd="$1" args_json="$2"
    if ! ENDPOINT="$(resolve_endpoint)"; then
        echo "ERROR: could not find the sotd daemon. Set --endpoint unix:/path, ssh:target[/host], or (Windows) pipe:name (or \$SOT_FE_ENDPOINT)." >&2
        exit 1
    fi
    # $FE (fe@<host>) and fe_cmd are both id-shaped tokens (an address;
    # a small fixed set of literal verbs from the case
    # dispatch below) -- neither can legitimately start with "/", so both
    # stay --arg (allowlisted; see the audit test in
    # test-join-disambiguation.sh).
    local payload
    if [ -n "$FE" ]; then
        payload="$(jq -nc --arg c "$fe_cmd" --argjson a "$args_json" --arg t "$FE" \
            '{cmd:$c, args:$a, target:$t}')"
    else
        payload="$(jq -nc --arg c "$fe_cmd" --argjson a "$args_json" \
            '{cmd:$c, args:$a}')"
    fi
    local req
    req="$(jq -nc --argjson p "$payload" \
        '{v:1, id:1, kind:"req", op:"fe.command.send", payload:$p}')"
    local resp
    resp="$(sot_send "$req" fe.command.send || true)"
    if [ -z "$resp" ]; then
        # Empty response = the read lost a race (busy daemon, giant
        # broadcast frames queued ahead) — NOT proof of refusal: the daemon
        # broadcasts BEFORE acking, so the command may already have been
        # delivered. Retry once ONLY for idempotent commands (a replayed
        # preview/reveal/goto/mode/notify is a no-op FE-side); open_url and
        # docs.open would open a second tab, so those fail loud with a
        # rerun hint instead (codex review).
        case "$fe_cmd" in
            open_url|docs)
                echo "WARNING: no response — the command may or may not have been delivered." >&2
                echo "  (open commands are not auto-retried: a duplicate would open a second tab." >&2
                echo "   Check the target FE; rerun manually if nothing opened.)" >&2
                ;;
            *)
                echo "note: no response on first attempt — retrying once" >&2
                sleep 1
                resp="$(sot_send "$req" fe.command.send || true)"
                ;;
        esac
    fi
    local ok
    ok="$(printf '%s' "$resp" | jq -r '.payload.ok // empty' 2>/dev/null || true)"
    if [ "$ok" != "true" ]; then
        echo "ERROR: fe.command.send ($fe_cmd) failed via $ENDPOINT" >&2
        [ -n "$resp" ] && printf '  daemon said: %s\n' "$resp" >&2
        exit 1
    fi
    # `delivered_to` (2026-09-09 field incident: an untargeted open-url acked
    # ok:true TWICE while landing on a machine other than the one the owner
    # was sitting at) is the daemon's own count of attached frontends this
    # command actually reached — ground truth, not a guess from --fe's
    # string shape. Absent means an old daemon that predates the field; only
    # there do we fall back to the previous blind-ack behaviour.
    local has_delivered delivered_to resolved_target
    has_delivered="$(printf '%s' "$resp" | jq -r '.payload | has("delivered_to")' 2>/dev/null || true)"
    delivered_to="$(printf '%s' "$resp" | jq -r '.payload.delivered_to // empty' 2>/dev/null || true)"
    resolved_target="$(printf '%s' "$resp" | jq -r '.payload.resolved_target // empty' 2>/dev/null || true)"
    if [ "$has_delivered" = "true" ]; then
        # Ground truth, every verb: nothing can act on this send.
        if [ "$delivered_to" = "0" ]; then
            echo "ERROR: fe.command.send ($fe_cmd) reached NO attached frontend${FE:+ (target '$FE' matched nothing)}." >&2
            echo "  'sot-fe version' lists attached frontends (fe@<host> + active|idle)." >&2
            exit 2
        fi
        if [ -n "$resolved_target" ]; then
            echo "fe.command sent: cmd=$fe_cmd target=$resolved_target via $ENDPOINT"
        else
            echo "fe.command sent: cmd=$fe_cmd broadcast to $delivered_to frontend(s) via $ENDPOINT"
        fi
    else
        # Old daemon (predates delivered_to): relaunch with no --fe keeps
        # its 2026-09-08-review guard as the only signal available.
        # `resolved_target` absent means EITHER "no frontend is active right
        # now" OR "this daemon predates resolved_target" (it still
        # broadcasts the untouched request, and every FE refuses an
        # undirected relaunch -- nothing bounces, but nothing happens
        # either) -- both read identically: fail visibly instead of
        # reporting success for a command nobody executed. A current daemon
        # never reaches this branch -- see the delivered_to rule above.
        if [ "$fe_cmd" = relaunch ] && [ -z "${FE:-}" ] && [ -z "$resolved_target" ]; then
            echo "ERROR: no active frontend to relaunch -- supply --fe <host> ('sot-fe version' lists attached frontends)." >&2
            exit 2
        fi
        echo "fe.command sent: cmd=$fe_cmd${FE:+ target=$FE} via $ENDPOINT"
    fi
    case "$fe_cmd" in
        preview|reveal)
            echo "note: badges the target workspace row — the user is never yanked; when they visit that workspace the file is cursored in the nav AND rendered in the preview (complete show, always). --urgent --fe <host> is the user-requested focus-capture option." ;;
    esac
}

# Resolve ENDPOINT in the CALLER's shell (never from inside a $( ) subshell),
# reporting an unreachable daemon as its own outcome. That distinction is
# load-bearing: a transport failure and a dead REPL look identical from a
# failing CLI and point at opposite fixes — one is your endpoint, the other is
# the kernel. A stale tcp: endpoint aimed at a dead port lands here.
_need_endpoint() {
    if ! ENDPOINT="$(resolve_endpoint)"; then
        echo "ERROR: could not find the sotd daemon — this is a TRANSPORT problem, not a REPL problem. Set --endpoint unix:/path, ssh:target[/host], or (Windows) pipe:name (or \$SOT_FE_ENDPOINT). A stale tcp: endpoint pointing at a dead port fails exactly like this." >&2
        exit 1
    fi
}

# Fetch workspace.list once and echo the row matching $1 (by workspace_id,
# label OR slug) as compact JSON; empty when nothing matched.
#
# The daemon's own resolver accepts workspace_id and slug but NOT label, so a
# perfectly reasonable `repl interrupt <Label>` would come back as
# "unknown_workspace" from the daemon. Resolving here instead lets every repl
# verb accept the same three identifiers and fail with a message that names the
# alternatives.
# NOTE: this runs inside $( ) — a SUBSHELL — so it must NOT be the thing that
# resolves ENDPOINT: an assignment here dies with the subshell and the caller
# would send to an empty endpoint. Callers resolve first (see _need_endpoint).
_ws_row() {
    local want="$1" req resp err
    req="$(jq -nc '{v:1, id:2, kind:"req", op:"workspace.list", payload:{}}')"
    resp="$(SEND_TIMEOUT="${SEND_TIMEOUT:-15}" sot_send "$req" workspace.list || true)"
    if [ -z "$resp" ]; then
        echo "ERROR: the daemon did not answer workspace.list via $ENDPOINT. TRANSPORT, not the REPL — nothing can be concluded about any kernel." >&2
        exit 1
    fi
    # A daemon that REJECTS the request (bad/missing token, protocol skew)
    # answers with the SAME op and a {error, code} payload — which passes the
    # empty check above and then has no .workspaces to iterate. Unguarded,
    # that crashed jq ("Cannot iterate over null") on exactly the recovery
    # verbs a user runs when things are already broken. Surface the daemon's
    # own words instead.
    err="$(printf '%s' "$resp" | jq -r '.payload.error // empty' 2>/dev/null || true)"
    if [ -n "$err" ]; then
        echo "ERROR: the daemon refused workspace.list: $err [$(printf '%s' "$resp" | jq -r '.payload.code // "?"')] — likely a token/auth problem (\$SOT_TOKEN / ~/.config/sot/token), not a REPL problem." >&2
        exit 1
    fi
    # $want is a workspace_id/label/slug identifier -- id-shaped, same
    # allowlisted category as $WS/$ws_id elsewhere in this file.
    printf '%s' "$resp" | jq -c --arg w "$want" \
        'first((.payload.workspaces // [])[] | select(.workspace_id == $w or .label == $w or .slug == $w)) // empty'
}

# Best-effort id resolution for run/eval: echo $1's canonical workspace_id, or
# $1 UNCHANGED if it cannot be resolved. Strictly additive — every string that
# works today still goes through verbatim — so this can only ADD label support,
# never take away an id/slug that already worked (these verbs are called from
# skills and other scripts; a hard failure here would be a regression). Without
# it `repl status <Label>` would work while `repl eval <Label>` returned
# unknown_workspace, since the daemon's resolver takes id/slug only.
#
# COST DISCIPLINE: this sits on the run/eval hot path, so it must not add a
# round-trip when it cannot help. A canonical `ws-*` id needs no resolution —
# skip the lookup entirely. For everything else the lookup's socket budget is
# capped at min(--timeout, 10)s so a slow daemon cannot burn more of the
# caller's budget pre-flight than the caller allotted overall. A failed lookup
# passes through with one stderr note — silent downgrade would resurface later
# as a misattributed repl.execute failure.
_ws_id_or_passthrough() {
    local want="$1" row="" cap=10
    case "$want" in
        ws-*) printf '%s' "$want"; return 0 ;;
    esac
    if [ -n "$TIMEOUT" ] && [ "$TIMEOUT" -lt "$cap" ]; then cap="$TIMEOUT"; fi
    row="$(SEND_TIMEOUT="$cap" _ws_row "$want" 2>/dev/null || true)"
    if [ -n "$row" ]; then
        printf '%s' "$row" | jq -r '.workspace_id'
    else
        echo "note: could not resolve '$want' via workspace.list (daemon slow, unreachable, or no such workspace) — sending it to the daemon as-is." >&2
        printf '%s' "$want"
    fi
}
