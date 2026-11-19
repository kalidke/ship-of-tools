#!/usr/bin/env bash
# installer-wrapper.sh -- the rendered all-in-one sot-launch (render_sot_launch) under the sandboxed PATH: the probe before
# every spawn, the crash-loop rollback, the exit-75 respawn, the service operations by unit ownership, the re-exec after
# an apply, an armed pointer the installed sotd refuses (no stop, no restart, one sot-apply to drop it), and the window's
# log file (sot_window_log): the trigger as a truth table, what lands in the file, what it keeps, that it fails open, that
# a start that is not an icon launch is left alone, one rotation across the wrapper's re-exec, and the wrapper that dials
# the plan, an earlier release's text included, which reaches the log through the checkout's launch-sot.sh. The mechanics
# run with the trigger forced (force_log), so they run anywhere. The real trigger, a wrapper whose parent is process 1
# and whose environment names the app, runs where `unshare -Urpf` is allowed (an unprivileged user and process
# namespace: its first process is process 1); where the kernel refuses it those cases print SKIP, and the real trigger's
# end to end proof is the macOS-only test in rust/backend/src/install/unix_tests.rs.
# Every case runs under its own HOME, and the stand-in sotd names a state folder under the case's own folder, so no run
# writes into this account's state folder.
#
# Run: scripts/tests/installer-wrapper.sh

set -euo pipefail

# shellcheck source=installer-support.sh
. "$(dirname "$0")/installer-support.sh"

# ---------------------------------------------------------------------------
# Wrapper fixture: the rendered sot-launch under the sandboxed PATH. Event log
# lines: probe (the bridge), spawn (sot), rollback / apply (sot-apply), hop (a re-exec'd
# wrapper), plus the systemctl argv.
# <dir> <sot exit codes> <owned 1|0> <pending 1|0> <apply: rollback-only|consume>
mk_wrapper() {
    local d="$1" codes="$2" owned="$3" pending="$4" apply="${5:-log}" prefix="$1/prefix" lib
    lib="$prefix/repo/current/scripts/lib"
    # The comm home exists, so the wrapper's comm refresh (a julia call, or a warning where julia is not on PATH) does
    # nothing and says nothing: no case depends on the box's julia or comm install.
    mkdir -p "$d/stubs" "$d/home/.local/bin" "$d/home/.config/systemd/user" "$prefix/bin" "$prefix/updates" "$lib" "$d/home/.sot-comm/bin"
    mk_stubs "$d/stubs"
    cat > "$d/stubs/systemctl" <<'SC'
#!/bin/sh
printf 'systemctl %s\n' "$*" >> "$STUB_LOG"
exit 0
SC
    chmod +x "$d/stubs/systemctl"
    # The real pkill, if the box has one, so a wrapper that ever pattern-kills again kills the decoy of standin_start.
    ! command -v pkill >/dev/null 2>&1 || ln -sf "$(command -v pkill)" "$d/stubs/pkill"
    [ "$owned" = 1 ] || rm -f "${d:?}/stubs/systemctl"
    cp "$LIB" "$lib/sot-daemon.sh"
    printf '%s\n' "$codes" > "$d/codes"
    : > "$d/log"
    printf '%s\n' "$d/state/sotd.log" > "$d/logpath"
    python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$d/sot.sock"
    cat > "$prefix/bin/sotd" <<SD
#!/bin/sh
case "\$1 \$2 \$3" in
    "stdio-bridge --endpoint unix:"*) echo probe >> "\$STUB_LOG"; exit 0 ;;
esac
# The daemon's log, under the case's own folder (the file logpath holds, so a case can change it).
[ "\$1" = log-path ] && { cat "$d/logpath"; exit 0; }
[ "\$1 \$2" = "update pointer" ] && exit "\$(cat "$d/pointer-exit" 2>/dev/null || echo 0)"
[ "\$1" = session-socket-path ] && echo "$d/sot.sock"
exit 0
SD
    cat > "$prefix/bin/sot" <<'SOT'
#!/bin/sh
echo spawn >> "$STUB_LOG"
echo "window out $(grep -c '^spawn$' "$STUB_LOG")"
echo "window err $(grep -c '^spawn$' "$STUB_LOG")" >&2
d="$(dirname "$0")/../.."
code="$(head -1 "$d/codes")"; sed -i.bak 1d "$d/codes"
exit "${code:-0}"
SOT
    case "$apply" in
        consume)
            cat > "$prefix/bin/sot-apply" <<SA
