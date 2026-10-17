#!/usr/bin/env bash
# in-container.sh [--user-manager] [--tmpdir-empty] UNIT -- CMD [ARG...] -- run CMD as the main process of the transient systemd --user
# service UNIT.service: the test container (Linux). Nothing a test starts leaves it: the job gets a private, empty
# XDG_RUNTIME_DIR and no DBUS_SESSION_BUS_ADDRESS, so nothing in it reaches the user manager, and the durable parent
# and every capsule row a test daemon starts stay in its control group (ADR 0043 decision 32's contained launch). When
# CMD ends, however it ends, systemd stops the unit and SIGKILLs every process left in it. --user-manager keeps the
# manager's address, for the tests that need it; their rows run in scopes of their own, outside the container.
# Foreground: output streams back; the exit status is CMD's when CMD exits, and a CMD killed by a signal gives 255. Stop it early: systemctl --user stop UNIT.service
# --tmpdir-empty: TMPDIR, the caller's fresh empty folder, must still be empty when CMD ends. If not, the job names what is
# left and exits 3, unless CMD already failed (its status stands). Opt-in: a job that does not promise an empty TMPDIR is
# not failed by one.
set -u
manager=0
tmp_empty=0
while :; do
    case "${1-}" in
        --user-manager) manager=1 ;;
        --tmpdir-empty) tmp_empty=1 ;;
        *) break ;;
    esac
    shift
done
[ $# -ge 3 ] && [ "$2" = -- ] || { echo "usage: in-container.sh [--user-manager] [--tmpdir-empty] UNIT -- CMD [ARG...]" >&2; exit 2; }
[ "$tmp_empty" = 0 ] || [ -n "${TMPDIR-}" ] || { echo "in-container.sh: --tmpdir-empty needs TMPDIR set" >&2; exit 2; }
unit=$1; shift 2
[[ $unit =~ ^[A-Za-z0-9-]+$ ]] || { echo "in-container.sh: bad UNIT $unit" >&2; exit 2; }
# Everything in the container ends with it, and at once: nothing in a test job needs a graceful stop.
props=(-p KillMode=control-group -p KillSignal=SIGKILL)
# A service starts from the manager's environment: the job's own values of these names go with it, nothing else.
envs=()
for n in PATH HOME LANG TMPDIR SSH_AUTH_SOCK CARGO_HOME RUSTUP_HOME CARGO_TARGET_DIR CARGO_PROFILE_DEV_DEBUG JULIA_DEPOT_PATH; do
    [ -n "${!n+x}" ] && envs+=("--setenv=$n=${!n}")
done
if [ "$manager" = 0 ]; then
    rt=$(mktemp -d /tmp/sot-job-XXXXXX) || exit 2
    props+=(-p "ExecStopPost=/bin/rm -rf $rt")
    set -- /usr/bin/env -u DBUS_SESSION_BUS_ADDRESS XDG_RUNTIME_DIR="$rt" "$@"
fi
# systemd expands $VAR and ${VAR} in a command line: doubled, each word reaches CMD as written.
set -- "${@//[\$]/\$\$}"
systemd-run --user --quiet --collect --wait --pipe --same-dir --unit="$unit.service" "${props[@]}" "${envs[@]}" -- "$@"
rc=$?
# ExecStopPost removes it when the unit ends; a failed start leaves it here. A client that returns while its unit
# still runs (it lost its bus) leaves the running job's folder alone: only a unit naming this folder keeps it.
[ "$manager" = 1 ] || systemctl --user show -p ExecStopPost --value "$unit.service" 2> /dev/null | grep -qF "$rt" || rm -rf "$rt"
if [ "$tmp_empty" = 1 ] && [ -n "$(ls -A "$TMPDIR")" ]; then
    echo "in-container.sh: TMPDIR $TMPDIR is not empty after the job:" >&2
    ls -A "$TMPDIR" >&2
    [ "$rc" = 0 ] && rc=3
fi
exit "$rc"
