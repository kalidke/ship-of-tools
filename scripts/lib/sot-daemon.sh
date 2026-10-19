SOT_LAUNCH_WAIT_S=160  # = rust/protocol/src/ops/lease.rs LAUNCH_WAIT; scripts/tests/installer-state.sh compares them
SOT_LOG_KEEP=5               # a start leaves at most this many nohup daemon logs, its own included, for a keep of 2 or more (sot_prune_logs)
SOT_LOG_CAP_BYTES=16777216   # 16MB: the unprotected logs' total a start prunes down to (sot_prune_logs)

# sot-daemon.sh -- the one place the sotd unit, the all-in-one sot-launch
# wrapper and the backend ensure are written down. Sourced by install.sh,
# sot-apply.sh, launch-sot.sh, restart-backend.sh and, through the text
# render_sot_launch writes, by the rendered wrapper itself. sot-apply.sh runs
# under /bin/sh (dash) and macOS ships bash 3.2, so this file is POSIX shell
# plus `local`: no arrays, `[[`, `declare`, `$'...'`, `function`, or `==` in `[`.

# ExecStart's binary path from a unit's text (stdin -> stdout); the same
# two shapes as install.sh's installer_unit_owner_path, which stays there
# because its ownership gate runs before any checkout exists. A test pins
# the two copies equal.
sot_unit_owner_path() {
    sed -n -E 's/^ExecStart=(.*exec ")?([^ "]+)"?.*/\2/p' | head -1
}

# The owner prefix a sot-launch wrapper's content embeds (stdin -> stdout);
# copy of install.sh's installer_wrapper_owner_prefix, pinned equal by test.
sot_wrapper_owner_prefix() {
    sed -n -E \
        -e 's#^PENDING="(.+)/updates/pending-.*"$#\1#p' \
        -e 's#^export SOT_FRONTEND_BIN="(.+)/bin/sot"$#\1#p' \
        -e 's#^exec "(.+)/bin/sot".*#\1#p' \
        | head -1
}

# Byte-identical to scripts/sot-apply.sh's (a test pins it): a value spliced into a sed replacement (delimiter |) with
# its backslash, ampersand and bar escaped, so a path or version holding any of them lands byte for byte.
sot_sed_literal() {  # <text>: the text as the replacement of an s|...|...| command, every byte literal
    printf '%s' "$1" | sed -e 's/[\\&|]/\\&/g'
}

# The one list of characters a path may not hold, because a rendered file splices it into text another reader parses: the
# unit's `bash -c` and the launchers' double-quoted text (`'`, `"`, `$`, backquote, `\`, newline), systemd (`%`, `\`), sed,
# JSON and shell lines (`&`, `|`, `;`, space, tab). Byte-identical to scripts/install.sh's and scripts/sot-apply.sh's (a test
# pins both): install.sh's front gate and sot-apply's check before the flip run it, and each render runs it before its own
# write. Returns 0 after one stderr line naming what (prefix or home), the path and the character, else 1.
sot_path_refused() {  # <what: prefix|home> <path>
    local ch= list='% \ '"'"' " $ ` & | ; space tab newline'
    case "$2" in
        *'%'*) ch='%' ;;
        *'\'*) ch='\' ;;
        *"'"*) ch="'" ;;
        *'"'*) ch='"' ;;
        *'$'*) ch='$' ;;
        *'`'*) ch='`' ;;
        *'&'*) ch='&' ;;
        *'|'*) ch='|' ;;
        *';'*) ch=';' ;;
        *' '*) ch=space ;;
        *'	'*) ch=tab ;;
        *'
'*) ch=newline ;;
        *) return 1 ;;
    esac
    printf 'ERROR: the %s %s holds %s, which a unit, launcher or shell line would read as syntax; use a %s with none of: %s\n' "$1" "$2" "$ch" "$1" "$list" >&2
    return 0
}

