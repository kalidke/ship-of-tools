#!/usr/bin/env bash
# installer-wrapper.sh -- the rendered all-in-one sot-launch (render_sot_launch) under the sandboxed PATH: the probe before
# every spawn, the crash-loop rollback, the exit-75 respawn, the service operations by unit ownership, the re-exec after
# an apply, and an armed pointer the installed sotd refuses (no stop, no restart, one sot-apply to drop it).
#
# Run: scripts/tests/installer-wrapper.sh

set -euo pipefail

# shellcheck source=installer-support.sh
. "$(dirname "$0")/installer-support.sh"

# ---------------------------------------------------------------------------
# Wrapper fixture: the rendered sot-launch under the sandboxed PATH. Event log
# lines: probe (the bridge), spawn (sot), rollback / apply (sot-apply), hop (a re-exec'd
# wrapper), plus the systemctl and pkill argv.
# <dir> <sot exit codes> <owned 1|0> <pending 1|0> <apply: rollback-only|consume>
mk_wrapper() {
    local d="$1" codes="$2" owned="$3" pending="$4" apply="${5:-log}" prefix="$1/prefix" lib
    lib="$prefix/repo/current/scripts/lib"
    mkdir -p "$d/stubs" "$d/home/.local/bin" "$d/home/.config/systemd/user" "$prefix/bin" "$prefix/updates" "$lib"
    mk_stubs "$d/stubs"
    cat > "$d/stubs/systemctl" <<'SC'
#!/bin/sh
printf 'systemctl %s\n' "$*" >> "$STUB_LOG"
exit 0
SC
    cat > "$d/stubs/pkill" <<'PK'
#!/bin/sh
echo pkill >> "$STUB_LOG"
exit 0
PK
    chmod +x "$d/stubs/systemctl" "$d/stubs/pkill"
    [ "$owned" = 1 ] || rm -f "${d:?}/stubs/systemctl"
    cp "$LIB" "$lib/sot-daemon.sh"
    printf '%s\n' "$codes" > "$d/codes"
    : > "$d/log"
    python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$d/sot.sock"
    cat > "$prefix/bin/sotd" <<SD
#!/bin/sh
case "\$1 \$2 \$3" in
    "stdio-bridge --endpoint unix:"*) echo probe >> "\$STUB_LOG"; exit 0 ;;
esac
[ "\$1 \$2" = "update pointer" ] && exit "\$(cat "$d/pointer-exit" 2>/dev/null || echo 0)"
[ "\$1" = session-socket-path ] && echo "$d/sot.sock"
exit 0
SD
    cat > "$prefix/bin/sot" <<'SOT'
