#!/usr/bin/env bash
# installer-support.sh -- shared setup for installer-state.sh, installer-wrapper.sh and installer-apply.sh: sources install.sh and
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
# dir of symlinks to exactly the tools the library needs, and any more a test's script needs.
mk_tools() {  # <dir> [tool...]
    local d="$1" t p
    shift
    mkdir -p "$d"
    for t in bash sh env cat sed grep head cut awk mkdir rm mv cp ln chmod touch readlink basename dirname date sleep nohup id uname hostname python3 git install stat cmp mktemp find wc "$@"; do
        p="$(command -v "$t")" || { printf 'FAIL mk_tools: %s is missing\n' "$t" >&2; exit 1; }
        ln -sf "$p" "$d/$t"
    done
    # install.sh takes sha256sum or shasum, and calls timeout only beside systemctl (macOS has shasum, not sha256sum, and no timeout).
    for t in sha256sum shasum timeout; do
        p="$(command -v "$t")" && ln -sf "$p" "$d/$t"
    done
    [ -e "$d/sha256sum" ] || [ -e "$d/shasum" ] || { printf 'FAIL mk_tools: sha256sum or shasum is missing\n' >&2; exit 1; }
}
TOOLS="$WORK/tools"; mk_tools "$TOOLS"

# host_target — the release target install.sh and sot-apply.sh name this box by.
host_target() { case "$(uname -s)-$(uname -m)" in Darwin-arm64) echo macos-aarch64 ;; *) echo linux-x86_64 ;; esac; }
TGT="$(host_target)"
# sha256_of FILE... — `<hash>  <file>` lines, by sha256sum or macOS's shasum.
sha256_of() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi; }
# tree_of DIR... — every entry with its type (f, d, l or o) and every file's hash, sorted: what a refused install must
# leave as it was, without GNU find's -printf.
tree_of() {
    local p t
    find "$@" 2>/dev/null | LC_ALL=C sort | while IFS= read -r p; do
        if [ -L "$p" ]; then t=l; elif [ -d "$p" ]; then t=d; elif [ -f "$p" ]; then t=f; else t=o; fi
        printf '%s %s\n' "$t" "$p"
        [ "$t" != f ] || sha256_of "$p"
    done
}
# bsd_stat_date DIR — stand-ins for BSD's (macOS's) stat and date, on Linux: they refuse GNU's -c and -d, and answer
# -f %m and -r through GNU's.
bsd_stat_date() {
    mkdir -p "$1"
    printf '#!/bin/sh\n[ "$1" = -f ] && [ "$2" = %%m ] && exec %s -c %%Y "$3"\necho "stat: illegal option -- $1" >&2; exit 1\n' \
        "$(command -v stat)" > "$1/stat"
    printf '#!/bin/sh\ncase "$1" in -d) echo "date: illegal option -- d" >&2; exit 1 ;; -r) s="$2"; shift 2; exec %s -d "@$s" "$@" ;; esac\nexec %s "$@"\n' \
        "$(command -v date)" "$(command -v date)" > "$1/date"
    chmod +x "$1/stat" "$1/date"
}
# stale_line LINE — LINE with each printed time (YYYY-MM-DD HH:MM:SS) as D.
stale_line() { printf '%s\n' "$1" | sed -E 's/[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2}/D/g'; }
# linux_check DESC EXPECTED ACTUAL — check on Linux; elsewhere a SKIP line. A systemd unit is Linux's: install.sh and
# sot-apply.sh run systemctl, bounded by GNU timeout, only for a unit they own, which a Mac never has.
linux_check() {
    if [ "$(uname -s)" = Linux ]; then check "$@"; else printf '    SKIP %s (a systemd unit: Linux only)\n' "$1"; fi
}
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
    "log-path  ") printf '%s\n' "${STUB_DAEMON_LOG:-}"; exit 0 ;;
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
# events DIR -- the stand-ins' event log of a launcher case (wrapper or dev launcher), on one line.
events() { grep -E '^(probe|spawn|rollback|hop|started|fe )' "$1/log" | tr '\n' ' ' | sed 's/ $//'; }
