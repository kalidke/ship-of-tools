# Part of test-hub-files.sh, sourced by it: lock errors are never busy, one line hash, the home-guard case.
# ---- a lock error is never "busy"; one line hash (B1 fix-up 7) ------------

POLL_ERR=""
# comm-poll with its stdout and stderr apart: POLL_OUT is stdout alone.
poll_stdout() {
    POLL_OUT="$(cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" \
        "$BIN/comm-poll.sh" 2>"$WORK/poll.err")"
    POLL_RC=$?
    POLL_ERR="$(cat "$WORK/poll.err" 2>/dev/null)"
}

# S-A: only a held lock is "try again". A lock fault makes every shared lock
# fail while the writers' exclusive lock stays real (faulty below). comm-poll
# names the fault on stdout before the messages, exits 0 and reads unlocked:
# every line exactly once across two polls with a send between. The
# end-of-turn hook carries the warning on its mail block and blocks once for
# the fault alone, never again for the same one; a clean check clears the
# tick, so the fault blocks again. Nothing says busy. The warning's text is
# flock's own error line (`flock: 9: <strerror>`), never a table: exit 65
# covers EIO as well as EBADF.
FF="$WORK/ff"
# flock_stub CODE STRERROR — $FF/flock fails every shared lock with CODE and
# flock's own shape of error while $FF/on exists, and is the real flock
# otherwise.
flock_stub() {
    rm -rf "${FF:?}"; mkdir -p "$FF"; : > "$FF/on"
    printf '#!/bin/sh\ncase " $* " in *" -s "*) [ -e "%s/on" ] && { echo "flock: 9: %s" >&2; exit %s; } ;; esac\nexec %s "$@"\n' \
        "$FF" "$2" "$1" "$(command -v flock)" > "$FF/flock"
    chmod +x "$FF/flock"
}
# faulty KIND CMD... — CMD under the fault: KIND `dir` makes the peer's lock
# file a directory while CMD runs, so no reader can open it (a send between
# runs with the real file: a writer on this host fails on the same open, and
# says so); any other KIND runs CMD with $FF/flock first on the PATH.
faulty() {  # KIND CMD...
    local kind="$1" rc=0; shift
    [ "$kind" = dir ] || { PATH="$FF:$PATH" "$@"; return; }
    rm -f "${INBOX:?}/$PEER.lock"; mkdir "$INBOX/$PEER.lock"
    "$@" || rc=$?
    rmdir "$INBOX/$PEER.lock"
    return "$rc"
}
fault_ticks() { compgen -G "$SOT_COMM_HOME/state/lock-fault-*.tick" >/dev/null; }
lock_fault_case() {  # KIND WHY — KIND a flock exit code or `dir`; WHY what the warning's parentheses say
    local kind="$1" warn h shown="" said="" m
    warn="WARNING: the inbox lock for @$PEER failed ($2) — reading without it; a line may show twice, none is lost"
    [ "$kind" = dir ] || flock_stub "$kind" "${2#*: }"
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    rm -f "${SOT_COMM_HOME:?}"/state/mail-*.tick "${SOT_COMM_HOME:?}"/state/lock-fault-*.tick
    ln -sfn "$BIN" "$SOT_COMM_HOME/bin"
    run_send "@$PEER" "f$kind-one"; run_send "@$PEER" "f$kind-two"
    h="$(faulty "$kind" idle_hook 2>&1)"; said+="$h"$'\n'
    contains "$h" '"decision":"block"' && contains "$h" "$warn New sot-comm mail for @$PEER" \
        || { echo "  [$kind] the mail block did not carry the warning: $h"; return 1; }
    h="$(faulty "$kind" idle_hook 2>&1)"; said+="$h"$'\n'
    ! contains "$h" '"decision"' || { echo "  [$kind] blocked again for the same fault: $h"; return 1; }
    for m in poll1 poll2; do
        [ "$m" = poll1 ] || run_send "@$PEER" "f$kind-three"
        [ "$SEND_RC" -eq 0 ] || { echo "  [$kind] the send between failed: $SEND_OUT"; return 1; }
        faulty "$kind" poll_stdout
        said+="$POLL_OUT"$'\n'"$POLL_ERR"$'\n'; shown+="$POLL_OUT"$'\n'
        [ "$POLL_RC" -eq 0 ] && [ "${POLL_OUT%%$'\n'*}" = "$warn" ] \
            || { echo "  [$kind] $m rc $POLL_RC, stdout: $POLL_OUT"; return 1; }
    done
    for m in one two three; do
        [ "$(count_of "$shown" "f$kind-$m")" -eq 1 ] || { echo "  [$kind] f$kind-$m not shown exactly once: $shown"; return 1; }
    done
    h="$(faulty "$kind" idle_hook 2>&1)"; said+="$h"$'\n'
    ! contains "$h" '"decision"' || { echo "  [$kind] blocked again after the polls: $h"; return 1; }
    h="$(idle_hook 2>&1)"
    ! contains "$h" '"decision"' && ! fault_ticks || { echo "  [$kind] a clean check left the tick: $h"; return 1; }
    h="$(faulty "$kind" idle_hook 2>&1)"; said+="$h"$'\n'
    contains "$h" "{\"decision\":\"block\",\"reason\":\"$warn\"}" || { echo "  [$kind] the fault alone did not block once: $h"; return 1; }
    h="$(faulty "$kind" idle_hook 2>&1)"; said+="$h"$'\n'
    ! contains "$h" '"decision"' || { echo "  [$kind] the fault alone blocked twice: $h"; return 1; }
    ! contains "$said" busy && ! contains "$said" "being written" || { echo "  [$kind] said busy: $said"; return 1; }
    echo "  [$kind] poll stdout: $warn"
    return 0
}
case_a_lock_error_71_is_named_and_the_inbox_read_unlocked() { lock_fault_case 71 "71: No locks available"; }
case_a_lock_error_65_is_named_and_the_inbox_read_unlocked() { lock_fault_case 65 "65: Bad file descriptor"; }
case_a_lock_error_65_eio_is_named_as_flock_names_it() { lock_fault_case 65 "65: Input/output error"; }
case_a_lock_file_that_will_not_open_is_named() { lock_fault_case dir "cannot open its lock file"; }