#!/bin/sh
echo apply >> "\$STUB_LOG"
rm -f "${prefix:?}/updates/pending-testtarget.json"
printf '#!/bin/sh\necho hop >> "\$STUB_LOG"\n' > "$d/home/.local/bin/sot-launch.new"
chmod +x "$d/home/.local/bin/sot-launch.new"
mv "$d/home/.local/bin/sot-launch.new" "$d/home/.local/bin/sot-launch"
SA
            ;;
        consume-keep)
            # Consumes the pointer, says something, and leaves the wrapper: the real wrapper re-execs itself.
            cat > "$prefix/bin/sot-apply" <<SA
#!/bin/sh
echo apply >> "\$STUB_LOG"
echo apply-said >&2
rm -f "${prefix:?}/updates/pending-testtarget.json"
SA
            ;;
        *)
            cat > "$prefix/bin/sot-apply" <<'SA'
#!/bin/sh
case "$*" in *--rollback*) echo rollback >> "$STUB_LOG" ;; *--drop-only*) echo drop >> "$STUB_LOG" ;; *) echo apply >> "$STUB_LOG" ;; esac
SA
            ;;
    esac
    chmod +x "$prefix/bin/sotd" "$prefix/bin/sot" "$prefix/bin/sot-apply"
    if [ "$owned" = 1 ]; then
        printf '{"service": "systemd"}\n' > "$prefix/install.json"
        render_sotd_unit "$prefix" "$(dirname "$0")/../../deploy/sotd.service" "$d/home/.config/systemd/user/sotd.service"
    fi
    [ "$pending" != 1 ] || printf '{}\n' > "$prefix/updates/pending-testtarget.json"
    : > "$prefix/updates/just-applied-testtarget"
    render_sot_launch "$prefix" testtarget "$d/home/.local/bin/sot-launch"
}
# A stand-in daemon for <dir>: a copy of bash named sotd, whose arguments name the socket the wrapper asks about (what
# sot_daemon_pid finds), and a decoy that only names <prefix>/bin/sotd in its command line. Sets SA_PID and SA_DECOY.
standin_start() {  # <dir>
    local d="$1"
    mkdir -p "$d/standin"; cp "$(type -P bash)" "$d/standin/sotd"
    "$d/standin/sotd" -c 'while :; do sleep 1; done' standin --socket "$d/sot.sock" &
    SA_PID=$!
    ( exec -a "tail -f $d/prefix/bin/sotd" sleep 300 ) &
    SA_DECOY=$!
}
# "stopped" when the stand-in daemon is gone (a zombie counts as gone), "kept" for the decoy while it lives; then both are ended.
standin_end() {  # <dir>: prints "<daemon> <decoy>"
    local dstate="stopped" ostate="gone" st
    st="$(ps -o stat= -p "$SA_PID" 2>/dev/null || true)"
    case "$st" in ''|Z*) ;; *) dstate="running" ;; esac
    ! kill -0 "$SA_DECOY" 2>/dev/null || ostate="alive"
    kill "$SA_PID" "$SA_DECOY" 2>/dev/null || true
    wait "$SA_PID" "$SA_DECOY" 2>/dev/null || true
    echo "$dstate decoy-$ostate"
}
run_wrapper() {  # <dir>: sets WR_RC
    local d="$1"
    WR_RC=0
    ( HOME="$d/home" SOT_COMM_HOME="$d/home/.sot-comm" PATH="$d/stubs:$TOOLS" STUB_LOG="$d/log" "$d/home/.local/bin/sot-launch" ) >/dev/null 2>&1 || WR_RC=$?
}

# ---------------------------------------------------------------------------
case_start "wrapper_ensures_before_every_spawn"
d="$WORK/w-ensure"; mk_wrapper "$d" "$(printf '1\n1\n0')" 0 0
run_wrapper "$d"
check "the wrapper probes the socket before every spawn, rolls back after two fast crashes" \
    "0 probe spawn probe spawn rollback probe spawn" "$WR_RC $(events "$d")"

# ---------------------------------------------------------------------------
case_start "wrapper_service_ops_follow_ownership"
d="$WORK/w-own-apply"; mk_wrapper "$d" "0" 1 1
standin_start "$d"; run_wrapper "$d"
check "an owned unit: apply is a try-restart, no sot-apply, no pid stop" \
    "1 0 running decoy-alive" "$(grep -c -- '--user try-restart sotd.service' "$d/log" || true) $(grep -c '^apply$' "$d/log" || true) $(standin_end "$d")"
d="$WORK/w-own-rb"; mk_wrapper "$d" "$(printf '1\n1\n0')" 1 0
standin_start "$d"; run_wrapper "$d"
check "an owned unit: rollback stops it through systemd, never by a pid" \
    "1 running decoy-alive" "$(grep -c -- '--user stop sotd.service' "$d/log" || true) $(standin_end "$d")"