# Write the sotd.service unit for PREFIX from TEMPLATE into DEST. Like every
# rendered file it is written beside DEST, under a name of this shell's own
# ($$), and moved in, so a failed write leaves the old file whole and returns 1.
render_sotd_unit() {  # <prefix> <template> <dest>
    sot_path_refused prefix "$1" && return 1
    sot_path_refused home "$HOME" && return 1
    sed -e "s|@SOT_BIN@|$(sot_sed_literal "$1/bin/sotd")|" \
        -e "s|@SOT_APPLY@|$(sot_sed_literal "$1/bin/sot-apply")|" \
        -e "s|@SOT_PROJECT_ROOT@|$(sot_sed_literal "$HOME")|" \
        "$2" > "$3.new.$$" && chmod 0644 "$3.new.$$" && mv -f "$3.new.$$" "$3" || { rm -f "${3:?}.new.$$"; return 1; }
}

# Start sotd.service at boot: DIR/default.target.d/sotd.conf holds `Wants=sotd.service`, since the unit has no [Install]
# section and so `systemctl enable` and `disable` change no file of a home several hosts share. The drop-in is written
# first (beside its name, then moved in), and only then is a link an earlier install's `enable` left, DIR's
# default.target.wants/sotd.service, removed, and only when it is a symlink: there is never a moment with neither.
sot_boot_wants() {  # <systemd-user-dir>
    local dir="$1" conf="$1/default.target.d/sotd.conf" link="$1/default.target.wants/sotd.service"
    mkdir -p "$dir/default.target.d" || return 1
    printf '[Unit]\nWants=sotd.service\n' > "$conf.new.$$" && chmod 0644 "$conf.new.$$" && mv -f "$conf.new.$$" "$conf" \
        || { rm -f "${conf:?}.new.$$"; return 1; }
    if [ -L "$link" ]; then rm -f "$link"; fi
    return 0
}

# Does the installed sotd refuse the pointer armed for TARGET under PREFIX? `sotd update pointer` exits 3 for one that no
# longer fits the install, or cannot be read. Any other answer (an older sotd exits 1 or 2, none at all 126 or 127) is
# no refusal here: sot-apply's own checks decide, as before. sot-apply.sh asks the same question and drops the pointer.
sot_pointer_refused() {  # <prefix> <target>
    [ -x "$1/bin/sotd" ] || return 1
    "$1/bin/sotd" update pointer --prefix "$1" --target "$2" >/dev/null 2>&1
    [ $? -eq 3 ]
}

