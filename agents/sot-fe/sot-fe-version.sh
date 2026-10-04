# sot-fe's version report: the daemon's build, its frontends and every capsule row's phase.
# Sourced by sot-fe; defines functions only.

# ADR 0030 §8 decision 31 (ADR 0043 decision 31): what build the daemon at
# --endpoint is, every frontend attached to it, and every capsule row's own
# phase, verbatim — see this verb's own usage-block doc above for the full
# column/exit contract. One version.query + one workspace.list against THIS
# ONE daemon; run again with a different --endpoint for the other side --
# this verb never fans out to both itself.
send_version() {
    _need_endpoint
    export SEND_TIMEOUT="${TIMEOUT:-15}"

    local vreq vresp verr
    vreq="$(jq -nc '{v:1, id:2, kind:"req", op:"version.query", payload:{}}')"
    vresp="$(sot_send "$vreq" version.query || true)"
    if [ -z "$vresp" ]; then
        echo "ERROR: the daemon did not answer version.query within ${SEND_TIMEOUT}s via $ENDPOINT. TRANSPORT, not a version problem." >&2
        exit 2
    fi
    verr="$(printf '%s' "$vresp" | jq -r '.payload.error // empty')"
    local daemon_version daemon_build daemon_lane_proto daemon_host
    if [ -n "$verr" ]; then
        # The ONE shape every daemon this verb must never fail against:
        # the generic unknown-op reply (server/dispatch.rs's `other =>` catch-all,
        # SAME op echoed back on a `res` frame) means "daemon predates
        # this op" -- print `unknown`, keep going, still exit 0. Any OTHER
        # refusal (a real error) is a genuine failure.
        if [ "$verr" = "unknown op: version.query" ]; then
            daemon_version="unknown"
            daemon_build="unknown"
            daemon_host="unknown"
        else
            echo "ERROR: the daemon refused version.query: $verr [$(printf '%s' "$vresp" | jq -r '.payload.code // "?"')]" >&2
            exit 2
        fi
    else
        daemon_version="$(printf '%s' "$vresp" | jq -r '.payload.daemon.app_version')"
        daemon_build="$(printf '%s' "$vresp" | jq -r '.payload.daemon.lane_build')"
        daemon_lane_proto="$(printf '%s' "$vresp" | jq -r '.payload.daemon.lane_proto // "?"')"
        # topology plan §F step 1: `DaemonVersion.host`, `#[serde(default)]`
        # on the Rust side -- a daemon predating this field answers without
        # it, so `// "?"` mirrors the lane_proto placeholder rather than a
        # blank cell.
        daemon_host="$(printf '%s' "$vresp" | jq -r '.payload.daemon.host // "?"')"
        [ -n "$daemon_host" ] || daemon_host="?"
        daemon_build="${daemon_build} (lane proto ${daemon_lane_proto}, host ${daemon_host}"
        # Session-listing brief: `uptime_s` is what makes a daemon
        # restart's forgetting of the `disconnected` map VISIBLE rather
        # than silently read as "no sessions" -- omitted (not "up 0")
        # when a daemon predates the field (`// 0`, indistinguishable
        # from a fresh boot, so print nothing rather than a misleading 0).
        local daemon_uptime_s; daemon_uptime_s="$(printf '%s' "$vresp" | jq -r '.payload.daemon.uptime_s // 0')"
        if [ "$daemon_uptime_s" -gt 0 ] 2>/dev/null; then
            daemon_build="${daemon_build}, up $(fmt_age "$daemon_uptime_s" | sed 's/ ago$//')"
        fi
        daemon_build="${daemon_build})"
    fi

    printf '%-20s %-28s %s\n' "component" "version" "build"
    if [ -z "$verr" ]; then
        # A client with a self-reported declared `.name` (a frontend's
        # `fe@<host>` address, a bridge/cli/agent's own handle) is
        # printed on its own row (never
        # collapsed, each box is distinct). topology plan §F step 1: when
        # the client also declared a `.host`, the row is labeled
        # `<role>@<host>` -- the declared handle itself is dropped from
        # the label (this is what makes "which frontend am I on"
        # answerable without reading a log; disambiguating two FEs on the
        # SAME host by `instance` is later grammar-v2 work, not this
        # step). A client that declared a name but no host falls back to
        # the "role name" label so it is never silently dropped. Either
        # way the row still ends
        # "\tversion\tactive|idle" -- the declaration, not a re-derivation.
        # No idle AGE is shown (deleted 2026-09-08 review, finding 6): the daemon's
        # expiry/tie policy is the only thing entitled to decide "how
        # idle" something is, so this script only ever asks "which one, if
        # any, is active" and prints that. Everything else (a client_id
        # with no declared handle -- every comm-script connection registers
        # as "sot-comm") keeps the pre-existing collapse: identical
        # (client_id, version) pairs fold into one row with a count, since
        # a busy backend has a dozen of those at once.
        # Session-listing brief decision 2: a named row ALSO carries
        # `.sessions` (the handles that box's OWN daemon owns, `None`
        # for an older frontend or a frontend box with no daemon at all,
        # `Some([])` for one that's declared and has nothing to report).
        # Each line below is tagged with a leading "kind" field so the
        # while-loop can tell a normal three-column row from a session
        # line WITHOUT guessing from field count — `.[] as $x | (header),
        # (sessions)` keeps a row's session lines immediately after it,
        # never clustered by kind the way two separate `map` groups would.
        local nows; nows="$(date -u +%s)"
        printf '%s' "$vresp" | jq -r '(.payload.clients // []) as $c
            | ( $c | map(select(.name != null and .name != "")) | .[] as $x
                | ( "row\t" + (if $x.host then $x.role + "@" + $x.host
                               else $x.role + " " + $x.name end)
                      + "\t" + $x.app_version + "\t"
                      + (if $x.active then "active" else "idle" end) ),
                  ( if ($x.sessions == null) then
                      "nosess"
                    elif ($x.sessions | length) == 0 then
                      "emptysess"
                    else
                      ($x.sessions[] | "sess\t" + .handle + "\t" + .state + "\t" + .summary + "\t" + .status_at)
                    end
                  )
              ),
              ( $c | map(select(.name == null or .name == ""))
                | group_by(.client_id + "\u0000" + .app_version) | .[]
                | "row\tclient " + .[0].client_id + (if length > 1 then " ×" + (length|tostring) else "" end) + "\t" + .[0].app_version + "\t—"
              ),
              ( (.payload.disconnected // [])[] as $d
                | "discon\t" + $d.identity + "\t" + ($d.since_s | tostring)
              )' \
            | while IFS=$'\t' read -r kind a b c d; do
                case "$kind" in
                    row) printf '%-20s %-28s %s\n' "$a" "$b" "$c" ;;
                    nosess)
                        echo "  (declares no sessions — older frontend, or no daemon on that box)"
                        ;;
                    emptysess)
                        # Review blocker 2 (cut for rc9.8): `.sessions[]`
                        # over an empty (but non-null) array yields NO
                        # jq output at all, so a box that declared and
                        # has nothing to report printed nothing here —
                        # indistinguishable from before this feature
                        # existed, and the deleted comm-list.sh footer
                        # made that silence look intentional.
                        echo "  (no sessions)"
                        ;;
                    sess)
                        # a=handle b=state c=summary d=status_at. `status_at`
                        # is the honesty valve (decision 2): an hour-old
                        # stamp prints "1h ago" here exactly as a local row's
                        # own strip would, via the SAME `fmt_age` comm-list.sh
                        # uses (moved to comm-lib.sh so the two never drift).
                        # A declared but EMPTY state prints "[—] no state
                        # recorded", never degraded to "[idle]": on a
                        # declared row an empty state means the registry
                        # entry was pruned when the run ended (ADR 0043
                        # decision 35), not that the run is quietly idle.
                        local sline
                        if [ -n "$b" ]; then
                            sline="  @$a [$b]"
                        else
                            sline="  @$a [—] no state recorded"
                        fi
                        [ -n "$c" ] && sline="$sline $c"
                        if [ -n "$d" ]; then
                            local sat; sat="$(date -u -d "$d" +%s 2>/dev/null || echo 0)"
                            [ "$sat" -gt 0 ] && sline="$sline · $(fmt_age $((nows - sat)))"
                        fi
                        echo "$sline"
                        ;;
                    discon)
                        # a=identity (the box's declared fe@<host> name)
                        # b=since_s. Its own `fe@` line (the `^fe@` grep/
                        # awk extraction in comm-list.sh keys on exactly
                        # this), never grouped under an attached row's
                        # three columns — a disconnected box has no
                        # `active`/`idle` to report, and no session lines
                        # follow: they left with the connection.
                        echo "$a  (frontend not connected since $(fmt_age "$b"); its sessions cannot be listed)"
                        ;;
                esac
            done
    fi
    # Labeled by the RESOLVED endpoint itself -- never a "local"/"backend"
    # guess (Codex review: an explicit local unix endpoint and a remote
    # endpoint from an env fallback are indistinguishable from here). The
    # build cell also carries the daemon's own declared host (topology
    # plan §F step 1) alongside lane proto -- same 3-cell row shape as
    # every other line, no new column.
    printf '%-20s %-28s %s\n' "$ENDPOINT" "$daemon_version" "$daemon_build"

    # workspace.list -- capsule rows only, phase printed VERBATIM, no
    # derived verdict (Codex review, blocker: this script never challenged
    # the lane itself, so "matches" claimed proof it never had for an old
    # daemon, a stopped row, a missing phase, or a watchdog-terminal one --
    # "foreign" IS the verdict already). Unlike version.query, a daemon new
    # enough to have answered it has no legacy excuse for failing
    # workspace.list -- a failure here is always genuine, reported, exit 2.
    local wreq wresp werr
    wreq="$(jq -nc '{v:1, id:2, kind:"req", op:"workspace.list", payload:{}}')"
    wresp="$(sot_send "$wreq" workspace.list || true)"
    if [ -z "$wresp" ]; then
        echo "ERROR: the daemon did not answer workspace.list within ${SEND_TIMEOUT}s via $ENDPOINT." >&2
        exit 2
    fi
    werr="$(printf '%s' "$wresp" | jq -r '.payload.error // empty')"
    if [ -n "$werr" ]; then
        echo "ERROR: the daemon refused workspace.list: $werr [$(printf '%s' "$wresp" | jq -r '.payload.code // "?"')]" >&2
        exit 2
    fi
    printf '%s' "$wresp" | jq -r '
        (.payload.workspaces // [])
        | map(select(.runtime == "capsule"))
        | .[]
        | .slug + "\t" + (.phase // "unknown")
    ' | while IFS=$'\t' read -r slug phase; do
        printf '%-20s %s\n' "row $slug" "$phase"
    done

    # Installed comm scripts (ADR 0030 §8 decision 31, "Installed comm
    # scripts"): `install_comm` stamps $SOT_COMM_HOME/VERSION with the
    # repo commit (dirty-suffixed) at install time. This is a LOCAL read
    # regardless of --endpoint -- the scripts running this very command
    # are the ones being reported on, not whatever the remote daemon's own
    # box has. A missing file means unknown, not a claim nothing is
    # installed (the scripts running THIS command obviously are).
    local comm_version_file="${SOT_COMM_HOME:-$HOME/.sot-comm}/VERSION"
    local comm_version="unknown"
    [ -f "$comm_version_file" ] && comm_version="$(cat "$comm_version_file")"
    printf '%-20s %s\n' "comm scripts" "$comm_version"
}