d="$WORK/w-for-apply"; mk_wrapper "$d" "0" 0 1
printf '#!/bin/sh\nexit 0\n' > "$d/stubs/systemctl"; chmod +x "$d/stubs/systemctl"
standin_start "$d"; run_wrapper "$d"
check "a foreign unit: apply stops the daemon by its pid, leaves a process that only names its path, and runs sot-apply, no try-restart" \
    "0 1 stopped decoy-alive" "$(grep -c 'try-restart' "$d/log" || true) $(grep -c '^apply$' "$d/log" || true) $(standin_end "$d")"
d="$WORK/w-for-rb"; mk_wrapper "$d" "$(printf '1\n1\n0')" 0 0
printf '#!/bin/sh\nprintf "systemctl %%s\\n" "$*" >> "$STUB_LOG"\nexit 0\n' > "$d/stubs/systemctl"; chmod +x "$d/stubs/systemctl"
standin_start "$d"; run_wrapper "$d"
check "a foreign unit: rollback stops the daemon by its pid and leaves the decoy, no systemd stop" \
    "0 stopped decoy-alive" "$(grep -c 'sotd.service' "$d/log" || true) $(standin_end "$d")"
# A pointer the installed sotd refuses (exit 3) costs no restart: sot-apply runs once to drop it and nothing stops or
# restarts the daemon, and sot-apply runs in its drop-only mode: a pointer that fits and was armed in between waits for the
# next start, when the daemon is stopped as usual. Exit 1, an older sotd's answer to the verb, leaves the restart in charge.
refused() {  # <name> <owned> <sotd exit>: the log's restart/stop, full-apply and drop-only counts, and whether the daemon was left running
    local d="$WORK/w-$1"; mk_wrapper "$d" "0" "$2" 1; echo "$3" > "$d/pointer-exit"; standin_start "$d"; run_wrapper "$d"
    echo "$(grep -c -e try-restart -e '--user stop' "$d/log" || true) $(grep -c '^apply$' "$d/log" || true) $(grep -c '^drop$' "$d/log" || true) $(standin_end "$d" | cut -d' ' -f1)"
}
check "an owned unit, pointer refused: no restart, one drop-only sot-apply" "0 0 1 running" "$(refused r-owned 1 3)"
check "a foreign unit, pointer refused: daemon kept, one drop-only sot-apply" "0 0 1 running" "$(refused r-foreign 0 3)"
check "an older sotd's answer (exit 1) leaves the restart in charge" "1 0 0 running" "$(refused r-older 1 1)"
# A real foreign unit, as sot_service_owned defines one: systemctl is here and
# the manifest says systemd, but the unit runs another prefix's sotd.
foreign_unit() {  # <dir>
    printf '{"service": "systemd"}\n' > "$1/prefix/install.json"
    render_sotd_unit "$WORK/elsewhere" "$(dirname "$0")/../../deploy/sotd.service" "$1/home/.config/systemd/user/sotd.service"
    printf '#!/bin/sh\nprintf "systemctl %%s\\n" "$*" >> "$STUB_LOG"\nexit 0\n' > "$1/stubs/systemctl"; chmod +x "$1/stubs/systemctl"
}
d="$WORK/w-real-for-apply"; mk_wrapper "$d" "0" 0 1; foreign_unit "$d"
standin_start "$d"; run_wrapper "$d"
check "a real foreign unit: apply runs no service op, only the pid stop plus sot-apply" \
    "0 1 stopped decoy-alive" "$(grep -c 'sotd.service' "$d/log" || true) $(grep -c '^apply$' "$d/log" || true) $(standin_end "$d")"
d="$WORK/w-real-for-rb"; mk_wrapper "$d" "$(printf '1\n1\n0')" 0 0; foreign_unit "$d"
standin_start "$d"; run_wrapper "$d"
check "a real foreign unit: rollback runs no service op, only the pid stop" \
    "0 stopped decoy-alive" "$(grep -c 'sotd.service' "$d/log" || true) $(standin_end "$d")"

# ---------------------------------------------------------------------------
case_start "the_finders_know_the_daemon_by_the_kernels_name"
# The stand-in daemon, behind an impostor started first: a bash whose argv[0] is sotd and whose arguments name the same
# socket. macOS's ps prints argv[0] as comm, cut to 16 characters in any column but the last, so the daemon's path is
# cut away and the impostor passes as sotd; the kernel's name (ucomm) tells them apart. This box's ps, then on Linux a
# model of macOS's (bsd_ps).
d="$WORK/finders"
( exec -a sotd bash -c 'while :; do sleep 1; done' impostor --socket "$d/sot.sock" ) &
IMPOSTOR=$!
standin_start "$d"
for pid in "$IMPOSTOR" "$SA_PID"; do  # each has exec'd once its arguments end with the socket
    for _ in {1..100}; do case "$(ps -o args= -p "$pid" 2>/dev/null)" in *" --socket $d/sot.sock") break ;; esac; sleep 0.05; done