# Write the all-in-one sot-launch wrapper for PREFIX / TARGET into DEST.
render_sot_launch() {  # <prefix> <target> <dest>
    local prefix="$1" target="$2" dest="$3"
    sot_path_refused prefix "$prefix" && return 1
    sot_path_refused home "$HOME" && return 1
    cat > "$dest.new.$$" <<EOF || { rm -f "${dest:?}.new.$$"; return 1; }
#!/usr/bin/env bash
# sot-launch: all-in-one
# All-in-one launcher: apply any armed pending update (offline pointer flip,
# fail-open) and re-exec itself so the new wrapper and library run, make
# sure the backend answers before EVERY frontend spawn, then SUPERVISE the
# frontend: exit-75 respawn (ADR 0017 on Unix) and crash-loop rollback of a
# just-applied update (ADR 0030 Phase C3).
PENDING="$prefix/updates/pending-$target.json"
MARKER="$prefix/updates/just-applied-$target"
# The checkout's library, read at run time: a rollback restores the wrapper
# together with the checkout, so the pair stays matched.
LIB="$prefix/repo/current/scripts/lib/sot-daemon.sh"
. "\$LIB" || { echo "ERROR: cannot read \$LIB" >&2; exit 1; }
SOCKET="\$("$prefix/bin/sotd" session-socket-path sot)"
# An owned service is stopped through systemd so its restart policy cannot
# race the stop.
stop_daemon() {
    if sot_service_owned "$prefix"; then
        systemctl --user stop sotd.service >/dev/null 2>&1
    else
        pkill -u "\$(id -u)" -f "$prefix/bin/sotd" 2>/dev/null && sleep 1
    fi
}
# Apply an armed update (ADR 0030 Phase C): an active owned service is
# try-restarted so its ExecStartPre runs sot-apply; otherwise the daemon is
# stopped FIRST, then sot-apply runs here.
# Succeeds only when the pointer is consumed: the caller then re-execs, and
# the consumed pointer is what stops the re-exec'd wrapper doing it again.
apply_pending() {
    [ -f "\$PENDING" ] || return 1
    [ -x "$prefix/bin/sot-apply" ] || return 1
    # A pointer the installed sotd refuses costs nothing: sot-apply drops it under the lock, and the daemon, with the
    # REPLs it holds, is neither stopped nor restarted for an update that will not happen. It runs drop-only: a pointer
    # that fits and was armed since the question waits for the next start, with the daemon stopped as usual.
    if type sot_pointer_refused >/dev/null 2>&1 && sot_pointer_refused "$prefix" "$target"; then
        "$prefix/bin/sot-apply" --drop-only >&2
        return 1
    fi
    if sot_service_owned "$prefix" && systemctl --user is-active --quiet sotd.service >/dev/null 2>&1; then
        echo "pending update armed - restarting sotd so ExecStartPre applies it" >&2
        systemctl --user try-restart sotd.service || true
    else
        stop_daemon
        APPLY_OUT="\$("$prefix/bin/sot-apply" 2>&1)"
        [ -n "\$APPLY_OUT" ] && printf '%s\n' "\$APPLY_OUT" >&2
    fi
    [ ! -f "\$PENDING" ]
}
if apply_pending; then exec "\$0" "\$@"; fi
# The agent comm scripts follow the checkout, whoever applied it: an applied tag can change the comm protocol.
sot_comm_refresh "$prefix/repo/current"
FAILS=0; ROLLED=0
while :; do
    sot_daemon_ensure "$prefix" "$prefix/bin/sotd" "\$SOCKET" || exit 1
    # What the ensure did, for the window's startup notice (the frontend reads SOT_LAUNCH_NOTICE).
    if [ -n "\$SOT_ENSURE_NOTICE" ]; then export SOT_LAUNCH_NOTICE="\$SOT_ENSURE_NOTICE"; else unset SOT_LAUNCH_NOTICE; fi
    START="\$(date +%s)"
    "$prefix/bin/sot" --socket "\$SOCKET"
    RC=\$?
    NOW="\$(date +%s)"
    RUNTIME=\$((NOW - START))
    # A healthy run closes the crash-loop health window.
    [ "\$RUNTIME" -ge 60 ] && rm -f "\${MARKER:?}" 2>/dev/null
    if [ "\$RC" -eq 75 ] || [ "\$RC" -eq 76 ]; then
        # ADR-0017 self-relaunch (75) or converge (76): on Unix both pick up any staged update, then respawn.
        if apply_pending; then exec "\$0" "\$@"; fi
        FAILS=0
        continue
    fi
    if [ "\$RC" -ne 0 ] && [ "\$RUNTIME" -le 10 ]; then
        FAILS=\$((FAILS + 1))
        if [ "\$FAILS" -ge 2 ]; then
            # Roll back ONLY inside the just-applied health window; an
            # unrelated crash weeks later must not downgrade a healthy
            # release.
            if [ "\$ROLLED" -eq 0 ] && [ -f "\$MARKER" ] \
               && [ -n "\$(find "\$MARKER" -mmin -30 2>/dev/null)" ]; then
                echo "frontend crash-looped inside the post-update window - rolling back" >&2
                stop_daemon
                [ -x "$prefix/bin/sot-apply" ] && "$prefix/bin/sot-apply" --rollback >&2
                ROLLED=1; FAILS=0
                continue
            fi
            exit "\$RC"
        fi
        continue
    fi
    exit "\$RC"
done
EOF
    chmod +x "$dest.new.$$" && mv -f "$dest.new.$$" "$dest" || { rm -f "${dest:?}.new.$$"; return 1; }
}

# True when SOCKET accepts a connection from this OS account: sotd's own bridge
# connects only to a socket this account listens on (ADR 0049, User isolation)
# and, its input empty, closes again at once. A socket it cannot
# reach is not open, and is never removed on that evidence.
sot_socket_open() {  # <sotd-bin> <socket>
    [ -S "$2" ] || return 1
    "$1" stdio-bridge --endpoint "unix:$2" </dev/null >/dev/null 2>&1
}

# True when this install owns a systemd user unit for the backend: systemctl
# is here, the manifest says systemd, and the unit file install.sh wrote
# points at this prefix's sotd. Reads the FILE, not `systemctl cat`, so apply,
# ensure and rollback ask one question the same way.
sot_service_owned() {  # <prefix>
    command -v systemctl >/dev/null 2>&1 || return 1
    grep -q '"service": *"systemd"' "$1/install.json" 2>/dev/null || return 1
    [ "$(sot_unit_owner_path < "$HOME/.config/systemd/user/sotd.service" 2>/dev/null)" = "$1/bin/sotd" ]
}