# flock runs inside $(…) to catch its error text, and the shared lock still
# holds in the calling shell: the lock is on the open file description, which
# the caller's fd 9 keeps. Another process's exclusive try fails while the
# reader holds it and succeeds once it lets go.
case_a_shared_lock_taken_inside_a_subshell_holds_in_the_caller() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local out
    out="$(bash -c 'source "$1/comm-lib.sh"; sot_inbox_read_lock "$2" || exit 9
        flock -n -x "$3" true && echo free || echo held
        sot_inbox_read_unlock
        flock -n -x "$3" true && echo free || echo held' _ "$BIN" "$PEER" "$INBOX/$PEER.lock" 2>&1)"
    [ "$out" = $'held\nfree' ] || { echo "  want held then free, got: $out"; return 1; }
}

# The end-of-turn hook under a lasting fault (71) with no mail pending. The
# fault's once-block never fires inside a stop-hook continuation; it waits for
# the next turn end. On a MARKER turn it fires and leaves the row unstamped;
# its feedback comes back in the transcript (the real shape, the reason from
# the hook's own stdout), the model replies plainly, and that turn end, a
# continuation, passes and stamps from the marker. The fault fires nothing
# more. A session relaunched under the handle is told once too. A nudge that
# is not about mail, the missing-marker one, carries the warning first.
peer_row() { jq -c --arg n "$PEER" '.agents[$n] | [.state, .summary, .status_at]' "$SOT_COMM_HOME/registry.json"; }
peer_status() {
    ( cd "$WORK" && SOT_COMM_SELF_FILE="$WORK/self-peer.txt" SOT_COMM_TEST_HOST="$HOST_PIN" "$BIN/comm-status.sh" "$@" ) >/dev/null 2>&1
}
case_a_lock_fault_blocks_a_marker_turn_once_and_prefixes_every_nudge() {
    local warn blk h row0 tr
    warn="WARNING: the inbox lock for @$PEER failed (71: No locks available) — reading without it; a line may show twice, none is lost"
    blk="{\"decision\":\"block\",\"reason\":\"$warn\"}"
    flock_stub 71 "No locks available"
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    rm -f "${SOT_COMM_HOME:?}"/state/mail-*.tick "${SOT_COMM_HOME:?}"/state/lock-fault-*.tick
    ln -sfn "$BIN" "$SOT_COMM_HOME/bin"
    h="$(faulty 71 idle_hook 'all done.' true 2>&1)"
    [ -z "$h" ] && ! fault_ticks || { echo "  fired inside a stop-hook continuation: $h"; return 1; }
    row0="$(peer_row)"
    tr="$WORK/held.jsonl"; rm -f "${SOT_COMM_HOME:?}"/state/stop-feedback-*.jsonl
    { jq -nc '{type:"user",promptId:"p1",message:{role:"user",content:"go"}}'
      jq -nc --arg t $'SITREP: the marker turn\n\nDone.' '{type:"assistant",message:{content:[{type:"text",text:$t}]}}'; } > "$tr"
    h="$(faulty 71 idle_hook_over "$tr" 2>&1)"
    [ "$h" = "$blk" ] || { echo "  the marker turn did not block once with the warning: $h"; return 1; }
    [ "$(peer_row)" = "$row0" ] || { echo "  the blocked marker turn changed the row: $row0 -> $(peer_row)"; return 1; }
    { jq -nc --arg r "$(printf '%s' "$h" | jq -r '.reason')" \
          '{type:"user",isMeta:true,promptId:"p1",message:{role:"user",content:("Stop hook feedback:\n" + $r)}}'
      jq -nc '{type:"assistant",message:{content:[{type:"text",text:"all done."}]}}'; } >> "$tr"
    h="$(faulty 71 idle_hook_over "$tr" true 2>&1)"
    [ -z "$h" ] && [ "$(peer_row | jq -r '.[1]')" = "the marker turn" ] \
        || { echo "  the held turn's end did not pass and stamp from its marker: '$h' $(peer_row)"; return 1; }
    h="$(faulty 71 idle_hook 'SITREP: again' 2>&1)"
    [ -z "$h" ] || { echo "  fired again for the same fault: $h"; return 1; }
    h="$(HOOK_SESSION=relaunched faulty 71 idle_hook 2>&1)"
    [ "$h" = "$blk" ] || { echo "  a session relaunched under the handle was not told: $h"; return 1; }
    h="$(HOOK_SESSION=relaunched faulty 71 idle_hook 2>&1)"
    [ -z "$h" ] || { echo "  the relaunched session was told twice: $h"; return 1; }
    COMM_STATUS_ORIGIN=user peer_status prompt; peer_status blocked "which one?"
    h="$(faulty 71 idle_hook 'I will wait here.' 2>&1)"
    printf '%s' "$h" | jq -e --arg w "$warn Your row ends this turn as \`blocked\`" '.decision == "block" and (.reason | startswith($w))' >/dev/null \
        || { echo "  the missing-marker nudge did not carry the warning first: $h"; return 1; }
    return 0
}