done
# [PATH-FIRST]: the pid sot_daemon_pid finds for the socket.
finders() { PATH="${1:+$1:}$TOOLS" sot_daemon_pid "$d/sot.sock"; }
check "this box's ps: the finder names the daemon, never the impostor" "$SA_PID" "$(finders)"
# Linux's ps cuts its last column to an exported COLUMNS even into a pipe; -ww keeps the arguments whole.
check "this box's ps, COLUMNS=40 exported: the finder still names the daemon" "$SA_PID" "$(export COLUMNS=40; finders)"
if [ "$(uname -s)" = Linux ]; then
    bsd_ps "$d/bsd"
    check "macOS's ps (modeled): the finder names the daemon, never the impostor" "$SA_PID" "$(finders "$d/bsd")"
else
    printf '    SKIP the modeled macOS ps (it reads /proc: Linux only; this box ran its own ps above)\n'
fi
kill "$IMPOSTOR" 2>/dev/null || true; wait "$IMPOSTOR" 2>/dev/null || true
standin_end "$d" >/dev/null

# ---------------------------------------------------------------------------
case_start "a_converge_relaunches_the_all_in_one_window"
# Exit 76 is a converge (ADR 0017): on Unix it is apply-pending-and-respawn, as 75 is, never a close.
for code in 75 76; do
    d="$WORK/w-conv-$code"; mk_wrapper "$d" "$(printf '%s\n%s\n0' "$code" "$code")" 0 0
    run_wrapper "$d"
    # Two fast exits would be a crash loop, and roll back inside the just-applied window, if the code were a failure.
    check "exit $code twice: the window is respawned each time, never rolled back, and the third run ends it" \
        "0 probe spawn probe spawn probe spawn" "$WR_RC $(events "$d")"
done
d="$WORK/w-conv-apply"; mk_wrapper "$d" "76" 0 0 consume
# A pending update armed while the window ran is applied at the converge, and the wrapper re-execs itself.
printf '#!/bin/sh\necho spawn >> "$STUB_LOG"\nprintf "{}\\n" > "%s/updates/pending-testtarget.json"\nexit 76\n' "$d/prefix" > "$d/prefix/bin/sot"
run_wrapper "$d"
check "exit 76 applies a pending update and re-execs" "0 hop" "$WR_RC $(events "$d" | sed 's/^probe spawn //')"

# ---------------------------------------------------------------------------
case_start "wrapper_reexecs_after_apply"
d="$WORK/w-hop"; mk_wrapper "$d" "0" 0 1 consume
run_wrapper "$d"
check "an apply that consumes the pointer re-execs the wrapper and spawns nothing" \
    "0 hop" "$WR_RC $(events "$d")"
d="$WORK/w-nohop"; mk_wrapper "$d" "0" 0 1
run_wrapper "$d"
check "an apply that leaves the pointer re-execs nothing and spawns once" \
    "0 0 1" "$WR_RC $(grep -c '^hop$' "$d/log" || true) $(grep -c '^spawn$' "$d/log" || true)"

# ---------------------------------------------------------------------------
# The window's log (sot_window_log). run_as <dir> <how> [launcher]: the launcher (default the wrapper) under its own HOME,
# with its streams as <how> says: null (both at /dev/null), outnull (standard output at /dev/null, standard error to
# $d/err), pipe (both into one pipe, captured in $d/piped), files (to $d/o and $d/err); the environment of an app macOS
# started (__CFBundleIdentifier) is set when $ICON_ENV is. Sets WR_RC.
run_as() {
    local d="$1" how="$2" l="${3:-$1/home/.local/bin/sot-launch}"
    WR_RC=0
    local env=(env HOME="$d/home" SOT_COMM_HOME="$d/home/.sot-comm" PATH="$d/stubs:$TOOLS" STUB_LOG="$d/log")
    [ -z "${ICON_ENV:-}" ] || env=("${env[@]}" __CFBundleIdentifier="$ICON_ENV")
    case "$how" in
        null) "${env[@]}" "$l" >/dev/null 2>&1 || WR_RC=$? ;;
        outnull) "${env[@]}" "$l" >/dev/null 2>"$d/err" || WR_RC=$? ;;
        pipe) "${env[@]}" "$l" 2>&1 | cat > "$d/piped" || true; WR_RC=${PIPESTATUS[0]} ;;
        files) "${env[@]}" "$l" >"$d/o" 2>"$d/err" || WR_RC=$? ;;
    esac
}
# force_log <dir>: the case's copy of the library says every launch wants its log, so a case tests the log's mechanics
# (what it keeps, that it fails open, one rotation) apart from the trigger, which has its own cases.
force_log() { printf '\nsot_window_log_wanted() { return 0; }\n' >> "$1/prefix/repo/current/scripts/lib/sot-daemon.sh"; }
modeof() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }   # a file's mode in octal, GNU or BSD stat
windows() { grep -h '^window' "$1" 2>/dev/null | tr '\n' ' ' | sed 's/ $//'; }   # the stand-in window's lines in a file

