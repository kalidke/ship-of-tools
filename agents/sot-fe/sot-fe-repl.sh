# sot-fe's REPL verbs: run a file, eval, interrupt and status of a workspace's persistent REPL.
# Sourced by sot-fe; defines functions only.

# Send a repl.execute request (ADR 0033) and print the collected result. $1 is
# the `input` JSON object ({kind:"run_file",path} | {kind:"eval",code}); $2 is
# the workspace. Unlike send_fe_command this is a real request/response op — it
# blocks for the run and the daemon returns the collected output on this op's
# res line (the interleaved repl.frame evts on the connection are skipped by
# sot_send's op-grep). Exits 2 on a non-ok outcome.
send_repl_execute() {
    local input_json="$1" ws="$2"
    if ! ENDPOINT="$(resolve_endpoint)"; then
        echo "ERROR: could not find the sotd daemon. Set --endpoint unix:/path, ssh:target[/host], or (Windows) pipe:name (or \$SOT_FE_ENDPOINT)." >&2
        exit 1
    fi
    ws="$(_ws_id_or_passthrough "$ws")"
    local secs="${TIMEOUT:-120}"
    local ms=$(( secs * 1000 ))
    # Wait a little past the backend budget so the daemon's timeout result (not
    # our socket read) is what lands.
    export SEND_TIMEOUT=$(( secs + 10 ))
    local payload req resp
    payload="$(jq -nc --arg w "$ws" --argjson in "$input_json" --argjson t "$ms" \
        '{workspace_id:$w, input:$in, timeout_ms:$t}')"
    # MSYS2 argv-conversion guard (comm-lib.sh's sot_jq_rawfile): ORIGIN is
    # free-text from --origin (a user-set drawer label) and must never
    # reach jq via --arg.
    if [ -n "$ORIGIN" ]; then
        _origin_file="$(sot_jq_rawfile "$ORIGIN")" || exit 1
        payload="$(printf '%s' "$payload" | jq -c --rawfile o "$_origin_file" '. + {origin:$o}')"
        rm -f "${_origin_file:?}"
    fi
    req="$(jq -nc --argjson p "$payload" '{v:1, id:2, kind:"req", op:"repl.execute", payload:$p}')"
    resp="$(sot_send "$req" repl.execute || true)"
    if [ -z "$resp" ]; then
        echo "ERROR: no response from repl.execute within ${SEND_TIMEOUT}s via $ENDPOINT. The eval is ENQUEUED before any reply is written, so a missing response — whether a TIMEOUT (first REPL use per workspace precompiles its deps, minutes) OR a connection dropped AFTER enqueue — does NOT mean nothing ran: your eval MAY STILL BE RUNNING. Do NOT re-run blindly — check the drawer/output first; re-running would execute it TWICE (double side-effects: files written twice, state mutated twice)." >&2
        exit 1
    fi
    printf '%s' "$resp" | jq -r '
        .payload as $p |
        if ($p.error != null) and (($p.outcome // "") == "") then
            "ERROR: \($p.error) [\($p.code // "?")]"
        else
            "outcome: \($p.outcome)   run_id: \($p.run_id)   elapsed: \($p.elapsed_ms)ms"
            + (if ($p.outcome == "timeout") then "\n⚠ TIMEOUT is NOT failed or cancelled: run_id \($p.run_id) exceeded the reply budget but is STILL RUNNING in the REPL (first REPL use per workspace precompiles its package deps — can take minutes). Do NOT re-run — check run_id \($p.run_id) / the drawer for its output; re-running would execute it a SECOND time (double side-effects: files written twice, state mutated twice)." else "" end)
            + (if $p.truncated then "   [output truncated]" else "" end)
            + (if ($p.project_dir // "") != "" then "\nproject: \($p.project_dir) (\($p.project_source // "?"))" else "" end)
            + (if ($p.stdout // "") != "" then "\n--- stdout ---\n\($p.stdout)" else "" end)
            + (if ($p.stderr // "") != "" then "\n--- stderr ---\n\($p.stderr)" else "" end)
            + (if (($p.values // []) | length) > 0 then "\n--- value ---\n" + ($p.values | map(.text) | join("\n")) else "" end)
            + (if $p.error != null then "\n--- error ---\n\($p.error.message)" + (($p.error.stacktrace // []) | map("\n  at \(.fn) (\(.file):\(.line))") | join("")) else "" end)
            + (if (($p.figures // []) | length) > 0 then "\n--- figures ---\n" + ($p.figures | join("\n")) else "" end)
        end
    '
    local outcome
    outcome="$(printf '%s' "$resp" | jq -r '.payload.outcome // "error"')"
    [ "$outcome" = "ok" ] || exit 2
}

# Send a repl.run_file request with fresh:true — RESTART the workspace's
# persistent REPL into <path>'s project (daemon: restart_with_project), then
# include the file in the fresh kernel. This is the kernel-bounce path (clears a
# stale in-memory package a struct/field change can't hot-reload; Revise is not
# wired in). Unlike repl.execute this is FIRE-AND-FORGET: the daemon acks once
# the restart is spawned (with project_dir/source); the include's frames stream
# as repl.frame evts to the FE drawer and are NOT collected here. $1=path
# $2=workspace. Exits 2 on a failed ack.
send_repl_run_file() {
    local file="$1" ws="$2"
    if ! ENDPOINT="$(resolve_endpoint)"; then
        echo "ERROR: could not find the sotd daemon. Set --endpoint unix:/path, ssh:target[/host], or (Windows) pipe:name (or \$SOT_FE_ENDPOINT)." >&2
        exit 1
    fi
    ws="$(_ws_id_or_passthrough "$ws")"
    # The restart spawns a fresh julia child before the ack; give it more room
    # than sot_send's 6s default (honour --timeout if the caller set it).
    export SEND_TIMEOUT="${TIMEOUT:-30}"
    local payload req resp
    # ReplRunFileReq: eval_id(u64)+path required, fresh:true, workspace_id routes
    # to the target REPL. eval_id only correlates streamed frames we don't
    # collect, so a fixed value is fine.
    # MSYS2 argv-conversion guard (comm-lib.sh's sot_jq_rawfile): $file is
    # an absolute backend path (the caller's own doc above: "agents
    # naturally hold an ABSOLUTE path") and must never reach jq via --arg.
    local _run_file_path_file; _run_file_path_file="$(sot_jq_rawfile "$file")" || exit 1
    payload="$(jq -nc --rawfile p "$_run_file_path_file" --arg w "$ws" \
        '{eval_id:1, path:$p, fresh:true, workspace_id:$w}')"
    rm -f "${_run_file_path_file:?}"
    req="$(jq -nc --argjson p "$payload" '{v:1, id:2, kind:"req", op:"repl.run_file", payload:$p}')"
    resp="$(sot_send "$req" repl.run_file || true)"
    if [ -z "$resp" ]; then
        echo "ERROR: no ack from repl.run_file within ${SEND_TIMEOUT}s via $ENDPOINT. The request is ENQUEUED before any ack is written, so a missing ack (timeout OR a connection dropped after enqueue) does NOT mean nothing happened — the fresh-restart may have ALREADY bounced the workspace REPL into <path>'s project and started the include. Check the drawer for a fresh REPL before re-running, or you'll re-bounce it (and re-run the file)." >&2
        exit 1
    fi
    # Ack is either {error,code} or {project_dir,project_source,accepted:true}.
    printf '%s' "$resp" | jq -r '
        .payload as $p |
        if ($p.error != null) then
            "ERROR: \($p.error) [\($p.code // "?")]"
        else
            "fresh-run accepted: REPL restarted into \($p.project_dir // "?") (\($p.project_source // "?"))"
            + "\nnote: kernel bounced; \($p.path // "the file") is being included in the fresh process. Output streams to the FE drawer, not collected here."
        end
    '
    local err
    err="$(printf '%s' "$resp" | jq -r '.payload.error // empty')"
    [ -z "$err" ] || exit 2
}

# Send a repl.interrupt request — stop the RUNNING eval, keep the kernel.
# $1=workspace (id, label or slug). The op is deliberately readable while the
# REPL is busy (that is its whole point), so this is the first thing to try on a
# wedged run: unlike --fresh it preserves the child's compiled packages, and
# unlike killing the pid it goes through the supervisor. Exits 2 on a
# daemon-reported error.
send_repl_interrupt() {
    local ws="$1" row state ws_id
    _need_endpoint
    # Export the caller's budget BEFORE the workspace.list pre-flight, so
    # --timeout governs BOTH round-trips. Ordered the other way, the pre-flight
    # ran on _ws_row's 15s floor and a `--timeout 120` against a daemon slow
    # precisely because of the runaway eval died before the interrupt was sent.
    export SEND_TIMEOUT="${TIMEOUT:-30}"
    row="$(_ws_row "$ws")"
    if [ -z "$row" ]; then
        echo "ERROR: no workspace matched '$ws' (tried workspace_id, label and slug). Run \`sot-fe repl status\` to see what exists." >&2
        exit 1
    fi
    ws_id="$(printf '%s' "$row" | jq -r '.workspace_id')"
    if [ "$(printf '%s' "$row" | jq 'has("repl_state")')" != "true" ]; then
        # Pre-repl_state daemon (version skew): we cannot pre-check the
        # lifecycle, but repl.interrupt itself predates the field — send, and
        # say why the guard was skipped rather than silently guessing.
        echo "note: this daemon predates repl_state — cannot pre-check the REPL lifecycle; sending the interrupt anyway." >&2
        state="unknown"
    else
        state="$(printf '%s' "$row" | jq -r '.repl_state')"
    fi
    # Refuse rather than SPAWN. Every submission path calls ensure_supervisor(),
    # which (re)starts a julia child whenever none is live — so an interrupt
    # aimed at a not_started OR dead REPL would pay a full precompile purely to
    # interrupt nothing (and the fresh shim would then answer interrupted:false
    # anyway). `dead` needs no cleanup: the next eval respawns it.
    case "$state" in
        not_started|dead)
            echo "nothing to interrupt: $ws has no live REPL child (repl_state=$state)." >&2
            echo "note: not sending — an interrupt here would SPAWN a kernel just to interrupt nothing." >&2
            [ "$state" = dead ] && echo "note: a dead REPL needs no cleanup — the next eval respawns it (and the respawn revokes the old child's announced browser ports)." >&2
            exit 3 ;;
        starting)
            # A booting child reads stdin only once its serve loop is up, so an
            # interrupt sent now stalls past any reasonable budget and then
            # reads as a transport failure — for a HEALTHY, documented state
            # (first boot precompiles for minutes). Worse, if an eval is queued
            # behind the boot, the late-landing interrupt kills that first
            # legitimate run. Refuse with the real story.
            echo "not sending: $ws's REPL is starting (first boot precompiles — can take MINUTES; the wait is not a wedge)." >&2
            echo "note: an interrupt sent now would sit unread until the boot finishes and then kill the first queued eval. Wait for repl_state=ready; if you truly want to abandon the boot, \`repl run --fresh\` replaces it." >&2
            exit 3 ;;
    esac
    local payload req resp
    payload="$(jq -nc --arg w "$ws_id" '{workspace_id:$w}')"
    req="$(jq -nc --argjson p "$payload" '{v:1, id:2, kind:"req", op:"repl.interrupt", payload:$p}')"
    resp="$(sot_send "$req" repl.interrupt || true)"
    if [ -z "$resp" ]; then
        # Say what silence means here, because it does NOT mean "not interrupted":
        # the request is enqueued before any reply is written. Status can NOT
        # confirm delivery (the wire has no busy/idle bit) — the only positive
        # confirmation is a reply, so re-send (harmless) rather than bounce.
        echo "ERROR: no response to repl.interrupt within ${SEND_TIMEOUT}s via $ENDPOINT. That does NOT mean the interrupt failed — it may have landed and the reply was lost. Re-sending an interrupt is harmless; bouncing the kernel (--fresh) is the escalation if repeated interrupts change nothing (a non-yielding compute-bound eval never takes one)." >&2
        exit 1
    fi
    # The shim answers {interrupted:true} or {interrupted:false, note} — that
    # one bit is the difference between "I cancelled your run" and "there was
    # nothing to cancel", and an escalation ladder keys on it. Branch on it and
    # give each outcome its own exit code; treat a reply with NEITHER field as
    # the garbage it is instead of defaulting to success.
    local err interrupted
    err="$(printf '%s' "$resp" | jq -r '.payload.error // empty')"
    if [ -n "$err" ]; then
        printf 'ERROR: %s [%s]\n' "$err" "$(printf '%s' "$resp" | jq -r '.payload.code // "?"')" >&2
        exit 2
    fi
    # NOT `.payload.interrupted // empty`: jq's // treats false as absent, so
    # the legitimate {interrupted:false} reply would read as garbage.
    interrupted="$(printf '%s' "$resp" | jq -r '.payload | if has("interrupted") then (.interrupted | tostring) else "missing" end')"
    case "$interrupted" in
        true)
            echo "eval interrupted — the running eval got an InterruptException; the kernel is KEPT (compiled packages intact)."
            echo "note: the exception lands at the eval's next yield point — a non-yielding compute-bound eval may still be running; if repeated interrupts change nothing, escalate to --fresh."
            ;;
        false)
            echo "nothing to interrupt: $(printf '%s' "$resp" | jq -r '.payload.note // "no eval in progress"') (the kernel is fine)."
            exit 3
            ;;
        *)
            echo "ERROR: unexpected repl.interrupt reply (no interrupted flag): $resp" >&2
            exit 2
            ;;
    esac
}

# Report the daemon's own view of a workspace's REPL (workspace.list →
# repl_state). $1=workspace, or empty for every workspace with a started REPL.
#
# This exists because the alternative is ps/ss/proc archaeology to answer
# questions the daemon already knows the answer to. It deliberately reports
# "cannot reach the daemon" as its own outcome (exit 1) rather than folding it
# into "no REPL": those two look identical from a failing CLI and point at
# opposite fixes — one is your endpoint, the other is the kernel.
send_repl_status() {
    local want="$1"
    _need_endpoint
    export SEND_TIMEOUT="${TIMEOUT:-15}"
    local req resp
    req="$(jq -nc '{v:1, id:2, kind:"req", op:"workspace.list", payload:{}}')"
    resp="$(sot_send "$req" workspace.list || true)"
    if [ -z "$resp" ]; then
        echo "ERROR: the daemon did not answer workspace.list within ${SEND_TIMEOUT}s via $ENDPOINT. Again: TRANSPORT, not the REPL — the daemon is unreachable or wedged, so nothing can be concluded about any kernel." >&2
        exit 1
    fi
    # Same rejected-request guard as _ws_row: a well-formed {error, code} res
    # passes the empty check and must not fall into the iteration below.
    local rerr
    rerr="$(printf '%s' "$resp" | jq -r '.payload.error // empty' 2>/dev/null || true)"
    if [ -n "$rerr" ]; then
        echo "ERROR: the daemon refused workspace.list: $rerr [$(printf '%s' "$resp" | jq -r '.payload.code // "?"')] — likely a token/auth problem (\$SOT_TOKEN / ~/.config/sot/token), not a REPL problem." >&2
        exit 1
    fi
    # $want is a workspace_id/label/slug identifier -- id-shaped, same
    # allowlisted category as $WS/$ws_id elsewhere in this file.
    if [ -n "$want" ]; then
        local found
        found="$(printf '%s' "$resp" | jq -r --arg w "$want" \
            '[(.payload.workspaces // [])[] | select(.workspace_id == $w or .label == $w or .slug == $w)] | length')"
        if [ "$found" = "0" ]; then
            echo "ERROR: no workspace matched '$want' (tried workspace_id, label and slug). Run \`sot-fe repl status\` with no argument to see what exists." >&2
            exit 1
        fi
    fi
    # Deliberately NOT printed: kernel_running. That field is the lazily-built
    # INTROSPECTION Kernel handle (workspaces.rs kernel_built()), unrelated to
    # the REPL child and stale by design — a workspace can read repl:dead with
    # kernel_running:true and vice versa. Printing it here steered readers
    # toward exactly the wrong escalation. `root:` is the WORKSPACE root, not
    # necessarily the child's active project (see the header note).
    #
    # A row with NO repl_state key is a pre-repl_state daemon (version skew,
    # the field is #[serde(default)] for rollout) — say "unknown" out loud
    # instead of collapsing to not_started, which turned running kernels into
    # a confident "no workspace has a started REPL yet".
    printf '%s' "$resp" | jq -r --arg w "$want" '
        (.payload.workspaces // [])
        | map(select(
            if $w == "" then (has("repl_state") | not) or (.repl_state != "not_started")
            else (.workspace_id == $w or .label == $w or .slug == $w) end))
        | if length == 0 then
            "no workspace has a started REPL yet (every repl_state is not_started)."
          else
            map(
              "workspace: \(.label)   repl: \(if has("repl_state") then .repl_state else "unknown" end)"
              + "\n  id: \(.workspace_id)   root: \(.project_root)"
              + "\n  " + (
                  if (has("repl_state") | not) then "this daemon predates repl_state — lifecycle unknown; upgrade the daemon for real answers."
                  elif .repl_state == "ready" then "alive and accepting work (the wire has no busy/idle bit — a rejected next eval with outcome:busy means a run is still in flight)."
                  elif .repl_state == "starting" then "spawning or precompiling — a first boot can take MINUTES; do not re-send, and do not read the wait as wedged."
                  elif .repl_state == "dead" then "the child exited; the NEXT eval respawns it, and that respawn also revokes the old child'"'"'s announced browser ports. Nothing to clean up by hand."
                  else "no REPL child yet — the first eval spawns one (expect precompilation)."
                  end)
            ) | join("\n")
          end
    '
}