#!/bin/sh
echo spawn >> "$STUB_LOG"
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
run_wrapper() {  # <dir>: sets WR_RC
    local d="$1"
    WR_RC=0
    ( HOME="$d/home" PATH="$d/stubs:$TOOLS" STUB_LOG="$d/log" "$d/home/.local/bin/sot-launch" ) >/dev/null 2>&1 || WR_RC=$?
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
run_wrapper "$d"
check "an owned unit: apply is a try-restart, no sot-apply, no pkill" \
    "1 0 0" "$(grep -c -- '--user try-restart sotd.service' "$d/log" || true) $(grep -c '^apply$' "$d/log" || true) $(grep -c '^pkill$' "$d/log" || true)"
d="$WORK/w-own-rb"; mk_wrapper "$d" "$(printf '1\n1\n0')" 1 0
run_wrapper "$d"
check "an owned unit: rollback stops it through systemd" \
    "1 0" "$(grep -c -- '--user stop sotd.service' "$d/log" || true) $(grep -c '^pkill$' "$d/log" || true)"
d="$WORK/w-for-apply"; mk_wrapper "$d" "0" 0 1
printf '#!/bin/sh\nexit 0\n' > "$d/stubs/systemctl"; chmod +x "$d/stubs/systemctl"
run_wrapper "$d"
check "a foreign unit: apply is pkill plus sot-apply, no try-restart" \
    "0 1 1" "$(grep -c 'try-restart' "$d/log" || true) $(grep -c '^apply$' "$d/log" || true) $(grep -c '^pkill$' "$d/log" || true)"
d="$WORK/w-for-rb"; mk_wrapper "$d" "$(printf '1\n1\n0')" 0 0
printf '#!/bin/sh\nprintf "systemctl %%s\\n" "$*" >> "$STUB_LOG"\nexit 0\n' > "$d/stubs/systemctl"; chmod +x "$d/stubs/systemctl"
run_wrapper "$d"
check "a foreign unit: rollback is pkill, no systemd stop" \
    "0 1" "$(grep -c 'sotd.service' "$d/log" || true) $(grep -c '^pkill$' "$d/log" || true)"
# A pointer the installed sotd refuses (exit 3) costs no restart: sot-apply runs once to drop it and nothing stops or
# restarts the daemon, and sot-apply runs in its drop-only mode: a pointer that fits and was armed in between waits for the
# next start, when the daemon is stopped as usual. Exit 1, an older sotd's answer to the verb, leaves the restart in charge.
refused() {  # <name> <owned> <sotd exit>: the log's restart/stop, pkill, full-apply and drop-only counts
    local d="$WORK/w-$1"; mk_wrapper "$d" "0" "$2" 1; echo "$3" > "$d/pointer-exit"; run_wrapper "$d"
    echo "$(grep -c -e try-restart -e '--user stop' "$d/log" || true) $(grep -c '^pkill$' "$d/log" || true) $(grep -c '^apply$' "$d/log" || true) $(grep -c '^drop$' "$d/log" || true)"
}
check "an owned unit, pointer refused: no restart, one drop-only sot-apply" "0 0 0 1" "$(refused r-owned 1 3)"
check "a foreign unit, pointer refused: no pkill, one drop-only sot-apply" "0 0 0 1" "$(refused r-foreign 0 3)"
check "an older sotd's answer (exit 1) leaves the restart in charge" "1 0 0 0" "$(refused r-older 1 1)"
# A real foreign unit, as sot_service_owned defines one: systemctl is here and
# the manifest says systemd, but the unit runs another prefix's sotd.
foreign_unit() {  # <dir>
    printf '{"service": "systemd"}\n' > "$1/prefix/install.json"
    render_sotd_unit "$WORK/elsewhere" "$(dirname "$0")/../../deploy/sotd.service" "$1/home/.config/systemd/user/sotd.service"
    printf '#!/bin/sh\nprintf "systemctl %%s\\n" "$*" >> "$STUB_LOG"\nexit 0\n' > "$1/stubs/systemctl"; chmod +x "$1/stubs/systemctl"
}
d="$WORK/w-real-for-apply"; mk_wrapper "$d" "0" 0 1; foreign_unit "$d"
run_wrapper "$d"
check "a real foreign unit: apply runs no service op, only pkill plus sot-apply" \
    "0 1 1" "$(grep -c 'sotd.service' "$d/log" || true) $(grep -c '^apply$' "$d/log" || true) $(grep -c '^pkill$' "$d/log" || true)"
d="$WORK/w-real-for-rb"; mk_wrapper "$d" "$(printf '1\n1\n0')" 0 0; foreign_unit "$d"
run_wrapper "$d"
check "a real foreign unit: rollback runs no service op, only pkill" \
    "0 1" "$(grep -c 'sotd.service' "$d/log" || true) $(grep -c '^pkill$' "$d/log" || true)"

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

# shellcheck source=installer-julia-step.sh
. "$(dirname "$0")/installer-julia-step.sh"
# shellcheck source=installer-julia-floor.sh
. "$(dirname "$0")/installer-julia-floor.sh"

# ---------------------------------------------------------------------------
printf '\n'
if [ "$fails" -eq 0 ]; then
    printf 'installer-wrapper: all checks passed\n'
else
    printf 'installer-wrapper: %d check(s) FAILED\n' "$fails" >&2
    exit 1
fi