# ...and a real held lock is still exit 75 and "being written", no warning.
case_a_held_lock_is_still_try_again() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    run_send "@$PEER" "held-one"
    start_holder 'exec sleep 60' || { echo "  holder never took the lock"; return 1; }
    SOT_INBOX_READ_WAIT_SECS=1 poll_stdout
    kill -9 "$HOLDER"; wait "$HOLDER" 2>/dev/null
    [ "$POLL_RC" -eq 75 ] && contains "$POLL_OUT" "the inbox for @$PEER is being written" && ! contains "$POLL_OUT$POLL_ERR" WARNING \
        || { echo "  rc $POLL_RC: $POLL_OUT $POLL_ERR"; return 1; }
    return 0
}

# S-B: the file's line and the reader's copy hash alike. A newline-terminated
# line holding a NUL (bash drops it from the copy), one ending in CR and an
# empty one, each the last line of a batch: shown once, and a second poll
# shows nothing and says nothing was cut back.
case_a_nul_a_cr_or_an_empty_last_line_is_shown_once() {
    setup_rows || { echo "  setup: could not join both rows"; return 1; }
    local f="$INBOX/$PEER.jsonl" kind
    for kind in nul cr empty; do
        rm -f "${SOT_COMM_HOME:?}/read/$PEER.cursor"
        case "$kind" in
            nul)   printf '{"from":"a","to":"t-peer","msg":"nul-x\000y","ts":"1"}\n' > "$f" ;;
            cr)    printf '{"from":"a","to":"t-peer","msg":"cr-line","ts":"1"}\r\n' > "$f" ;;
            empty) printf '{"from":"a","to":"t-peer","msg":"empty-before","ts":"1"}\n\n' > "$f" ;;
        esac
        poll_stdout
        [ "$(count_of "$POLL_OUT" "$kind-")" -eq 1 ] || { echo "  [$kind] poll 1 did not show it once: $POLL_OUT"; return 1; }
        poll_stdout
        [ "$POLL_OUT" = "No new messages." ] && ! contains "$POLL_ERR" "was cut back" \
            || { echo "  [$kind] poll 2: $POLL_OUT $POLL_ERR"; return 1; }
    done
    return 0
}