# True when LOG is NEWEST or the pid in its name sotd.<stamp>Z-<pid>.log is alive.
sot_log_protected() {  # <log> <newest>
    local p="${1##*/}"
    [ "$1" = "$2" ] && return 0
    p="${p##*Z-}"; p="${p%.log}"
    case "$p" in ''|*[!0-9]*) return 1 ;; esac
    kill -0 "$p" 2>/dev/null
}

# Prune DIR's nohup daemon logs before a start. Oldest first (the legacy fixed
# sotd.log, then sotd.<UTC stamp>Z-<pid>.log by name, which is start order),
# delete while SOT_LOG_KEEP or more remain (the log about to be created is the last
# of the KEEP; the newest old log is never deleted, so a keep of 1 leaves 2) or the unprotected total exceeds SOT_LOG_CAP_BYTES. A log is protected while the <pid> in its name answers
# kill -0 (its writer lives), or while it is the newest (the legacy sotd.log has
# no pid); a protected file is never deleted and is left out of the total. A
# failed rm is logged in one line and skipped; it leaves both bounds.
sot_prune_logs() {  # <dir>
    local dir="$1" f newest="" count total=0 size err
    set --
    [ -f "$dir/sotd.log" ] && set -- "$dir/sotd.log"
    for f in "$dir"/sotd.[0-9]*Z-*.log; do
        [ -f "$f" ] && set -- "$@" "$f"
    done
    count=$#
    # ${1+"$@"}, not "$@": with no logs, bash below 4.1 exits a set -u shell.
    for f in ${1+"$@"}; do newest="$f"; done
    for f in ${1+"$@"}; do
        sot_log_protected "$f" "$newest" && continue
        size="$(wc -c < "$f" 2>/dev/null)" || size=0
        total=$((total + ${size:-0}))
    done
    for f in ${1+"$@"}; do
        [ "$count" -ge "$SOT_LOG_KEEP" ] || [ "$total" -gt "$SOT_LOG_CAP_BYTES" ] || break
        sot_log_protected "$f" "$newest" && continue
        size="$(wc -c < "$f" 2>/dev/null)" || size=0
        err="$(rm -f -- "${f:?}" 2>&1)" || printf 'kept old log %s: %s\n' "$f" "$err" >&2
        count=$((count - 1))
        total=$((total - ${size:-0}))
    done
    return 0
}

