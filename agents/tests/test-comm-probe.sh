#!/usr/bin/env bash
# test-comm-probe.sh — comm-probe.sh finds a probe row by the name it was created with until its responder joins,
# `up` returns 0 only once each row has joined, and the line `up` types is one the row's shell runs: `bash` with the
# script in a Unix login shell, and for a `pipe:` daemon's cmd.exe row Git for Windows' launcher by its Windows path
# with the script as its file argument (never `bash -c`, which execs twice and leaves the row rule no way to the
# capsule, PROTOCOL.md). The daemon is replaced by stubs in the shell that sources the staged script: `probe_request`
# answers from a JSON file of rows, and `sot_pty_input` records the typed line and, unless NOJOIN is set, joins the row
# as its responder would. A stand-in cygpath shows the Windows arm on every box; on a box with a native cygpath (Git
# Bash) the launcher the line names must also exist and run.
#
# Usage: agents/tests/test-comm-probe.sh     Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-probe-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi
trap 'rm -rf "${WORK:?}"' EXIT
export SOT_COMM_HOME="$WORK/home" SOT_SELF_HOST=testhost
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
mkdir -p "$SOT_COMM_HOME"

PASS=0
FAIL=0
ok()  { PASS=$((PASS + 1)); echo "PASS: $1"; }
bad() { FAIL=$((FAIL + 1)); echo "FAIL: $1"; }
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (got '$2', want '$3')"; fi; }
has() { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1 (no '$3' in: $2)" ;; esac; }

# The stubs, run by the bash on PATH (bash 3.2 under under-bash.sh). A create adds an unjoined row named for the
# handle; a typed line is recorded as `<row id> TAB <text>`.
export ROWS="$WORK/rows.json" TYPED="$WORK/typed"
cat > "$WORK/stubs.sh" <<'EOF'
probe_request() {
    local name
    case "$2" in
        workspace.list) jq -c '{payload: {workspaces: .}}' "$ROWS" ;;
        workspace.create)
            name="$(printf '%s' "$1" | jq -r .payload.agent_name)"
            jq --arg n "$name" '. + [{workspace_id: ("ws-" + $n), agent_name: $n, agent_handle: ""}]' "$ROWS" \
                > "$ROWS.new" && mv "$ROWS.new" "$ROWS"
            printf '{"payload":{"workspace_id":"ws-%s"}}\n' "$name" ;;
        *) return 1 ;;
    esac
}
sot_pty_input() {
    printf '%s\t%s\n' "$1" "$(printf '%s' "$2" | base64 --decode)" >> "$TYPED"
    if [ -z "${NOJOIN:-}" ]; then
        jq --arg w "$1" 'map(if .workspace_id == $w then .agent_handle = .agent_name else . end)' "$ROWS" \
            > "$ROWS.new" && mv "$ROWS.new" "$ROWS"
    fi
    printf '{"payload":{"ok":true}}\n'
}
EOF
# probe PATH ENDPOINT CODE — CODE's output (stdout and stderr) in a bash that sourced the staged comm-probe.sh and the
# stubs, with ENDPOINT set; RC holds its status.
probe() {
    OUT="$(PATH="$1" ENDPOINT="$2" bash -c '. "$1"; . "$2"; eval "$3"' _ "$SCRIPTS_DIR/comm-probe.sh" "$WORK/stubs.sh" "$3" 2>&1)"
    RC=$?
}
rows() { printf '%s\n' "$1" > "$ROWS"; : > "$TYPED"; }
typed() { cut -f"$1" "$TYPED" | tr '\n' ' ' | sed 's/ $//'; }
UNJOINED='{"workspace_id":"ws-1","agent_name":"probe-testhost","agent_handle":""}'
JOINED='{"workspace_id":"ws-2","agent_name":"probe-testhost","agent_handle":"probe-testhost"}'

# 1. A row is known by the name it was created with until its responder joins; one that declares the handle wins.
rows "[$UNJOINED]"; probe "$PATH" unix:x 'probe_row_id probe-testhost'
check "row id: a row whose responder never joined, by the name it was created with" "$OUT" ws-1
rows "[$JOINED]"; probe "$PATH" unix:x 'probe_row_id probe-testhost'
check "row id: a row that declares the handle" "$OUT" ws-2
rows "[$UNJOINED,$JOINED]"; probe "$PATH" unix:x 'probe_row_id probe-testhost'
check "row id: the row that declares the handle wins over one that only expects it" "$OUT" ws-2
rows '[{"workspace_id":"ws-3","agent_name":"other","agent_handle":"other"}]'; probe "$PATH" unix:x 'probe_row_id probe-testhost'
check "row id: no row for the handle" "$OUT" ""
rows "[$UNJOINED]"
SOT_COMM_SELF_FILE="$WORK/self/testhost__ws-1.txt" probe "$PATH" unix:x 'probe_self_handle'
check "serve: with no SOT_COMM_NAME, a responder before its join is named by its row's agent_name" "$OUT" probe-testhost