case_start "window_log_the_file"
d="$WORK/l-file"; mk_wrapper "$d" "0" 0 0; force_log "$d"
run_as "$d" null
check "output and error at /dev/null land in frontend.log beside the daemon's log, the folder made" \
    "0 window out 1 window err 1" "$WR_RC $(windows "$d/state/frontend.log")"
check "the log is private to the account" "600" "$(stat -c %a "$d/state/frontend.log" 2>/dev/null || stat -f %Lp "$d/state/frontend.log")"

case_start "window_log_is_bounded"
d="$WORK/l-bound"; mk_wrapper "$d" "$(printf '0\n0\n0\n0\n0')" 0 0; force_log "$d"
for _ in 1 2 3; do run_as "$d" null; done
check "three launches keep the last two and nothing older" \
    "out 3 err 3|out 2 err 2|frontend.log frontend.prev.log" \
    "$(windows "$d/state/frontend.log" | sed 's/window //g')|$(windows "$d/state/frontend.prev.log" | sed 's/window //g')|$(ls "$d/state" | tr '\n' ' ' | sed 's/ $//')"
printf '#!/bin/sh\necho spawn >> "$STUB_LOG"\nexit 0\n' > "$d/prefix/bin/sot"
run_as "$d" null
run_as "$d" null
check "a launch that said nothing leaves the older backup alone" \
    "out 3 err 3||" \
    "$(windows "$d/state/frontend.prev.log" | sed 's/window //g')|$(windows "$d/state/frontend.log")|"
cat > "$d/prefix/bin/sot" <<'SOT'
#!/bin/sh
echo spawn >> "$STUB_LOG"
echo "window out $(grep -c '^spawn$' "$STUB_LOG")"
exit 0
SOT
run_as "$d" null
check "the next launch that speaks keeps that older backup" "out 6|out 3 err 3" \
    "$(windows "$d/state/frontend.log" | sed 's/window //g')|$(windows "$d/state/frontend.prev.log" | sed 's/window //g')"

case_start "window_log_fails_open"
touch "$WORK/a-file"
mkdir -p "$WORK/l-open-state/frontend.log"
i=0
for variant in "a folder under a file|$WORK/a-file/sot/sotd.log" "a log that is a folder|$WORK/l-open-state/sotd.log" \
               "no answer from sotd|" "a relative answer|sotd.log"; do
    i=$((i + 1)); d="$WORK/l-open-$i"; mk_wrapper "$d" "0" 0 0; force_log "$d"
    printf '%s\n' "${variant#*|}" > "$d/logpath"
    run_as "$d" outnull
    check "${variant%%|*}: the window still starts, and its error stays where it was with nothing added" \
        "0 probe spawn|window err 1" "$WR_RC $(events "$d")|$(cat "$d/err")"
done
check "a folder that cannot be made leaves nothing behind" "no" "$([ -e "$WORK/a-file/sot" ] && echo yes || echo no)"

case_start "window_log_a_file_or_terminal_is_left_alone_even_when_the_trigger_holds"
d="$WORK/l-forced-file"; mk_wrapper "$d" "0" 0 0; force_log "$d"
run_as "$d" files
check "standard output into a file a caller gave: the trigger holds and the lines still go to the caller; no log file" \
    "0 window out 1|window err 1|no" "$WR_RC $(windows "$d/o")|$(windows "$d/err")|$([ -e "$d/state" ] && echo yes || echo no)"
# A terminal: util-linux's script gives the wrapper a pseudo-terminal for its output (BSD's script takes other arguments).
if [ "$(uname -s)" = Linux ] && script --version 2>/dev/null | grep -q util-linux; then
    d="$WORK/l-forced-tty"; mk_wrapper "$d" "0" 0 0; force_log "$d"
    WR_RC=0
    script -qec "$(printf '%q ' env HOME="$d/home" SOT_COMM_HOME="$d/home/.sot-comm" PATH="$d/stubs:$TOOLS" STUB_LOG="$d/log" "$d/home/.local/bin/sot-launch")" "$d/typescript" \
        >/dev/null 2>&1 </dev/null || WR_RC=$?
    tr -d '\r' < "$d/typescript" > "$d/typescript.txt"
    check "standard output a terminal: the trigger holds and the lines still go to the terminal; no log file" \
        "0 window out 1 window err 1 no" "$WR_RC $(windows "$d/typescript.txt") $([ -e "$d/state" ] && echo yes || echo no)"