# The pid of this account's daemon on SOCKET, else (LABEL given) of the first one
# started with exactly LABEL; nothing when there is none. Among this account's
# processes named `sotd` (the kernel's `comm`, so a program that only mentions sotd
# in its arguments is not one; macOS's ps prints the program's path, so its last
# part is compared), the one started with `--socket` and exactly this socket. Both
# are literal comparisons of whole arguments: the values reach awk through its
# environment, never as a pattern or an escape, and a socket or label that only
# begins with this one is another daemon's (ADR 0049, User isolation).
sot_daemon_pid() {  # <socket> [label]
    ps -u "$(id -u)" -o pid=,comm=,args= | FIND_SOCKET="$1" FIND_LABEL="${2:-}" awk '
        BEGIN { sock = " --socket " ENVIRON["FIND_SOCKET"] " "; label = " --label " ENVIRON["FIND_LABEL"] " " }
        { n = $2; sub(/.*\//, "", n) } n != "sotd" { next }
        { args = $0; sub(/^ *[0-9]+ +[^ ]+ +/, "", args); args = " " args " " }
        index(args, sock) { print $1; found = 1; exit }
        ENVIRON["FIND_LABEL"] != "" && index(args, label) && first == "" { first = $1 }
        END { if (!found && first != "") print first }
    '
}

# Whether the daemon answering on SOCKET may stay: 0 when it may, 1 once it was asked
# to end (the ensure's start and wait loop then takes over, whatever came of it). It
# may stay unless `sotd same-version`, asked of the binary this install runs (an owned
# unit's own, else SOTD-BIN), says it runs another version (exit 3), which an update
# leaves when it replaces the binary and not the process; exit 4 says the same while a
# window holds a lease, and that daemon stays. SOT_ENSURE_NOTICE names what a launch
# did for the window to show (daemon out of date, or replaced). That daemon is named before it is asked,
# so a launch ends only the daemon it asked and never the successor another launch just
# started: an owned unit's MainPID, else the pid found by its socket (sot_daemon_pid).
# An owned unit is restarted through systemd onto the new binary only while MainPID is
# still that pid; a nohup daemon is sent TERM and waited for, at most 10 s. A daemon it
# cannot name or end stays, with a warning: the window still starts and names the skew.
sot_daemon_current() {  # <prefix> <sotd-bin> <socket>
    local run="$2" owned=0 theirs pid now t0 rc
    if sot_service_owned "$1"; then
        owned=1; run="$1/bin/sotd"
        pid="$(systemctl --user show -p MainPID --value sotd.service 2>/dev/null)"
    else
        pid="$(sot_daemon_pid "$3")"
    fi
    theirs="$("$run" same-version --endpoint "unix:$3" 2>/dev/null)"
    rc=$?
    if [ "$rc" -eq 4 ]; then
        SOT_ENSURE_NOTICE="daemon out of date, restart when ready"
        printf 'the backend on %s runs %s, not the version of %s, and a window holds it: kept; %s\n' "$3" "$theirs" "$run" "$SOT_ENSURE_NOTICE" >&2
        return 0
    fi
    [ "$rc" -eq 3 ] || return 0
    case "$pid" in ''|0|*[!0-9]*)
        printf 'WARNING: could not find the backend on %s to end it; the window will show it out of date\n' "$3" >&2
        return 0 ;;
    esac
    SOT_ENSURE_NOTICE="the backend ran $theirs and was replaced; its rows keep running and their REPLs restart"
    printf 'the backend on %s runs %s, not the version of %s: replacing it (its rows keep running; their REPLs restart)\n' "$3" "$theirs" "$run" >&2
    if [ "$owned" = 1 ]; then
        now="$(systemctl --user show -p MainPID --value sotd.service 2>/dev/null)"
        [ "$now" != "$pid" ] || systemctl --user restart sotd.service >/dev/null 2>&1
    else
        kill "$pid" 2>/dev/null || ! kill -0 "$pid" 2>/dev/null || {
            printf 'WARNING: could not end the backend on %s; the window will show it out of date\n' "$3" >&2
            return 0
        }
        t0="$(date +%s)"
        while sot_socket_open "$2" "$3" && [ "$(date +%s)" -lt $((t0 + 10)) ]; do sleep 0.25; done
        if sot_socket_open "$2" "$3"; then
            printf 'WARNING: the backend on %s did not end; the window will show it out of date\n' "$3" >&2
            return 0
        fi
    fi
    return 1
}

# Bring the agent comm resources to the checkout's commit: `ShipTools.update_comm()` runs when the comm install is
# missing or its VERSION stamp (the short commit update_comm writes last, `-dirty` when its tree had changes) names
# another commit than CHECKOUT's, the rule sot-freshness.ps1 states for Windows. Answered from disk, so whoever applied
# the update and whenever, an apply refreshes comm at the next launch; a commit that cannot be read decides nothing. A
# failure warns and returns 0, and leaves no stamp, so the next launch tries again.
sot_comm_refresh() {  # <checkout>
    local home="${SOT_COMM_HOME:-$HOME/.sot-comm}" stamp="" commit
    commit="$(cd "$1" 2>/dev/null && git rev-parse --short=9 HEAD 2>/dev/null)"
    [ ! -f "$home/VERSION" ] || stamp="$(sed -e 's/-dirty$//' -e 1q "$home/VERSION")"
    if [ -d "$home/bin" ] && { [ -z "$commit" ] || [ "$stamp" = "$commit" ]; }; then return 0; fi
    PATH="$PATH:$HOME/.juliaup/bin"
    command -v julia >/dev/null 2>&1 || { echo "WARNING: comm is not at this checkout's commit and julia is not on PATH" >&2; return 0; }
    # Bounded where `timeout` exists (macOS has none): a stalled julia costs the refresh, not the unit's start timeout.
    # update_comm writes the stamp last, so a refresh that did not finish leaves none and the next launch tries again.
    local bound=""
    ! command -v timeout >/dev/null 2>&1 || bound="timeout ${SOT_COMM_REFRESH_TIMEOUT_S:-30}"
    $bound julia --project="$1" -e 'using ShipTools; ShipTools.update_comm()' >/dev/null 2>&1
    case $? in
        0) ;;
        124) echo "WARNING: ShipTools.update_comm() timed out; the next launch tries again" >&2 ;;
        *) echo "WARNING: ShipTools.update_comm() failed; the next launch tries again" >&2 ;;
    esac
    return 0
}