# 2. `up` starts a responder in each row and returns 0 only once each has joined.
rows '[]'; probe "$PATH" unix:x 'probe_up'
check "up: exits 0 once both rows have joined" "$RC" 0
has "up: says each row has joined" "$OUT" "@probe2-testhost has joined"
check "up (unix): types bash with the script into each new row" "$(typed 2)" \
    "bash '$SCRIPTS_DIR/comm-probe.sh' serve bash '$SCRIPTS_DIR/comm-probe.sh' serve"
rows "[$UNJOINED,"'{"workspace_id":"ws-4","agent_name":"probe2-testhost","agent_handle":""}]'
probe "$PATH" unix:x 'probe_up'
check "up: restarts the responder in each row that never joined, creating none" "$(typed 1)" "ws-1 ws-4"
check "up: rows that never joined, restarted, exit 0" "$RC" 0
rows '[]'; NOJOIN=1 SOT_PROBE_READY_WAIT=0 probe "$PATH" unix:x 'probe_up'
check "up: a responder that never joins fails up" "$RC" 1
has "up: the failure names the row and the handle" "$OUT" "row ws-probe-testhost did not join as @probe-testhost"

# 3. A pipe: daemon's row is cmd.exe: the line names Git for Windows' launcher by its Windows path and gives it the
# script as its file argument. The stand-in cygpath: `/` is $GITROOT, -w turns / into \, any other path as written.
mkdir -p "$WORK/win" "$WORK/git/bin" "$WORK/bare" "$WORK/git%pct/bin"
: > "$WORK/git/bin/bash.exe"; : > "$WORK/git%pct/bin/bash.exe"
cat > "$WORK/win/cygpath" <<'EOF'
#!/usr/bin/env bash
w=0
while [ "${1#-}" != "$1" ]; do case "$1" in -w) w=1 ;; -m|-l) ;; *) exit 2 ;; esac; shift; done
p="$1"; [ "$p" != / ] || p="$GITROOT/"
if [ "$w" = 1 ]; then printf '%s\n' "$p" | tr / '\\'; else printf '%s\n' "$p"; fi
EOF
chmod +x "$WORK/win/cygpath"
WIN_PATH="$WORK/win:$PATH"
PIPE='pipe:\\.\pipe\sot-probe-test'
rows '[]'; GITROOT="$WORK/git" probe "$WIN_PATH" "$PIPE" 'probe_up'
want="\"$(printf '%s' "$WORK/git/bin/bash.exe" | tr / '\\')\" \"$SCRIPTS_DIR/comm-probe.sh\" serve"
check "up (windows): types Git's launcher with the script as its file argument" "$(typed 2)" "$want $want"
check "up (windows): exits 0 once both rows have joined" "$RC" 0
rows '[]'; GITROOT="$WORK/bare" probe "$WIN_PATH" "$PIPE" 'probe_up'
check "up (windows): no launcher under Git's root: refused" "$RC" 1
check "up (windows): no launcher: nothing typed" "$(typed 2)" ""
has "up (windows): no launcher: the refusal names it" "$OUT" "no Git for Windows launcher"
rows '[]'; GITROOT="$WORK/git%pct" probe "$WIN_PATH" "$PIPE" 'probe_up'
check "up (windows): a launcher path cmd.exe would expand (%) is refused" "$RC:$(typed 2)" "1:"

# 4. On a box with Git Bash, the real launcher: the line names a bash.exe that exists and runs.
if command -v cygpath > /dev/null 2>&1; then
    probe "$PATH" "$PIPE" 'probe_start_line'
    launcher="${OUT#\"}"; launcher="${launcher%%\"*}"
    has "start line (native): the script is the launcher's file argument" "$OUT" "\" \"$(cygpath -m -l "$SCRIPTS_DIR/comm-probe.sh")\" serve"
    check "start line (native): the launcher is Git's bin\\bash.exe and runs" \
        "$(case "$launcher" in *'\bin\bash.exe') "$(cygpath -u "$launcher")" -c 'printf ran' 2>&1 ;; esac)" ran
else
    echo "SKIP: start line (native): this box has no cygpath"
fi

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
