#!/usr/bin/env bash
# installer-support.sh -- shared setup for installer-state.sh and installer-apply.sh: sources install.sh and
# lib/sot-daemon.sh, the check helpers, and the sandboxed tool dir and recording stubs. Sourced, never run.

SOT_INSTALL_SOURCE_ONLY=1
export SOT_INSTALL_SOURCE_ONLY
# shellcheck source=../install.sh
. "$(dirname "$0")/../install.sh"
# shellcheck source=../lib/sot-daemon.sh
. "$(dirname "$0")/../lib/sot-daemon.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK:?}"' EXIT
fails=0

check() {  # <description> <expected> <actual>
    if [ "$2" = "$3" ]; then
        printf '    ok   %s\n' "$1"
    else
        printf '    FAIL %s\n      expected: %s\n      actual:   %s\n' "$1" "$2" "$3"
        fails=$((fails + 1))
    fi
}
starts_with() {  # <description> <prefix> <actual>
    case "$3" in
        "$2"*) printf '    ok   %s\n' "$1" ;;
        *) printf '    FAIL %s\n      expected prefix: %s\n      actual:          %s\n' "$1" "$2" "$3"
           fails=$((fails + 1)) ;;
    esac
}
case_start() { printf '  %s\n' "$1"; }

# New tests never see the host's /usr/bin: PATH is a recording-stub dir plus a
# dir of symlinks to exactly the tools the library needs.
mk_tools() {  # <dir>
    mkdir -p "$1"
    local t p
    for t in bash sh env cat sed grep head cut awk mkdir rm mv cp ln chmod touch readlink basename dirname date sleep nohup id uname hostname python3 git sha256sum install timeout stat cmp mktemp find wc; do
        p="$(command -v "$t")" || { printf 'FAIL mk_tools: %s is missing\n' "$t" >&2; exit 1; }
        ln -sf "$p" "$1/$t"
    done
}
TOOLS="$WORK/tools"; mk_tools "$TOOLS"
LIB="$(dirname "$0")/../lib/sot-daemon.sh"

# Recording stubs. The sotd stub binds the --socket it is given after
# STUB_DELAY seconds (or exits STUB_EXIT without binding), then stays up.
mk_stubs() {  # <dir>
    mkdir -p "$1"
    cat > "$1/systemctl" <<'SC'
#!/bin/sh
printf '%s\n' "$*" >> "$STUB_LOG"
case "$*" in
    *"start sotd.service"*)
        python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$STUB_SOCKET" ;;
esac
exit 0
SC
    cat > "$1/sotd" <<'SD'
#!/bin/sh
case "$1 $2 $3" in
    "stdio-bridge --endpoint unix:"*) exit 0 ;;
esac
printf 'sotd %s\n' "$*" >> "$STUB_LOG"
printf '%s\n' "$$" > "$STUB_PIDFILE"
[ -z "${STUB_EXIT:-}" ] || exit "$STUB_EXIT"
while [ $# -gt 0 ] && [ "$1" != --socket ]; do shift; done
[ $# -gt 0 ] || exit 4
exec python3 -c 'import socket,sys,time; time.sleep(float(sys.argv[2])); socket.socket(socket.AF_UNIX).bind(sys.argv[1]); time.sleep(60)' "$2" "${STUB_DELAY:-0}"
SD
    chmod +x "$1/systemctl" "$1/sotd"
}