# Make the backend's socket answer, with a daemon of this install's version
# (sot_daemon_current): return 0 once it does, 1 (with the reason on stderr) if
# it cannot. Waits SOT_LAUNCH_WAIT_S for a successor while a previous instance
# is still shutting down. Never removes the socket: the
# daemon unlinks a stale one itself, and an ensure-side rm can delete a
# successor's fresh bind. Never kills a daemon it started: the daemon's own
# lock wait is shorter than this one. A backend it starts itself logs under
# the install to a file of its own, <prefix>/logs/sotd.<UTC start>Z-<pid>.log,
# never to a path another user can hold; the newest by name is the current one.
sot_daemon_ensure() {  # <prefix> <sotd-bin> <socket>
    SOT_ENSURE_NOTICE=""
    local prefix="$1" sotd_bin="$2" socket="$3" mode=nohup pid="" code="" start now warned=0
    local logdir="$1/logs" logfile="" stamp err
    # A logs folder that exists is owner-only whichever way the daemon starts: an install that ran nohup before and is
    # systemd-owned now still holds its old logs (unmasked secrets) there.
    [ ! -d "$logdir" ] || chmod 700 "$logdir" 2>/dev/null || printf 'WARNING: cannot secure %s\n' "$logdir" >&2
    if sot_socket_open "$sotd_bin" "$socket"; then
        sot_daemon_current "$prefix" "$sotd_bin" "$socket" && return 0
    fi
    if sot_service_owned "$prefix"; then
        mode=systemd
    else
        # The logs folder is owner-only, new or old: it keeps every log in it, past and future, from other accounts.
        { mkdir -p "$logdir" && chmod 700 "$logdir"; } || { printf 'ERROR: cannot create or secure %s\n' "$logdir" >&2; return 1; }
        sot_prune_logs "$logdir"
        # Milliseconds where date has %N (GNU); 000 where it does not (BSD).
        stamp="$(date -u +%Y%m%d-%H%M%S-%3N)"
        case "$stamp" in *-[0-9][0-9][0-9]) ;; *) stamp="$(date -u +%Y%m%d-%H%M%S)-000" ;; esac
        logfile="$logdir/sotd.${stamp}Z-$$.log"
        # Append, never truncate: the name is new, and a file another daemon
        # still writes is never cut short.
        nohup "$sotd_bin" --socket "$socket" --project-root "$HOME" --label sot >>"$logfile" 2>&1 </dev/null &
        pid=$!
        # Name the log for the daemon that writes it, so a prune keeps it while
        # that daemon lives; a rename never disturbs the open file. On failure
        # this shell's own pid in the old name protects it until the ensure ends.
        if err="$(mv -f -- "$logfile" "$logdir/sotd.${stamp}Z-$pid.log" 2>&1)"; then
            logfile="$logdir/sotd.${stamp}Z-$pid.log"
        else
            printf 'kept the log name %s: %s\n' "$logfile" "$err" >&2
        fi
    fi
    start="$(date +%s)"
    while :; do
        now="$(date +%s)"
        [ "$now" -lt $((start + SOT_LAUNCH_WAIT_S)) ] || break
        [ "$mode" != systemd ] || systemctl --user start sotd.service >/dev/null 2>&1
        sot_socket_open "$sotd_bin" "$socket" && return 0
        if [ "$mode" = nohup ] && ! kill -0 "$pid" 2>/dev/null; then
            wait "$pid" 2>/dev/null
            code=$?
            # Another daemon may have won the lock.
            sot_socket_open "$sotd_bin" "$socket" && return 0
            printf 'ERROR: the backend exited (%s) before opening %s; see %s\n' "$code" "$socket" "$logfile" >&2
            return 1
        fi
        if [ "$warned" = 0 ] && [ "$now" -ge $((start + 3)) ]; then
            echo "waiting for the backend (a previous one may still be shutting down)" >&2
            warned=1
        fi
        sleep 0.25
    done
    printf 'ERROR: the backend did not open %s within %ss\n' "$socket" "$SOT_LAUNCH_WAIT_S" >&2
    if [ "$mode" = systemd ]; then echo "see journalctl --user -u sotd.service" >&2
    else printf 'see %s\n' "$logfile" >&2; fi
    return 1
}