else
    printf '    SKIP a terminal for standard output (util-linux script is not here)\n'
fi

case_start "window_log_trigger"
ours="$(sot_bundle_id)"
check "the trigger is a truth table of the app identifier and the parent pid: ours and 1, ours and another parent, another app and 1, none and 1, none" \
    "yes no no no no" \
    "$(sot_window_log_wanted "$ours" 1 && echo yes || echo no) $(sot_window_log_wanted "$ours" 4242 && echo yes || echo no) $(sot_window_log_wanted com.example.other 1 && echo yes || echo no) $(sot_window_log_wanted "" 1 && echo yes || echo no) $(sot_window_log_wanted "" "" && echo yes || echo no)"
check "the identifier is a reverse-domain name" "yes" "$(case "$ours" in (*.*.*) echo yes ;; (*) echo no ;; esac)"

case_start "window_log_leaves_every_other_start_alone"
d="$WORK/l-pipe"; mk_wrapper "$d" "0" 0 0
run_as "$d" pipe
check "into a pipe: the lines arrive on the pipe and no log file is made" \
    "0 window out 1 window err 1 no" "$WR_RC $(windows "$d/piped") $([ -e "$d/state" ] && echo yes || echo no)"
d="$WORK/l-files"; mk_wrapper "$d" "0" 0 0
run_as "$d" files
check "into files a caller gave: each has its line and no log file is made" \
    "0 window out 1|window err 1|no" "$WR_RC $(windows "$d/o")|$(windows "$d/err")|$([ -e "$d/state" ] && echo yes || echo no)"
d="$WORK/l-desc"; mk_wrapper "$d" "0" 0 0
ICON_ENV="$(sot_bundle_id)" run_as "$d" null
check "a descendant of the app, which inherits the identifier but has another parent, makes no log file even at /dev/null" \
    "0 no" "$WR_RC $([ -e "$d/state" ] && echo yes || echo no)"
d="$WORK/l-desc-pipe"; mk_wrapper "$d" "0" 0 0
ICON_ENV="$(sot_bundle_id)" run_as "$d" pipe
check "and its lines arrive on its pipe" "0 window out 1 window err 1 no" "$WR_RC $(windows "$d/piped") $([ -e "$d/state" ] && echo yes || echo no)"

case_start "window_log_once_per_launch"
d="$WORK/l-once"; mk_wrapper "$d" "0" 0 1 consume-keep; force_log "$d"
mkdir -p "$d/state"; printf 'old\n' > "$d/state/frontend.log"
run_as "$d" null
check "the wrapper re-execs itself after the apply and the launch rotates once (stdout already is the log): the old log is the backup, this launch's lines are the log" \
    "0 1 1 old|apply-said window out 1 window err 1" \
    "$WR_RC $(grep -c '^apply$' "$d/log" || true) $(grep -c '^spawn$' "$d/log" || true) $(cat "$d/state/frontend.prev.log")|$(grep -h '^apply-said\|^window' "$d/state/frontend.log" | tr '\n' ' ' | sed 's/ $//')"

case_start "window_log_reaches_the_old_icon_launcher"
d="$WORK/l-icon"; mk_wrapper "$d" "0" 0 0; force_log "$d"
printf '#!/usr/bin/env bash\nexec "%s"\n' "$d/home/.local/bin/sot-launch" > "$d/icon"; chmod +x "$d/icon"
run_as "$d" null "$d/icon"
check "the two-line icon launcher of an earlier install reaches the wrapper's log: its lines end in the file" \
    "0 window out 1 window err 1" "$WR_RC $(windows "$d/state/frontend.log")"

# mk_remote <dir> <old|new>: the wrapper that dials the plan over the checkout's own launch-sot.sh and libraries (the
# real files of this tree, with a stand-in window and sotd). `new` is what render_remote_launch writes now; `old` is the
# text an earlier release wrote, which an update does not re-render (sot_wrapper_owned knows only the all-in-one form).
mk_remote() {
    local d="$1" which="$2" prefix="$1/prefix" scripts="$1/prefix/repo/current/scripts"
    mkdir -p "$scripts/lib"
    cp "$LIB" "$scripts/lib/sot-daemon.sh"; cp "$(dirname "$LIB")/sot-hosts.sh" "$scripts/lib/sot-hosts.sh"
    cp "$(dirname "$LIB")/../launch-sot.sh" "$scripts/launch-sot.sh"; chmod +x "$scripts/launch-sot.sh"
    if [ "$which" = new ]; then
        ( HOME="$d/home" render_remote_launch "$prefix" "" "$d/remote" )
    else
        cat > "$d/remote" <<OLD
#!/usr/bin/env bash
# sot-launch: dials the topology plan (a frontend over SSH to its daemon hosts, ADR 0030 section 5). It applies an armed
# update first, then runs the pinned checkout's own launch-sot.sh, so the two never drift.
if [ -x "$prefix/bin/sot-apply" ]; then
    APPLY_OUT="\$("$prefix/bin/sot-apply" 2>&1)"
    [ -n "\$APPLY_OUT" ] && printf '%s\n' "\$APPLY_OUT" >&2
fi
export SOT_OWN_DAEMON=1
export SOT_PREFIX="$prefix"
export SOT_FRONTEND_BIN="$prefix/bin/sot"
export SOT_NO_UPDATE=1
exec "$prefix/repo/current/scripts/launch-sot.sh" "\$@"
OLD
        chmod +x "$d/remote"
    fi
}

