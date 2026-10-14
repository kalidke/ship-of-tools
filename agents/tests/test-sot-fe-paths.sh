#!/usr/bin/env bash
# test-sot-fe-paths.sh — an absolute path a session gives `sot-fe preview` or `sot-nav.sh preview` is made relative to
# its root whatever its spelling. On Windows git prints its root as C:/..., a native daemon spells a row's root C:\...,
# and the shell spells a path /c/... or /tmp/..., so the two are compared in the one spelling a folder has on the box:
# comm-lib's sot_relative_to over sot_native_path, the library's one `cygpath -m -l`. Linux has no cygpath, and there the
# paths compare as written. Here a stand-in cygpath (and a git that prints its root through it, as Git for Windows
# does) shows the Windows arm; the real ones ran only in the tmp workflows' hosted steps (W7, W8, W9), and no CI job
# runs this suite on Windows. Sections 4 and 5 read what sot-fe sends from a recording socket in this suite's own work
# directory, never a real ~/.sot-comm.
#
# Usage: agents/tests/test-sot-fe-paths.sh     Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-fe-paths-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi
WORK="$(cd "$WORK" && pwd -P)" || exit 1   # git prints the physical root; a Mac's TMPDIR lies behind /var -> /private/var
trap 'rm -rf "${WORK:?}"' EXIT

export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
export SOTD_BIN="$(guard_bridge_stub "$WORK/bridge")"
[ -x "$SOTD_BIN" ] || exit 2
mkdir -p "$SOT_COMM_HOME"

PASS=0
FAIL=0
ok()  { PASS=$((PASS + 1)); echo "PASS: $1"; }
bad() { FAIL=$((FAIL + 1)); echo "FAIL: $1"; }
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (got '$2', want '$3')"; fi; }
has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1 (no '$3' in: $2)" ;; esac; }