# The one copy helper: copy SRC to DST.new.<pid> (this shell's own, so two
# writers never share it), set MODE when one is given, and move it over DST, so
# DST only ever exists whole. Any failure removes the temp file and returns 1. sot-apply.sh and install.sh carry byte-identical copies
# (they copy before any checkout is known); a test pins all three equal.
sot_install_copy() {  # <src> <dst> [mode]
    if cp -p "$1" "$2.new.$$" && { [ -z "${3:-}" ] || chmod "$3" "$2.new.$$"; } && mv -f "$2.new.$$" "$2"; then
        return 0
    fi
    rm -f "${2:?}.new.$$"
    return 1
}

# Copy FILE to BACKUP, whole or not at all.
sot_backup() {  # <file> <backup>
    sot_install_copy "$1" "$2"
}

# True when the sot-launch wrapper is one of ours and embeds PREFIX.
sot_wrapper_owned() {  # <prefix>
    local wrapper="$HOME/.local/bin/sot-launch"
    [ -f "$wrapper" ] \
       && { grep -q '^# sot-launch: all-in-one$' "$wrapper" || grep -q '^start_daemon_if_needed()' "$wrapper"; } \
       && [ "$(sot_wrapper_owner_prefix < "$wrapper")" = "$1" ]
}

# Back up the unit and the wrapper this install owns, the files
# sot_rerender_owned rewrites, to UNIT-BAK and WRAP-BAK; returns 1 at the
# first that fails.
sot_backup_owned() {  # <prefix> <unit-bak> <wrap-bak>
    if sot_service_owned "$1"; then
        sot_backup "$HOME/.config/systemd/user/sotd.service" "$2" || return 1
    fi
    if sot_wrapper_owned "$1"; then
        sot_backup "$HOME/.local/bin/sot-launch" "$3" || return 1
    fi
    return 0
}

# Re-render, from CHECKOUT's own templates, the unit and the wrapper this
# install owns, so an update carries their text along with the binaries. The
# caller has already taken sot_backup_owned's backups of the same files.
sot_rerender_owned() {  # <prefix> <target> <checkout>
    local prefix="$1" target="$2" checkout="$3"
    local unit="$HOME/.config/systemd/user/sotd.service" wrapper="$HOME/.local/bin/sot-launch"
    if sot_service_owned "$prefix"; then
        render_sotd_unit "$prefix" "$checkout/deploy/sotd.service" "$unit" || return 1
        # The unit's host pin follows hosts.toml; with none it stays as the
        # install claimed it, since an update applied on another host sharing
        # the home must not move the daemon there. A failure keeps the pin it had.
        "$prefix/bin/sotd" topology pin --dir "$HOME/.config/systemd/user" >/dev/null \
            || echo "sot-apply: sotd topology pin failed (sotd.service keeps its previous host pin)" >&2
        # A unit an earlier install enabled (its link) or this one wired (the drop-in) is moved to the drop-in; with neither,
        # a person turned boot start off and an update must not turn it back on. A rollback keeps the drop-in.
        local ud="$HOME/.config/systemd/user"
        if [ -L "$ud/default.target.wants/sotd.service" ] || [ -f "$ud/default.target.d/sotd.conf" ]; then
            sot_boot_wants "$ud" || echo "sot-apply: could not write the sotd boot drop-in (the unit keeps its previous boot wiring)" >&2
        fi
        # The file on disk is right either way; the live check reads NeedDaemonReload.
        timeout 10 systemctl --user daemon-reload >/dev/null 2>&1 \
            || echo "sot-apply: systemctl --user daemon-reload failed (the unit file is current)" >&2
    fi
    if sot_wrapper_owned "$prefix"; then
        render_sot_launch "$prefix" "$target" "$wrapper" || return 1
    fi
    return 0
}