case_start "window_log_remote_launcher"
d="$WORK/l-remote-text"; mk_wrapper "$d" "0" 0 0; mk_remote "$d" new
check "the wrapper that dials the plan names no log function: the checkout's launch-sot.sh makes the one call" "0 1" \
    "$(grep -c sot_window_log "$d/remote" || true) $(grep -c '^type sot_window_log' "$d/prefix/repo/current/scripts/launch-sot.sh" || true)"
for which in new old; do
    d="$WORK/l-remote-$which"; mk_wrapper "$d" "0" 0 0; mk_remote "$d" "$which"; force_log "$d"
    mkdir -p "$d/state"; printf 'old\n' > "$d/state/frontend.log"
    run_as "$d" null "$d/remote"
    check "the $which wrapper over this checkout (trigger forced): the window's lines are the log and the old log is the backup, once" \
        "0 old|window out 1 window err 1" "$WR_RC $(cat "$d/state/frontend.prev.log")|$(windows "$d/state/frontend.log")"
done
d="$WORK/l-remote-pipe"; mk_wrapper "$d" "0" 0 0; mk_remote "$d" new
ICON_ENV="$(sot_bundle_id)" run_as "$d" pipe "$d/remote"
check "the wrapper that dials the plan: a start that is not an icon launch is left alone, its lines on its pipe" \
    "0 window out 1 window err 1 no" "$WR_RC $(windows "$d/piped") $([ -e "$d/state" ] && echo yes || echo no)"

case_start "window_log_the_folder_is_private_as_the_daemons_is"
# The state folder holds this account's logs and state, and the wrapper can be the first to make it, before any daemon:
# private as the daemon makes it (the leaf 0700 whatever the umask), so its file is never readable by others.
d="$WORK/l-mode"; mk_wrapper "$d" "0" 0 0; force_log "$d"
printf '%s\n' "$d/xdg/state/sot/sotd.log" > "$d/logpath"
( umask 022; run_as "$d" null )
check "a folder made under umask 022: the leaf is 0700, a file in it 0600, and no group or other bit is set" "700 600 0" \
    "$(modeof "$d/xdg/state/sot") $(modeof "$d/xdg/state/sot/frontend.log") $(( 0$(modeof "$d/xdg/state/sot") & 077 ))"
d="$WORK/l-mode-old"; mk_wrapper "$d" "0" 0 0; force_log "$d"
mkdir -p "$d/state"; chmod 755 "$d/state"
( umask 022; run_as "$d" null )
check "a folder that already exists keeps its mode" "755" "$(modeof "$d/state")"

# ---------------------------------------------------------------------------
# The real trigger: a wrapper whose parent is process 1 and whose environment names the app, with pipes for its streams as
# macOS gives them, inside an unprivileged user and process namespace (its first process is process 1).
have_ns=0
if [ "$(uname -s)" = Linux ] && command -v unshare >/dev/null 2>&1 && unshare -Urpf true >/dev/null 2>&1; then have_ns=1; fi
# run_real <dir> [launcher]: starts the launcher as process 1's child (the shell stays process 1, so the namespace outlives
# the window), standard output and error one pipe captured in $d/piped, umask 022. Sets WR_RC.
run_real() {
    local d="$1" l="${2:-$1/home/.local/bin/sot-launch}"
    rm -f "$d/rc"
    unshare -Urpf env HOME="$d/home" SOT_COMM_HOME="$d/home/.sot-comm" PATH="$d/stubs:$TOOLS" STUB_LOG="$d/log" \
        __CFBundleIdentifier="$(sot_bundle_id)" bash -c 'umask 022; "$@"; echo $? > "$0"' "$d/rc" "$l" 2>&1 | cat > "$d/piped" || true
    WR_RC="$(cat "$d/rc" 2>/dev/null || echo none)"
}
# run_detached <dir> <launcher>: what an agent inside the app does, `launcher > caller-file 2>&1 &` from a shell that ends
# at once, so the launcher is orphaned and process 1 is its parent; waits for the window's last line.
run_detached() {
    local d="$1" l="$2"
    unshare -Urpf env HOME="$d/home" SOT_COMM_HOME="$d/home/.sot-comm" PATH="$d/stubs:$TOOLS" STUB_LOG="$d/log" \
        __CFBundleIdentifier="$(sot_bundle_id)" bash -c '( "$0" > "$1" 2>&1 & ); n=0
            while [ "$n" -lt 100 ] && ! grep -q "^window err" "$1" 2>/dev/null; do sleep 0.1; n=$((n + 1)); done; sleep 0.3' \
        "$l" "$d/caller" >/dev/null 2>&1 || true
}