# The home guard every suite sources (lib-home-guard.sh), pointed at a temp
# HOME T and never at the real one, so the live home it records is
# T/.sot-comm. T/.sot-comm and a path under it are FATAL with exit 2, first
# while it does not exist (its literal path) and then through a symlink to it
# (its physical path); so are an inherited SOT_COMM_HOME, an empty name, and
# the account's own home, whatever HOME says. Sourcing records the real
# account home (read-only: only its path is compared), which is getent's
# answer where getent exists; a stubbed `id` names root's home instead, and an
# `id` name outside [A-Za-z0-9._-], or not starting [A-Za-z_], is never evaluated and records none.
# T/other passes, and sourcing drops the host's comm identity and daemon route.
guard_run() {  # T COMM_HOME — the guard's verdict on COMM_HOME under HOME=T
    GUARD_RC=0
    GUARD_OUT="$(HOME="$1" SOT_COMM_HOME="$1/inherited" bash -c '. "$1"; guard_refuse_live_home "$2"; echo passed' \
        _ "$SCRIPT_DIR/lib-home-guard.sh" "$2" 2>&1)" || GUARD_RC=$?
}
case_the_home_guard_refuses_a_live_comm_home() {
    local t="$WORK/guard-home" h
    mkdir -p "$t/other"
    for h in "$t/.sot-comm" "$t/.sot-comm/x" MKDIR "$t/.sot-comm" "$t/.sot-comm/x" "$t/link/x" "$t/inherited" ""; do
        [ "$h" != MKDIR ] || { mkdir -p "$t/.sot-comm"; ln -sfn "$t/.sot-comm" "$t/link"; continue; }
        guard_run "$t" "$h"
        [ "$GUARD_RC" -eq 2 ] && contains "$GUARD_OUT" FATAL && ! contains "$GUARD_OUT" passed \
            || { echo "  '$h' was not refused: rc $GUARD_RC, $GUARD_OUT"; return 1; }
    done
    guard_run "$t" "$t/other"
    [ "$GUARD_RC" -eq 0 ] && [ "$GUARD_OUT" = passed ] || { echo "  '$t/other' was refused: rc $GUARD_RC, $GUARD_OUT"; return 1; }
    # The account's home as the system records it counts too, whatever HOME
    # says: getpwnam's answer, which getent gives too where it exists.
    h="$(HOME="$t" bash -c '. "$1"; printf %s "$_GUARD_ACCT"' _ "$SCRIPT_DIR/lib-home-guard.sh")"
    [ -n "$h" ] || { echo "  sourcing recorded no account home"; return 1; }
    if command -v getent >/dev/null 2>&1; then
        [ "$h" = "$(getent passwd "$(id -un)" | cut -d: -f6)" ] \
            || { echo "  the account home '$h' is not getent's"; return 1; }
    fi
    mkdir -p "$t/id-bin"
    printf '#!/bin/sh\necho root\n' > "$t/id-bin/id"; chmod +x "$t/id-bin/id"
    local root=~root
    for h in "$root/.sot-comm/x" "$t/other"; do
        GUARD_RC=0
        GUARD_OUT="$(PATH="$t/id-bin:$PATH" HOME="$t" bash -c '. "$1"; guard_refuse_live_home "$2"; echo passed' \
            _ "$SCRIPT_DIR/lib-home-guard.sh" "$h" 2>&1)" || GUARD_RC=$?
        case "$h" in
            "$root"/*) [ "$GUARD_RC" -eq 2 ] && contains "$GUARD_OUT" FATAL \
                || { echo "  the account's own home was not refused: rc $GUARD_RC, $GUARD_OUT"; return 1; } ;;
            *) [ "$GUARD_RC" -eq 0 ] && [ "$GUARD_OUT" = passed ] \
                || { echo "  '$h' was refused under the stubbed account: rc $GUARD_RC, $GUARD_OUT"; return 1; } ;;
        esac
    done
    printf '#!/bin/sh\necho "x\\$(touch %s/evaluated)"\n' "$t" > "$t/id-bin/id"
    h="$(PATH="$t/id-bin:$PATH" HOME="$t" SOT_COMM_HOME= bash -c '. "$1"; printf %s "$_GUARD_ACCT|${#_GUARD_LIVE[@]}"' \
        _ "$SCRIPT_DIR/lib-home-guard.sh")"
    [ "$h" = "|1" ] && [ ! -e "$t/evaluated" ] || { echo "  an invalid name recorded an account home: $h"; return 1; }
    # a name that starts with a digit, or is - or all digits, is never `~name`
    for n in - 0 12; do
        printf '#!/bin/sh\necho %s\n' "$n" > "$t/id-bin/id"
        h="$(PATH="$t/id-bin:$PATH" HOME="$t" SOT_COMM_HOME= bash -c '. "$1"; printf %s "$_GUARD_ACCT|${#_GUARD_LIVE[@]}"' \
            _ "$SCRIPT_DIR/lib-home-guard.sh")"
        [ "$h" = "|1" ] || { echo "  the name '$n' recorded an account home: $h"; return 1; }
    done
    h="$(SOT_COMM_NAME=n SOT_COMM_SELF_FILE=f SOT_WORKSPACE_ID=w SOT_SOCKET=s SOT_FE_ENDPOINT=e \
        SOT_RELAY_ENDPOINT=e SOT_SPAWN_ENDPOINT=e SOT_ANY_ENDPOINT=e HOME="$t" bash -c '. "$1"
        echo "${SOT_COMM_HOME-}${SOT_COMM_NAME-}${SOT_COMM_SELF_FILE-}${SOT_WORKSPACE_ID-}${SOT_SOCKET-}"; env | grep "_ENDPOINT="' \
        _ "$SCRIPT_DIR/lib-home-guard.sh")"
    [ -z "$h" ] || { echo "  sourcing left the comm identity or daemon route set: $h"; return 1; }
    return 0
}