# Git Bash's `cygpath -m [-l]` as this suite needs it: \ becomes /; /tmp/X is C:/SHORT~1/X (the mount, as an 8.3 short
# name, as on the hosted runner); /c/X is C:/X; any other /X is C:/G/X (Git's own folder); a C:/X path stays as
# written; -l makes SHORT~1 the long name TempLong. A git whose `--show-toplevel` prints its root long, through it, as
# Git for Windows does; and a realpath that keeps a C:/ path as written, as Git Bash's does (hosted run 38385277932).
WIN="$WORK/win"
mkdir -p "$WIN"
cat > "$WIN/cygpath" <<'EOF'
#!/usr/bin/env bash
long=0
while [ "${1#-}" != "$1" ]; do case "$1" in -l) long=1 ;; -m) ;; *) exit 2 ;; esac; shift; done
p="$(printf '%s' "$1" | tr '\\' /)"
case "$p" in
    /tmp|/tmp/*) p="C:/SHORT~1${p#/tmp}" ;;
    /[a-zA-Z]/*) p="$(printf '%s' "${p:1:1}" | tr a-z A-Z):${p:2}" ;;
    /*) p="C:/G$p" ;;
esac
[ "$long" = 0 ] || p="${p//SHORT~1/TempLong}"
printf '%s\n' "$p"
EOF
REAL_GIT="$(command -v git)"
cat > "$WIN/git" <<EOF
#!/usr/bin/env bash
out="\$("$REAL_GIT" "\$@")" || exit \$?
case " \$* " in *" --show-toplevel "*) cygpath -m -l "\$out" ;; *) printf '%s\n' "\$out" ;; esac
EOF
REAL_REALPATH="$(command -v realpath)"
cat > "$WIN/realpath" <<EOF
#!/usr/bin/env bash
for a in "\$@"; do p="\$a"; done
case "\$p" in [A-Za-z]:/*) printf '%s\n' "\$p" ;; *) exec "$REAL_REALPATH" "\$@" ;; esac
EOF
chmod +x "$WIN/cygpath" "$WIN/git" "$WIN/realpath"
WIN_PATH="$WIN:$PATH"
NATIVE_CYGPATH=0; command -v cygpath > /dev/null 2>&1 && NATIVE_CYGPATH=1

# lib PATH FN ARG... — FN's answer from the staged comm-lib.sh with PATH, or "rc N" when it fails.
lib() {
    local p="$1" out rc; shift
    out="$(PATH="$p" bash -c '. "$1"; shift; "$@"' _ "$SCRIPTS_DIR/comm-lib.sh" "$@" 2>/dev/null)"; rc=$?
    if [ "$rc" = 0 ]; then printf '%s' "$out"; else printf 'rc %s' "$rc"; fi
}

# 1. The rule, one table: ROOT, PATH and the answer, in each spelling.
while IFS='|' read -r arm root path want; do
    [ -n "$arm" ] || continue
    case "$arm" in
        linux) [ "$NATIVE_CYGPATH" = 0 ] || { echo "SKIP: relative_to $root $path: this box has a cygpath"; continue; }; p="$PATH" ;;
        windows) p="$WIN_PATH" ;;
    esac
    check "relative_to ($arm) '$root' '$path'" "$(lib "$p" sot_relative_to "$root" "$path")" "$want"
done <<'TABLE'
linux|/r/proj|/r/proj/a/b|a/b
linux|/r/proj|/r/proj|.
linux|/r/proj/|/r/proj/a|a
linux|/r/proj|/r/projx/a|rc 1
linux|/r/proj|/o/a|rc 1
linux||/r/proj/a|rc 1
linux|/r/proj|a/b|a/b
linux||a|a
windows|C:/r/proj|/c/r/proj/a/b|a/b
windows|C:/r/proj|C:\r\proj\a|a
windows|C:\r\proj|/c/r/proj|.
windows|C:/TempLong/x|/tmp/x/src/a.jl|src/a.jl
windows|C:/SHORT~1/x|C:/TempLong/x/a|a
windows|C:/r/proj|/c/r/projx/a|rc 1
windows|C:/r/proj|src/a|src/a
TABLE

# 2. sot_daemon_path keeps its contract on sot_native_path: a pipe: daemon gets cygpath -m -l's spelling, a failed or
# missing cygpath refuses, and any other daemon gets the path as written.
check "daemon_path: a pipe: daemon gets cygpath -m -l's spelling" \
    "$(ENDPOINT='pipe:\\.\pipe\x' lib "$WIN_PATH" sot_daemon_path /tmp/x)" "C:/TempLong/x"
check "daemon_path: a unix: daemon gets the path as written" \
    "$(ENDPOINT=unix:/s lib "$WIN_PATH" sot_daemon_path /tmp/x)" "/tmp/x"
if [ "$NATIVE_CYGPATH" = 0 ]; then
    check "daemon_path: a pipe: daemon with no cygpath refuses" "$(ENDPOINT='pipe:\\.\pipe\x' lib "$PATH" sot_daemon_path /tmp/x)" "rc 1"
else
    echo "SKIP: daemon_path with no cygpath: this box has one"
fi

# 2b. sot_same_root, the one way a registry or self-file root is compared: A, B and its answer ("" is rc 0).
while IFS='|' read -r arm a b want; do
    [ -n "$arm" ] || continue
    case "$arm" in
        linux) [ "$NATIVE_CYGPATH" = 0 ] || { echo "SKIP: same_root $a $b: this box has a cygpath"; continue; }; p="$PATH" ;;
        windows) p="$WIN_PATH" ;;
    esac
    check "same_root ($arm) '$a' '$b'" "$(lib "$p" sot_same_root "$a" "$b")" "$want"
done <<'TABLE'
linux|/r/proj|/r/proj|
linux|/r/proj|/r/other|rc 1
windows|C:/r/proj|/c/r/proj|
windows|C:\r\proj|/c/r/proj|
windows|C:/TempLong/x|/tmp/x|
windows|C:/r/proj|/c/r/other|rc 1
TABLE

# 2c. comm-context.sh, the self-file root compare (A3's site): a self file whose root is the shell's spelling of this
# repo (as comm-spawn records a root given as /tmp/...) is this project's, though comm-context spells its root as git
# does (C:/..., kept by realpath).
mkdir -p "$WORK/ctx" && (cd "$WORK/ctx" && "$REAL_GIT" init -q) || exit 2
printf 'ctx-handle\nrepo=ctx\nroot=%s\n' "$(cd "$WORK/ctx" && pwd -P)" > "$WORK/self-ctx.txt"
ctx="$(cd "$WORK/ctx" && PATH="$WIN_PATH" SOT_COMM_SELF_FILE="$WORK/self-ctx.txt" SOT_COMM_TEST_HOST=testhost \
    bash "$SCRIPTS_DIR/comm-context.sh" 2>"$WORK/ctx.err")"
check "context (windows): the self file's /tmp root is this repo's C:/ root" \
    "$(printf '%s\n' "$ctx" | sed -n 's/^NAME=//p') $(grep -c stale "$WORK/ctx.err")" "ctx-handle 0"

# 3. sot-nav.sh preview: the envelope's path, against a root in the shell's or the daemon's spelling.
nav() {  # PATH ROOT ARG — the envelope's path, or "rc N: <stderr>"
    local out rc
    out="$(PATH="$1" SOT_WORKSPACE=ws SOT_WORKSPACE_ROOT="$2" SOT_NAV_DRY_RUN=1 \
        bash "$SCRIPTS_DIR/sot-nav.sh" preview "$3" 2>"$WORK/nav.err")"; rc=$?
    if [ "$rc" = 0 ]; then printf '%s' "$out" | jq -r '.sot_ui.path'; else printf 'rc %s: %s' "$rc" "$(cat "$WORK/nav.err")"; fi
}
check "nav: an absolute path under the root" "$(nav "$PATH" /r/proj /r/proj/src/a.jl)" "src/a.jl"
check "nav: a relative path passes through" "$(nav "$PATH" /r/proj src/a.jl)" "src/a.jl"
check "nav: a path outside the root is refused" "$(nav "$PATH" /r/proj /o/a.jl)" \
    "rc 1: sot-nav: path '/o/a.jl' is not under workspace root '/r/proj'"
check "nav (windows): the daemon's C:\\ root and the shell's /c/ path" "$(nav "$WIN_PATH" 'C:\r\proj' /c/r/proj/src/a.jl)" "src/a.jl"
check "nav (windows): a C:\\ path under a C:\\ root" "$(nav "$WIN_PATH" 'C:\r\proj' 'C:\r\proj\src\a.jl')" "src/a.jl"
check "nav (windows): the root itself" "$(nav "$WIN_PATH" 'C:\r\proj' /c/r/proj)" "."

# 4. sot-fe preview: the path it sends, read by a socket that records what each connection sends until its request
# (the second line, after the hello) and then hangs up without an answer (for at most 300 s); or its refusal of a path
# outside the repo.
mkdir -p "$WORK/repo" "$WORK/elsewhere" && (cd "$WORK/repo" && "$REAL_GIT" init -q && : > a.txt) || exit 2
REC="$WORK/rec.sock" RECLOG="$WORK/rec.log"
python3 -c '
import os, signal, socket, sys, threading
signal.alarm(300)
s = socket.socket(socket.AF_UNIX)
s.bind(sys.argv[1])
s.listen(8)
def record(c):
    seen = b""
    while b"fe.command.send" not in seen and seen.count(b"\n") < 2:
        b = c.recv(65536)
        if not b:
            break
        seen += b
        fd = os.open(sys.argv[2], os.O_WRONLY | os.O_APPEND | os.O_CREAT)
        os.write(fd, b)
        os.close(fd)
    c.close()
while True:
    threading.Thread(target=record, args=(s.accept()[0],), daemon=True).start()
' "$REC" "$RECLOG" &
REC_PID=$!
trap 'kill "$REC_PID" 2>/dev/null; rm -rf "${WORK:?}"' EXIT
for _ in $(seq 1 50); do [ -S "$REC" ] && break; sleep 0.1; done
[ -S "$REC" ] || { echo "FATAL: the recording socket did not bind"; exit 2; }
REFUSED="is absolute and not under the current repo"
fe() {  # PATH ARG — the path sot-fe preview sends, or "refused" when it refuses ARG
    local err
    : > "$RECLOG"
    err="$(cd "$WORK/repo" && PATH="$1" bash "$SCRIPTS_DIR/sot-fe" preview ws "$2" \
        --endpoint "unix:$REC" --timeout 1 2>&1 >/dev/null)"
    case "$err" in *"$REFUSED"*) printf 'refused'; return 0 ;; esac
    jq -r 'select(.op == "fe.command.send") | .payload.args.path' "$RECLOG" 2>/dev/null | head -n 1
}
check "fe: a path under the repo is sent relative" "$(fe "$PATH" "$WORK/repo/a.txt")" "a.txt"
check "fe: a relative path is sent as written" "$(fe "$PATH" a.txt)" "a.txt"
check "fe: a path outside the repo is refused" "$(fe "$PATH" "$WORK/elsewhere/x")" "refused"
check "fe (windows): git's C:/ root and the shell's path" "$(fe "$WIN_PATH" "$WORK/repo/a.txt")" "a.txt"
win_file="$(PATH="$WIN_PATH" cygpath -m "$WORK/repo/a.txt" | tr / '\\')"
check "fe (windows): a C:\\ path under git's C:/ root" "$(fe "$WIN_PATH" "$win_file")" "a.txt"
check "fe (windows): a path outside the repo is refused" "$(fe "$WIN_PATH" "$WORK/elsewhere/x")" "refused"

# 5. A path sent to a daemon inside a JSON payload, where Git Bash converts nothing: a pipe: daemon (a native Windows
# process; this suite's pipe goes to the recording socket) gets cygpath -m -l's spelling, a unix: daemon the path as
# written. `sot-fe docs`, `sot-fe repl run` (with and without --fresh) and comm-probe's workspace.create. A --fresh run
# also carries no eval_id, so the daemon numbers it and announces it to every window, and it carries --origin.
export GUARD_PIPE_SOCKET="$REC"
PIPE='pipe:\\.\pipe\sot-paths-test'
sent() {  # OP JQ-PATH PATH CMD... — the value at JQ-PATH of the OP request CMD sends, run from the repo with PATH
    local op="$1" q="$2" p="$3"; shift 3
    : > "$RECLOG"
    (cd "$WORK/repo" && PATH="$p" "$@" > /dev/null 2>&1)
    jq -r --arg op "$op" "select(.op == \$op) | $q" "$RECLOG" 2>/dev/null | head -n 1
}
F="$WORK/repo/a.txt"
FW="$(PATH="$WIN_PATH" cygpath -m -l "$F")"
check "docs (windows): a pipe: daemon gets its spelling" \
    "$(sent fe.command.send .payload.args.path "$WIN_PATH" bash "$SCRIPTS_DIR/sot-fe" docs ws "$F" --endpoint "$PIPE" --timeout 1)" "$FW"
check "docs: a unix: daemon gets the path as written" \
    "$(sent fe.command.send .payload.args.path "$PATH" bash "$SCRIPTS_DIR/sot-fe" docs ws "$F" --endpoint "unix:$REC" --timeout 1)" "$F"
check "repl run --fresh (windows): a pipe: daemon gets its spelling" \
    "$(sent repl.run_file .payload.path "$WIN_PATH" bash "$SCRIPTS_DIR/sot-fe" repl run ws-x "$F" --fresh --endpoint "$PIPE" --timeout 1)" "$FW"
check "repl run --fresh numbers no run: the daemon numbers it and announces it to every window" \
    "$(sent repl.run_file '.payload | has("eval_id")' "$PATH" bash "$SCRIPTS_DIR/sot-fe" repl run ws-x "$F" --fresh --endpoint "unix:$REC" --timeout 1)" "false"
check "repl run --fresh: --origin labels the run" \
    "$(sent repl.run_file .payload.origin "$PATH" bash "$SCRIPTS_DIR/sot-fe" repl run ws-x "$F" --fresh --origin tester --endpoint "unix:$REC" --timeout 1)" "tester"
check "repl run (windows): a pipe: daemon gets its spelling" \
    "$(sent repl.execute .payload.input.path "$WIN_PATH" bash "$SCRIPTS_DIR/sot-fe" repl run ws-x "$F" --endpoint "$PIPE" --timeout 1)" "$FW"
check "repl run: a unix: daemon gets the path as written" \
    "$(sent repl.execute .payload.input.path "$PATH" bash "$SCRIPTS_DIR/sot-fe" repl run ws-x "$F" --endpoint "unix:$REC" --timeout 1)" "$F"
probe_root_sent() {  # PATH ENDPOINT — the project_root comm-probe's workspace.create carries
    PATH="$1" ENDPOINT="$2" bash -c 'log="$2"; . "$1"; probe_request() { printf "%s\n" "$1" > "$log"; return 1; }
        probe_create probe-t > /dev/null 2>&1; jq -r .payload.project_root "$log"' _ "$SCRIPTS_DIR/comm-probe.sh" "$WORK/probe-req"
}
check "probe (windows): a pipe: daemon gets its spelling" \
    "$(probe_root_sent "$WIN_PATH" "$PIPE")" "$(PATH="$WIN_PATH" cygpath -m -l "$SOT_COMM_HOME/probe/probe-t")"
check "probe: a unix: daemon gets the root as written" "$(probe_root_sent "$PATH" "unix:$REC")" "$SOT_COMM_HOME/probe/probe-t"

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