case_start "window_log_the_real_trigger"
if [ "$have_ns" != 1 ]; then
    printf '    SKIP the real trigger on Linux (this kernel refuses an unprivileged user and process namespace: unshare -Urpf)\n'
else
    ours="$(sot_bundle_id)"
    d="$WORK/r-file"; mk_wrapper "$d" "0" 0 0
    run_real "$d"
    check "icon start (identifier, parent 1, pipes): the window's lines are in the file, none on the pipe, the folder 0700 and the file 0600" \
        "0 window out 1 window err 1||700 600" \
        "$WR_RC $(windows "$d/state/frontend.log")|$(windows "$d/piped")|$(modeof "$d/state") $(modeof "$d/state/frontend.log")"
    d="$WORK/r-old"; mk_wrapper "$d" "0" 0 0; mkdir -p "$d/state"; chmod 755 "$d/state"
    run_real "$d"
    check "a state folder that exists keeps its mode" "755" "$(modeof "$d/state")"
    d="$WORK/r-three"; mk_wrapper "$d" "$(printf '0\n0\n0')" 0 0
    for _ in 1 2 3; do run_real "$d"; done
    check "three icon starts keep the last two and nothing older" "out 3 err 3|out 2 err 2|frontend.log frontend.prev.log" \
        "$(windows "$d/state/frontend.log" | sed 's/window //g')|$(windows "$d/state/frontend.prev.log" | sed 's/window //g')|$(ls "$d/state" | tr '\n' ' ' | sed 's/ $//')"
    d="$WORK/r-once"; mk_wrapper "$d" "0" 0 1 consume-keep
    mkdir -p "$d/state"; printf 'old\n' > "$d/state/frontend.log"
    run_real "$d"
    check "the re-exec after an apply keeps the identifier and parent 1 and rotates nothing twice" \
        "0 1 1 old|apply-said window out 1 window err 1" \
        "$WR_RC $(grep -c '^apply$' "$d/log" || true) $(grep -c '^spawn$' "$d/log" || true) $(cat "$d/state/frontend.prev.log")|$(grep -h '^apply-said\|^window' "$d/state/frontend.log" | tr '\n' ' ' | sed 's/ $//')"
    d="$WORK/r-icon"; mk_wrapper "$d" "0" 0 0
    printf '#!/usr/bin/env bash\nexec "%s"\n' "$d/home/.local/bin/sot-launch" > "$d/icon"; chmod +x "$d/icon"
    run_real "$d" "$d/icon"
    check "the two-line icon launcher of an earlier install reaches the log" "0 window out 1 window err 1|" "$WR_RC $(windows "$d/state/frontend.log")|$(windows "$d/piped")"
    for which in old new; do
        d="$WORK/r-remote-$which"; mk_wrapper "$d" "0" 0 0; mk_remote "$d" "$which"
        mkdir -p "$d/state"; printf 'old\n' > "$d/state/frontend.log"
        run_real "$d" "$d/remote"
        check "the $which wrapper that dials the plan, over this checkout: the log is written once (the old log is the backup), nothing on the pipe" \
            "0 old|window out 1 window err 1|" "$WR_RC $(cat "$d/state/frontend.prev.log")|$(windows "$d/state/frontend.log")|$(windows "$d/piped")"
    done
    d="$WORK/r-detached"; mk_wrapper "$d" "0" 0 0
    mkdir -p "$d/state"; printf 'the running window\n' > "$d/state/frontend.log"
    run_detached "$d" "$d/home/.local/bin/sot-launch"
    check "a start detached from inside the app (identifier, parent 1, a file for output) is left alone: the caller's file has the lines and the running window's log is not rotated" \
        "window out 1 window err 1|the running window|no" \
        "$(windows "$d/caller")|$(cat "$d/state/frontend.log")|$([ -e "$d/state/frontend.prev.log" ] && echo yes || echo no)"
fi

# ---------------------------------------------------------------------------
printf '\n'
if [ "$fails" -eq 0 ]; then
    printf 'installer-wrapper: all checks passed\n'
else
    printf 'installer-wrapper: %d check(s) FAILED\n' "$fails" >&2
    exit 1
fi
