#!/usr/bin/env bash
# test-agent-layers.sh — a process acts as a comm handle only if at most one
# agent lies between it and its row's capsule (or the top of its process tree
# outside a row). A second agent started inside a session (`codex exec`,
# `claude -p`), and anything it starts, has no comm identity: it never reads
# the inbox, sends, joins, stamps, blocks the Stop hook or refreshes the
# heartbeat as the row's handle.
#
#   1. TABLE: _sot_agent_layers counts agent layers in a process chain
#      (comm-lib.sh), over the shapes the rule has to get right.
#   2. END TO END: fake processes, named by argv[0] only (`exec -a`), stand in
#      for a row's capsule and agents, so the result does not depend on what
#      runs this suite. Own first (one agent, no agent, an npm codex that is two
#      processes), then the child (a second agent between the first and the
#      tool shell).
#
# A process whose identity names a row (a self file `<host>__<id>.txt`, or
# SOT_WORKSPACE_ID with no self file) must also reach THAT row's capsule: the top,
# another row's capsule and a capsule with an unreadable command line are refused
# (rc 2); a process that names no row passes at the top and at any capsule. The
# row cases run beneath `in_row` (lib-home-guard.sh), a capsule stand-in.
#
# HERMETIC: a temp $SOT_COMM_HOME, a pinned $SOT_COMM_TEST_HOST, per-handle
# $SOT_COMM_SELF_FILEs — never the real ~/.sot-comm.
#
# Usage: comm/core/tests/test-agent-layers.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SCRIPT_DIR/../scripts" && pwd)"
HOOKS_DIR="$(cd "$SCRIPT_DIR/../../adapters/claude/hooks" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-agent-layers-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
export SOT_COMM_HOME="$WORK/home"
guard_fresh_home "$WORK"; guard_refuse_live_home "$SOT_COMM_HOME"
mkdir -p "$SOT_COMM_HOME"
trap 'rm -rf "${WORK:?}"' EXIT
# Run against a copy of the scripts that can find no daemon and no hub, so a
# setup send can never reach a live one (the test-hub-files.sh seam); the
# append is checked, since a read-only source mode would silently skip it.
cp -r "$SCRIPTS_DIR" "$WORK/scripts" && chmod -R u+w "$WORK/scripts" || { echo "FATAL: cannot copy the scripts" >&2; exit 1; }
SCRIPTS_DIR="$WORK/scripts"
cat >> "$SCRIPTS_DIR/comm-lib.sh" <<'STUB'

# ---- no daemon, no hub (test only) ------------------------------------------
sot_daemon_endpoint() { return 1; }
sot_relay_endpoint() { return 1; }
STUB
grep -q '^sot_relay_endpoint() { return 1; }$' "$SCRIPTS_DIR/comm-lib.sh" || { echo "FATAL: the no-daemon stub did not land in the copy" >&2; exit 1; }
cat >> "$SCRIPTS_DIR/comm-lib.sh" <<'SIMWIN'

# ---- a Windows host on Linux (test only, only while SIMWIN is set) ----------
# Windows pids are Linux pids; a process is native when its argv[0] ends in .exe,
# else MSYS. Cygwin's /proc, this process and its MSYS ancestors (ppid 1 under a
# native parent), is built here, when the library is sourced; $SIMWIN/sotd walks
# the native part.
if [ -n "${SIMWIN:-}" ]; then
    _sot_is_windows() { return 0; }
    SOTD_BIN="$SIMWIN/sotd"
    _SOT_PROC="$SIMWIN/proc.$$"
    rm -rf "${_SOT_PROC:?}"; mkdir -p "$_SOT_PROC"
    _sim_p=$$
    while [ "$_sim_p" -gt 1 ] && IFS= read -r _sim_l < "/proc/$_sim_p/stat" 2>/dev/null; do
        _sim_l="${_sim_l##*) }"; _sim_l="${_sim_l#* }"; _sim_pp="${_sim_l%% *}"
        _sim_a0=""; IFS= read -r -d '' _sim_a0 < "/proc/$_sim_p/cmdline" 2>/dev/null || :
        _sim_pa0=""; [ "$_sim_pp" -le 1 ] || { IFS= read -r -d '' _sim_pa0 < "/proc/$_sim_pp/cmdline" 2>/dev/null || :; }
        case "$_sim_a0" in
            *.exe) ;;
            *) mkdir -p "$_SOT_PROC/$_sim_p"
               ln -sf "/proc/$_sim_p/cmdline" "$_SOT_PROC/$_sim_p/cmdline"
               echo "$_sim_p" > "$_SOT_PROC/$_sim_p/winpid"
               case "$_sim_pa0" in *.exe|'') _sim_q=1 ;; *) _sim_q="$_sim_pp" ;; esac
               printf '%s (sim) S %s\n' "$_sim_p" "$_sim_q" > "$_SOT_PROC/$_sim_p/stat" ;;
        esac
        _sim_p="$_sim_pp"
    done
fi
SIMWIN
JOIN="$SCRIPTS_DIR/comm-join.sh"; POLL="$SCRIPTS_DIR/comm-poll.sh"
SEND="$SCRIPTS_DIR/comm-send.sh"; STATUS="$SCRIPTS_DIR/comm-status.sh"
ln -s "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
# A fake findmnt first on PATH reports a local filesystem, so the inbox lock is
# this box's own and every filing is local, whatever $WORK sits on and with no
# daemon involved (the same seam as test-relay-file-first.sh).
mkdir -p "$WORK/findmnt-bin" "$SOT_COMM_HOME/inbox"
printf '#!/bin/sh\necho "ext4 rw,relatime /dev/fake"\n' > "$WORK/findmnt-bin/findmnt"
chmod +x "$WORK/findmnt-bin/findmnt"
export PATH="$WORK/findmnt-bin:$PATH"
bash -c 'source "$1"; sot_inbox_lock_identity "$INBOX_DIR"' _ "$SCRIPTS_DIR/comm-lib.sh" > "$SOT_COMM_HOME/inbox-lock-manager"
export SOT_COMM_TEST_HOST="testhost"
unset SOT_COMM_NAME COMM_STATUS_ORIGIN CLAUDE_CODE_SESSION_ID SOT_WORKSPACE_ID SOT_COMM_SELF_FILE SOT_COMM_HOOKS

PASS=0; FAIL=0
ok()  { echo "PASS: $1"; PASS=$((PASS + 1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL + 1)); }
has()   { case "$2" in *"$3"*) ok "$1" ;; *) bad "$1 (no '$3' in: $2)" ;; esac; }
hasnt() { case "$2" in *"$3"*) bad "$1 (found '$3' in: $2)" ;; *) ok "$1" ;; esac; }
eq()    { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (got '$2', want '$3')"; fi; }

# --- 0. the guard closed daemon discovery (lib-home-guard.sh, its self-test passed) ---
eq  "guard: sotd resolves to the refusing stub" "$(command -v sotd)" "$_GUARD_STUBS/sotd"
gout="$(sotd probe 2>&1)"; grc=$?
eq  "guard: the sotd stub exits 97" "$grc" 97
has "guard: the sotd stub says it refused" "$gout" "refused daemon discovery"
eq  "guard: pgrep resolves to the refusing stub" "$(command -v pgrep)" "$_GUARD_STUBS/pgrep"
for gs in powershell.exe pwsh pwsh.exe; do
    eq  "guard: $gs resolves to the refusing stub" "$(command -v "$gs")" "$_GUARD_STUBS/$gs"
    gout="$("$gs" -NoProfile 2>&1)"; grc=$?
    eq  "guard: the $gs stub exits 97" "$grc" 97
    has "guard: the $gs stub says it refused" "$gout" "refused daemon discovery"
done
case "${LOCALAPPDATA:-}" in "$WORK/guard/"*) ok "guard: LOCALAPPDATA is under the guard directory" ;; *) bad "guard: LOCALAPPDATA is under the guard directory (got '${LOCALAPPDATA:-}')" ;; esac
gout="$( . "$SCRIPT_DIR/../scripts/comm-lib.sh" >/dev/null 2>&1; _sot_windows_local_pipe 2>/dev/null )"; grc=$?
if [ "$grc" -ne 0 ] && [ -z "$gout" ]; then ok "guard: _sot_windows_local_pipe finds no pipe (on every host)"; else bad "guard: _sot_windows_local_pipe finds no pipe (rc=$grc out='$gout')"; fi
gout="$( . "$SCRIPT_DIR/../scripts/comm-lib.sh" >/dev/null 2>&1; sot_daemon_endpoint 2>/dev/null; sot_relay_endpoint 2>/dev/null )"
eq  "guard: the tree's own comm-lib finds no daemon and no hub" "$gout" ""

# --- 1. the table -------------------------------------------------------------
# shellcheck source=../scripts/comm-lib.sh
source "$SCRIPTS_DIR/comm-lib.sh"
# A chain is one process per argument, caller first; the count is the number of
# layers _sot_agent_layers prints.
# A record is one process's argv, fields joined by US (comm-lib.sh reads
# /proc/<pid>/cmdline NUL-separated, so a space inside an argument survives).
# tbl splits each argument at its spaces; tblu takes fields joined by `|`, for
# an argument that holds a space. Both end the chain with `!end`, as a walk to the
# top does, and count the names before the filter's closing `!ok`.
US=$'\037'
tbl() {  # WANT DESC LINE...
    local want="$1" desc="$2" got l recs=(); shift 2
    for l in "$@"; do recs+=("${l// /$US}"); done
    got="$(printf '%s\n' "${recs[@]}" '!end' | _sot_agent_layers 2>/dev/null | awk 'NF && $0 != "!ok"' | wc -l | tr -d ' ')"
    eq "table: $desc" "$got" "$want"
}
tblu() {  # WANT DESC RECORD...   (fields joined by |)
    local want="$1" desc="$2" got l recs=(); shift 2
    for l in "$@"; do recs+=("${l//|/$US}"); done
    got="$(printf '%s\n' "${recs[@]}" '!end' | _sot_agent_layers 2>/dev/null | awk 'NF && $0 != "!ok"' | wc -l | tr -d ' ')"
    eq "table: $desc" "$got" "$want"
}
# req WANT_RC DESC TEXT RECORD... : sot_require_agent over a stubbed chain
# (fields joined by |); its status and one-line reason.
req() {
    local want="$1" desc="$2" text="$3" res; shift 3; CHAIN=("$@")
    res="$( _sot_ancestor_chain() { local r; [ "${#CHAIN[@]}" -gt 0 ] || return 1; for r in "${CHAIN[@]}"; do printf '%s\n' "${r//|/$US}"; done
                                    case "$r" in '!'*) ;; *) echo '!end' ;; esac; }
            o="$(sot_require_agent)"; echo "rc=$?|$o" )"
    case "$res" in "rc=$want|"*"$text"*) ok "require: $desc" ;; *) bad "require: $desc (want rc=$want and '$text', got: $res)" ;; esac
}
tbl 1 "native claude under the capsule"          bash "/h/.local/bin/claude --x" "sot-capsule run"
tbl 2 "codex exec under a claude row (the repro)" bash "bash -lc p" "/v/codex/codex exec" "node /n/bin/codex exec" "/bin/bash -c s" "/h/.local/bin/claude" "sot-capsule run"
tbl 1 "npm codex is a wrapper and its binary"     bash "/v/codex/codex" "node /n/bin/codex" "sot-capsule run"
tbl 1 "julia between is not an agent"             bash "julia s.jl" "bash -c s" claude "sot-capsule run"
tbl 1 "python and make between, no capsule"       bash "python3 x.py" make bash claude
tbl 0 "a bash row has no agent"                   bash -bash "sot-capsule run"
tbl 2 "claude -p under a claude row"              bash "claude -p" bash claude "sot-capsule run"
tbl 2 "two native codex"                          bash codex codex
tbl 1 "walk reaches the top outside a row"        bash claude bash tmux systemd
tbl 1 "the walk stops at the capsule"             bash claude "sot-capsule run" bash claude
tbl 2 "windows exe names"                         bash.exe codex.exe node.exe bash.exe claude.exe sot-capsule.exe
tbl 1 "a node script is not an agent by its directory alone" bash "node /x/claude-code/cli.js" claude
tblu 2 "an agent path with spaces"                 bash "/tmp/agent dir/codex|exec" "/tmp/agent dir/claude" "sot-capsule|run"
tblu 2 "node, an option, then an agent script"     bash "node|--no-warnings|/n/bin/codex.js|exec" claude "sot-capsule|run"
tblu 2 "node running a script with a space in its path" bash "node|/tmp/x y/codex.js" claude "sot-capsule|run"
tblu 2 "node running the npm codex package"        bash "node|/n/node_modules/@openai/codex/bin/run.js" claude "sot-capsule|run"
tblu 2 "node running the npm claude package"       bash "node|/n/node_modules/@anthropic-ai/claude-code/cli.js" codex "sot-capsule|run"
tblu 2 "node running a package, windows separators" bash.exe 'node.exe|C:\n\node_modules\@openai\codex\bin\run.js' claude.exe sot-capsule.exe
tblu 1 "node with only options is not an agent"    bash "node|--inspect" claude "sot-capsule|run"
tblu 1 "npm codex over its native child, by package" bash "/v/codex/codex" "node|/n/node_modules/@openai/codex/bin/run.js" "sot-capsule|run"
tblu 2 "node after a native agent of a different name" bash "/v/claude" "node|/n/bin/codex.js" "sot-capsule|run"
tblu 1 "node after its own native agent, same name" bash codex "node|/n/bin/codex.js" "sot-capsule|run"
tblu 2 "an upper-case windows agent name"           bash.exe CLAUDE.EXE codex.exe sot-capsule.exe
tblu 2 "node --require x.cjs, then the npm claude package" bash "node|--require|/tmp/x.cjs|/n/node_modules/@anthropic-ai/claude-code/cli.js" codex "sot-capsule|run"
tblu 2 "node --require x.cjs, then an agent script"  bash "node|--require|/tmp/x.cjs|/n/bin/codex.js|exec" claude "sot-capsule|run"
tbl  2 "node /x y/codex.js as split tokens (ps)"    bash "node /x y/codex.js" claude "sot-capsule run"
tbl  2 "a package path with a space, split (ps)"    bash "node /tmp/agent dir/node_modules/@anthropic-ai/claude-code/cli.js" codex "sot-capsule run"
tbl  2 "a package path with a space, split inside the agent basename (ps)" bash "node /tmp/a b/codex.mjs" claude "sot-capsule run"
tblu 1 "node whose arguments name no agent"         bash "node|--require|/tmp/x.cjs|/n/build.js" claude "sot-capsule|run"

# --- 1b. an unreadable ancestry is its own refusal (rc 2), not a child (rc 1) ----
R_TREE_TEXT="cannot read this process's ancestry"
req 0 "one agent, the walk reaches the capsule"      "" bash claude "sot-capsule|run"
req 1 "two agents is a child"                        "has no comm identity" bash codex bash claude "sot-capsule|run"
req 2 "a truncated record before the capsule"        "$R_TREE_TEXT" bash codex '!truncated'
req 2 "a truncated record hides the outer agent"     "$R_TREE_TEXT" bash claude bash '!truncated'
req 0 "a capsule reached before any truncation"      "" bash claude "sot-capsule|run" '!truncated'
req 2 "no chain at all"                              "$R_TREE_TEXT"

# A parse that does not finish is refused too: a filter that dies with no output
# (no awk), and a sotd.exe whose exit status is not 0 or 3.
mkdir -p "$WORK/badawk"; printf '#!/bin/sh\nexit 1\n' > "$WORK/badawk/awk"; chmod +x "$WORK/badawk/awk"
res="$( PATH="$WORK/badawk:$PATH"; o="$(sot_require_agent)"; echo "rc=$?|$o" )"
case "$res" in "rc=2|"*"$R_TREE_TEXT"*) ok "require: an awk that exits 1 with no output is refused" ;; *) bad "require: an awk that exits 1 with no output is refused (got: $res)" ;; esac
# wtbl WANT DESC LINE... : the layers a Windows chain counts, through the records filter
# (each LINE is `<exe><TAB><command line>`, caller first, the capsule last).
wtbl() {
    local want="$1" desc="$2" got; shift 2
    got="$( { printf '%s\n' "$@" | awk -v OFS='\t' '{ print NR, $0 }' | _sot_win_records && echo '!end'; } | _sot_agent_layers 2>/dev/null | awk 'NF && $0 != "!ok"' | wc -l | tr -d ' ')"
    eq "windows table: $desc" "$got" "$want"
}
# The Windows walk on a "Windows" host: $BR stands in for Cygwin's /proc, and the stub
# sotd answers `ancestors --from W` with $BR/sotd.W (one `pid<TAB>exe<TAB>command line`
# per line) and exits with $BR/sotd.W.rc; anything else exits 2.
BR="$WORK/bridge"
printf '#!/bin/sh\n[ "$#" = 3 ] && [ "$1" = ancestors ] && [ "$2" = --from ] && [ -f "%s/sotd.$3" ] || exit 2\ncat "%s/sotd.$3"\nexit "$(cat "%s/sotd.$3.rc")"\n' \
    "$BR" "$BR" "$BR" > "$WORK/sotd-bridge"; chmod +x "$WORK/sotd-bridge"
bproc() {  # PID PPID WINPID ARGV... : one Cygwin process in $BR
    local p="$1" pp="$2" w="$3"; shift 3
    mkdir -p "$BR/$p"; printf '%s (x) S %s 0 0\n' "$p" "$pp" > "$BR/$p/stat"; printf '%s\0' "$@" > "$BR/$p/cmdline"; echo "$w" > "$BR/$p/winpid"
}
bsotd() {  # FROM RC LINE... : the stub's answer to --from FROM, and its status
    local f="$1" rc="$2"; shift 2; printf '%s\n' "$@" > "$BR/sotd.$f"; echo "$rc" > "$BR/sotd.$f.rc"
}
# bwalk SOTD_RC LINE... : $BR holds this shell alone, under a native parent (winpid 9000),
# and sotd's walk from it is LINE... (each `<exe><TAB><command line>`), given the pids
# 9001, 9002, ... in order (a `!` line none).
bwalk() {
    local rc="$1" l i=9000 out=(); shift
    rm -rf "${BR:?}"; bproc $$ 1 9000 bash -c x
    for l in "$@"; do case "$l" in '!'*) out+=("$l") ;; *) i=$((i + 1)); out+=("$i${TAB}$l") ;; esac; done
    bsotd 9000 "$rc" "${out[@]}"
}
breq() {  # WANT_RC DESC TEXT SELF WSID : sot_require_agent on a "Windows" host over $BR
    local want="$1" desc="$2" text="$3" self="$4" wsid="$5" res
    res="$( [ -z "$self" ] || export SOT_COMM_SELF_FILE="$self"; [ -z "$wsid" ] || export SOT_WORKSPACE_ID="$wsid"
            _sot_is_windows() { return 0; }; _SOT_PROC="$BR"; export SOTD_BIN="$WORK/sotd-bridge"
            o="$(sot_require_agent)"; echo "rc=$?|$o" )"
    case "$res" in "rc=$want|"*"$text"*) ok "$desc" ;; *) bad "$desc (want rc=$want and '$text', got: $res)" ;; esac
}
winreq() {  # WANT_RC DESC TEXT SOTD_RC LINE... : sot_require_agent over bwalk on a "Windows" host
    local want="$1" desc="$2" text="$3" src="$4"; shift 4
    bwalk "$src" "$@"; breq "$want" "sotd.exe: $desc" "$text" "" ""
}
TAB=$'\t'
WALK=("bash.exe${TAB}bash.exe -c x" "claude.exe${TAB}claude.exe" "sot-capsule.exe${TAB}sot-capsule.exe run")
winreq 0 "exit 0, the walk reaches the capsule"        ""            0 "${WALK[@]}"
winreq 2 "exit 3 is refused as a truncated walk"       "$R_TREE_TEXT" 3 "bash.exe${TAB}bash.exe -c x" "claude.exe${TAB}claude.exe"
winreq 2 "exit 2 is refused and says to update sotd"   "update sotd" 2 "bash.exe${TAB}bash.exe -c x" "claude.exe${TAB}claude.exe"
winreq 2 "exit 1 is refused and says to update sotd"   "update sotd" 1 "${WALK[@]}"
winreq 2 "node.exe with an empty command line is refused"   "$R_TREE_TEXT" 0 "bash.exe${TAB}bash.exe -c x" "node.exe${TAB}" "${WALK[@]:1}"
winreq 2 "a !truncated line is refused"                "$R_TREE_TEXT" 3 "bash.exe${TAB}bash.exe -c x" '!truncated'
wtbl 2 "node.exe cli.js with no arguments over a native claude.exe whose command line is unreadable" \
    "bash.exe${TAB}bash.exe -c x" "claude.exe${TAB}" "node.exe${TAB}node.exe C:\\n\\node_modules\\@anthropic-ai\\claude-code\\cli.js" "sot-capsule.exe${TAB}sot-capsule.exe run"
wtbl 1 "a standalone claude.exe with an unreadable command line is one layer" \
    "bash.exe${TAB}bash.exe -c x" "claude.exe${TAB}" "sot-capsule.exe${TAB}sot-capsule.exe run"
# Exact arguments: sotd.exe prints a newline and a return inside a command line as \x1c and
# \x1b (a TAB stays a raw TAB), so a host and a child whose arguments differ only by one of
# them against a space are two layers, and identical ones are one.
WCLI='node.exe C:\n\node_modules\@anthropic-ai\claude-code\cli.js'
for g in $'\x1c' $'\x1b'; do
    gn="$(printf '%s' "$g" | od -An -tx1 | tr -d ' ')"
    wtbl 2 "a node host and a native child whose arguments differ only by \\x$gn against a space" \
        "bash.exe${TAB}bash.exe -c x" "claude.exe${TAB}claude.exe -p A B" "node.exe${TAB}$WCLI -p A${g}B" "sot-capsule.exe${TAB}sot-capsule.exe run"
    wtbl 1 "a node host and a native child with the same \\x$gn argument are one layer" \
        "bash.exe${TAB}bash.exe -c x" "claude.exe${TAB}claude.exe -p A${g}B" "node.exe${TAB}$WCLI -p A${g}B" "sot-capsule.exe${TAB}sot-capsule.exe run"
done
# An unquoted TAB is an argument separator and a quoted one stays in its argument (the new
# sotd prints it raw, so the command line is everything after the first TAB).
WCJS='node.exe C:\n\node_modules\@openai\codex\bin\codex.js'
wtbl 1 "an unquoted TAB in a node host's command line separates arguments: one layer" \
    "bash.exe${TAB}bash.exe -c x" "codex.exe${TAB}codex.exe --flag work" "node.exe${TAB}$WCJS --flag${TAB}work" "sot-capsule.exe${TAB}sot-capsule.exe run"
wtbl 2 "a quoted TAB stays inside its argument, and the rest is read: two layers" \
    "bash.exe${TAB}bash.exe -c x" "codex.exe${TAB}codex.exe \"A${TAB}B\" D" "node.exe${TAB}$WCJS \"A${TAB}B\" C" "sot-capsule.exe${TAB}sot-capsule.exe run"
wtbl 2 "a quoted TAB against a space in the child: two layers" \
    "bash.exe${TAB}bash.exe -c x" "codex.exe${TAB}codex.exe \"A B\"" "node.exe${TAB}$WCJS \"A${TAB}B\"" "sot-capsule.exe${TAB}sot-capsule.exe run"

# The Microsoft C runtime's rules for backslashes before a quote (and `""` inside quotes): a host
# and a child whose arguments are the same under those rules are one layer, otherwise two.
wpair() { wtbl "$1" "C runtime: host $2 against child $3" "bash.exe${TAB}bash.exe -c x" "codex.exe${TAB}codex.exe $3" "node.exe${TAB}$WCJS $2" "sot-capsule.exe${TAB}sot-capsule.exe run"; }
wpair 1 '"a\\"' 'a\'; wpair 1 '"a\\\\"' 'a\\'; wpair 1 'a\\\"' '"a\\\""'; wpair 1 '"a b"\' '"a b\\"'   # F F C F
wpair 1 '"a b\"' '"a b\""'; wpair 1 '"a""b c"' '"a\"b c"'; wpair 1 'p q' 'p q'                      # C F C
wpair 2 '"a\\"' 'a\\'; wpair 2 'a\\\"' '"a\\"'; wpair 2 '"a b"' 'a b'                                 # C C C: arguments differ

# --- 1b2. the row rule: a process that names a row must reach that row's capsule ---
R_ROW_TEXT="is not shown to run inside that row's capsule"
# rreq WANT DESC TEXT SELF WSID RECORD... : req with an identity set only inside its subshell
# (SELF is a self file path, WSID a SOT_WORKSPACE_ID; empty sets neither). RHOST, when set
# for the call (`RHOST=h rreq ...`), is this host's SOT_SELF_HOST inside the subshell only.
rreq() {
    local want="$1" desc="$2" text="$3" self="$4" wsid="$5" res; shift 5; CHAIN=("$@")
    res="$( [ -z "${RHOST:-}" ] || export SOT_SELF_HOST="$RHOST"
            [ -z "$self" ] || export SOT_COMM_SELF_FILE="$self"; [ -z "$wsid" ] || export SOT_WORKSPACE_ID="$wsid"
            _sot_ancestor_chain() { local r; [ "${#CHAIN[@]}" -gt 0 ] || return 1; for r in "${CHAIN[@]}"; do printf '%s\n' "${r//|/$US}"; done
                                    case "$r" in '!'*) ;; *) echo '!end' ;; esac; }
            o="$(sot_require_agent)"; echo "rc=$?|$o" )"
    case "$res" in "rc=$want|"*"$text"*) ok "row rule: $desc" ;; *) bad "row rule: $desc (want rc=$want and '$text', got: $res)" ;; esac
}
SA="$WORK/self/h__ws-a.txt"
rreq 2 "a chain to the top, naming ws-a"                  "$R_ROW_TEXT" "$SA" "" bash claude
rreq 2 "ws-b's leg, naming ws-a"                          "$R_ROW_TEXT" "$SA" "" bash claude "sot-capsule|run|/s/workspaces/ws-b/voyages/v1|v1|--cols|80"
rreq 2 "a capsule with no arguments"                      "$R_ROW_TEXT" "$SA" "" bash claude sot-capsule
rreq 2 "ws-a only after --"                               "$R_ROW_TEXT" "$SA" "" bash claude "sot-capsule|supervise|/s/workspaces/ws-b|--start|--|/s/workspaces/ws-a/voyages/v1"
rreq 2 "a prefix id: ws-ab's leg for row ws-a"            "$R_ROW_TEXT" "$SA" "" bash claude "sot-capsule|run|/s/workspaces/ws-ab/voyages/v1|v1"
rreq 2 "no self file, SOT_WORKSPACE_ID=ws-a, at the top"  "$R_ROW_TEXT" "" ws-a bash claude
rreq 0 "ws-a's leg"                                       "" "$SA" "" bash claude "sot-capsule|run|/s/workspaces/ws-a/voyages/v1|v1|--cols|80"
rreq 0 "ws-a's supervisor"                                "" "$SA" "" bash "sot-capsule|supervise|/s/workspaces/ws-a|--start|--|claude"
rreq 0 "a ps-split path"                                  "" "$SA" "" bash claude "sot-capsule|run|/Users/a|b/.local/state/sot/workspaces/ws-a/voyages/v1|v1"
rreq 0 "a backslash path"                                 "" "$SA" "" bash claude 'sot-capsule|run|C:\s\workspaces\ws-a\voyages\v1'
rreq 0 "h__nopane.txt names no row, at the top"           "" "$WORK/self/h__nopane.txt" "" bash claude
rreq 0 "self-x.txt with SOT_WORKSPACE_ID set names no row, at the top" "" "$WORK/self/self-x.txt" ws-a bash claude
rreq 0 "nothing set, at the top"                          "" "" "" bash claude
rreq 0 "self-x.txt names no row, inside ws-b's leg"       "" "$WORK/self/self-x.txt" "" bash claude "sot-capsule|run|/s/workspaces/ws-b/voyages/v1|v1"
rreq 1 "two agents inside ws-a's leg"                     "has no comm identity" "$SA" "" bash codex bash claude "sot-capsule|run|/s/workspaces/ws-a/voyages/v1|v1"
rreq 1 "two agents at the top with row ws-a"              "has no comm identity" "$SA" "" bash codex bash claude
# Claude Code's agent view moves a conversation into its own background daemon, under systemd --user.
rreq 2 "a conversation moved to Claude Code's background daemon" "$R_ROW_TEXT" "" ws-t-1 bash "claude bg-spare|--bg-spare|$WORK/a.claim.sock" "claude bg-pty-host" "/lib/systemd/systemd|--user"
# The row id strips this host's own label first (a label may hold __; so may an id).
WLEG='sot-capsule|run|/s/workspaces/%s/voyages/v1|v1'
RHOST=dev__box rreq 0 "host dev__box, self file dev__box__ws-a.txt, ws-a's leg" "" "$WORK/self/dev__box__ws-a.txt" "" bash claude "$(printf "$WLEG" ws-a)"
RHOST=h rreq 0 "an id holding __: ws-a__b's leg" "" "$WORK/self/h__ws-a__b.txt" "" bash claude "$(printf "$WLEG" ws-a__b)"
RHOST=h rreq 2 "an id holding __: ws-b's leg" "$R_ROW_TEXT" "$WORK/self/h__ws-a__b.txt" "" bash claude "$(printf "$WLEG" ws-b)"
RHOST=other rreq 2 "host other does not match dev__box: first __, row box__ws-a, ws-a's leg" "$R_ROW_TEXT" "$WORK/self/dev__box__ws-a.txt" "" bash claude "$(printf "$WLEG" ws-a)"

# rwinreq WANT DESC TEXT SELF WSID SOTD_RC LINE... : winreq with an identity set only inside its subshell.
rwinreq() {
    local want="$1" desc="$2" text="$3" self="$4" wsid="$5" src="$6"; shift 6
    bwalk "$src" "$@"; breq "$want" "sotd.exe row rule: $desc" "$text" "$self" "$wsid"
}
WSANDBOX=("bash.exe${TAB}bash.exe -c x" "bash.exe${TAB}bash.exe")
rwinreq 2 "the sandbox gap: exit 0 and only bash.exe lines"  "$R_ROW_TEXT" "$SA" "" 0 "${WSANDBOX[@]}"
rwinreq 0 "the same output with no row named (the sshd case)" "" "" "" 0 "${WSANDBOX[@]}"
rwinreq 0 "a quoted leg under claude.exe"                    "" "$SA" "" 0 "bash.exe${TAB}bash.exe -c x" "claude.exe${TAB}claude.exe" \
    "sot-capsule.exe${TAB}\"C:\\p\\sot-capsule.exe\" run \"C:\\Users\\a b\\AppData\\Local\\sot\\workspaces\\ws-a\\voyages\\v1\" v1 --cols 80 --rows 24"
rwinreq 2 "a capsule with an empty command line"             "$R_ROW_TEXT" "$SA" "" 0 "bash.exe${TAB}bash.exe -c x" "claude.exe${TAB}claude.exe" "sot-capsule.exe${TAB}"
rwinreq 2 "ws-b's leg"                                       "$R_ROW_TEXT" "$SA" "" 0 "bash.exe${TAB}bash.exe -c x" "claude.exe${TAB}claude.exe" \
    "sot-capsule.exe${TAB}\"C:\\p\\sot-capsule.exe\" run \"C:\\s\\workspaces\\ws-b\\voyages\\v1\" v1"

# --- 1c. the two non-Linux parsers, as stdin filters (Linux walks /proc, so they run nowhere else) ---
pipes() { tr "$US" '|' | tr '\n' ' ' | sed 's/ $//'; }
eq "ps filter: a chain to the top, spaces become field breaks" \
    "$(printf '%s\n' '  100    99 bash -c x' '   99    98 node /x y/codex.js' '   98     1 sot-capsule run' | _sot_ps_records 100 | pipes)" \
    "bash|-c|x node|/x|y/codex.js sot-capsule|run !end"
eq "ps filter: a parent missing from the table is a truncated walk" \
    "$(printf '%s\n' '  100    99 bash -c x' '   50     1 init' | _sot_ps_records 100 | pipes)" "bash|-c|x !truncated"
eq "ps filter: my own pid missing prints nothing" "$(printf '%s\n' '   50     1 init' | _sot_ps_records 100 | pipes)" ""
# mtbl WANT DESC LINE... : the layers a macOS chain counts, through the ps filter (`ps` lines, caller first).
mtbl() {
    local want="$1" desc="$2" got; shift 2
    got="$(printf '%s\n' "$@" | _sot_ps_records 100 | _sot_agent_layers 2>/dev/null | awk 'NF && $0 != "!ok"' | wc -l | tr -d ' ')"
    eq "macOS table: $desc" "$got" "$want"
}
B='  100    99 bash -c x'; CAP='   97     1 sot-capsule run'
mtbl 2 "a (claude) whose arguments ps cannot read under a codex is a second layer" "$B" '   99    98 codex exec y' '   98    97 (claude)' "$CAP"
mtbl 2 "a (claude) never dedupes with its node host" "$B" '   99    98 (claude)' '   98    97 node /n/node_modules/@anthropic-ai/claude-code/cli.js -p q' "$CAP"
mtbl 1 "a standalone (claude) is one layer" "$B" '   99    97 (claude)' "$CAP"
mtbl 1 "a readable claude under a login shell is one layer" "$B" '   99    98 claude -p q' '   98    97 -zsh' '   97     1 (login)'
eq "ps filter: a (name) record is the name with arguments unknown" \
    "$(printf '%s\n' '  100    99 bash -c x' '   99    97 (claude)' "$CAP" | _sot_ps_records 100 | pipes)" "bash|-c|x claude|$(printf '\036') sot-capsule|run !end"
eq "ps filter: a (node) record is truncated, and nothing follows" \
    "$(printf '%s\n' '  100    99 bash -c x' '   99    97 (node)' | _sot_ps_records 100 | pipes)" "bash|-c|x !truncated"
eq "ps filter: a (name with spaces) record keeps the whole name" \
    "$(printf '%s\n' '  100    99 bash -c x' '   99     1 (Foo Helper)' | _sot_ps_records 100 | pipes)" "bash|-c|x Foo Helper|$(printf '\036') !end"
eq "ps filter: a chain past 64 is truncated at 64" \
    "$(for ((i = 1000; i < 1070; i++)); do printf '%s %s bash\n' "$i" "$((i + 1))"; done | _sot_ps_records 1000 | awk 'END { print NR, $0 }')" "65 !truncated"
eq "windows filter: arguments, quotes and an escaped quote; the pid and argv[0] are dropped" \
    "$(printf '%s\n' "11${TAB}bash.exe${TAB}C:\\git\\bash.exe -c \"x y\"" "12${TAB}a.exe${TAB}a.exe \"p\\\"q\"" | _sot_win_records | pipes)" 'bash.exe|-c|x y a.exe|p"q'
eq "windows filter: a node.exe with an empty command line is truncated, and nothing follows" \
    "$(printf '%s\n' "11${TAB}bash.exe${TAB}bash.exe" "12${TAB}node.exe${TAB}" "13${TAB}claude.exe${TAB}claude.exe" | _sot_win_records | pipes)" "bash.exe !truncated"
eq "windows filter: any other exe with an empty command line has arguments unknown (one RS argument), not an empty tail" \
    "$(printf '%s\n' "11${TAB}claude.exe${TAB}" "12${TAB}a.exe${TAB}a.exe" | _sot_win_records | pipes)" "claude.exe|$(printf '\036') a.exe"
eq "windows filter: the encoded newline and return stay inside their token" \
    "$(printf '%s\n' "11${TAB}a.exe${TAB}a.exe p q"$'\x1c'"s t"$'\x1b'"u" | _sot_win_records | tr -d '\n' | od -An -c | tr -d ' ' | tr -d '\n')" \
    "$(printf '%s' "a.exe${US}p${US}q"$'\x1c'"s${US}t"$'\x1b'"u" | od -An -c | tr -d ' ' | tr -d '\n')"
eq "windows filter: a !truncated line passes through and ends the output" \
    "$(printf '%s\n' "11${TAB}bash.exe${TAB}bash.exe" '!truncated' | _sot_win_records | pipes)" "bash.exe !truncated"
eq "windows filter: a line that does not begin with a pid (a sotd.exe older than --from) is truncated" \
    "$(printf '%s\n' "bash.exe${TAB}bash.exe -c x" | _sot_win_records | pipes)" "!truncated"
eq "windows filter: the status is 0 after records, 1 once it has printed !truncated" \
    "$(printf '%s\n' "11${TAB}a.exe${TAB}a.exe" | _sot_win_records > /dev/null; printf '%s ' "$?"; printf '%s\n' '!truncated' | _sot_win_records > /dev/null; echo "$?")" "0 1"
eq "windows filter: the C runtime's backslash and quote rules" "$(printf '%s\n' "11${TAB}a.exe${TAB}"'a.exe "a\\" a\\\" "a b"\ "a""b c" x\\y' | _sot_win_records | pipes)" 'a.exe|a\|a\"|a b\|a"b c|x\\y'

# --- 1d. the Windows walk: Cygwin's /proc up to a native parent, sotd above it, segment by segment ---
CAPA="sot-capsule.exe${TAB}sot-capsule.exe run C:\\s\\workspaces\\ws-a\\voyages\\v1 v1"
CLA="claude.exe${TAB}claude --x"
WCX="runs under codex, started inside claude's session"
rm -rf "${BR:?}"; bproc $$ 7001 9000 bash /s/comm-poll.sh; bproc 7001 1 9001 bash -c tool
bsotd 9001 0 "9101${TAB}bash.exe${TAB}C:\\git\\bin\\bash.exe -c tool" "9102${TAB}$CLA" "9103${TAB}$CAPA"
breq 0 "windows walk: a script started as a script reaches its row's capsule through its MSYS parent" "" "$SA" ""
# codex.exe started by a script: sotd stops at that script (an MSYS exec, no live Windows parent).
rm -rf "${BR:?}"; bproc $$ 7001 9000 bash /s/comm-poll.sh; bproc 7001 1 9001 bash -c poll
bsotd 9001 0 "9201${TAB}codex.exe${TAB}codex exec x" "9202${TAB}bash.exe${TAB}bash w.sh"
bproc 7002 7003 9202 bash w.sh; bproc 7003 1 9003 bash -c tool
bsotd 9003 0 "9301${TAB}bash.exe${TAB}C:\\git\\bin\\bash.exe -c tool" "9302${TAB}$CLA" "9303${TAB}$CAPA"
breq 1 "windows walk: where sotd stops at a live MSYS process, its Cygwin parent carries the walk to claude" "$WCX" "$SA" ""
breq 1 "windows walk: the same with no row named still sees the second agent" "$WCX" "" ""
# The same, but the stopped-at process reports its native image's winpid (the line before).
rm -rf "${BR:?}/7002"; bproc 7002 7003 9201 bash w.sh
breq 1 "windows walk: where sotd stops at a stub, the line before names its Cygwin process" "$WCX" "$SA" ""
# Both lines map: the last line's own process is taken first.
rm -rf "${BR:?}"; bproc $$ 7001 9000 bash /s/x.sh; bproc 7001 1 9001 bash -c poll
bsotd 9001 0 "9401${TAB}a.exe${TAB}a.exe" "9402${TAB}bash.exe${TAB}bash s.sh"
bproc 7402 7403 9402 bash s.sh; bproc 7403 1 9403 bash -c tool
bproc 7401 7404 9401 bash t.sh; bproc 7404 1 9404 bash -c other
bsotd 9403 0 "9501${TAB}$CLA" "9502${TAB}$CAPA"
bsotd 9404 0 "9601${TAB}codex.exe${TAB}codex" "9602${TAB}$CLA" "9603${TAB}$CAPA"
breq 0 "windows walk: the last line's own process is tried before the line before's" "" "$SA" ""
# The stopped-at process has a native parent: that is the top.
rm -rf "${BR:?}"; bproc $$ 7001 9000 bash /s/x.sh; bproc 7001 1 9001 bash -c poll
bsotd 9001 0 "9701${TAB}codex.exe${TAB}codex" "9702${TAB}bash.exe${TAB}bash -c y"; bproc 7702 1 9702 bash -c y
breq 2 "windows walk: a stopped-at process with a native parent is the top, refused for a row" "$R_ROW_TEXT" "$SA" ""
breq 0 "windows walk: the same top passes with no row named" "" "" ""
# sotd printed nothing: the head's native parent is the top.
rm -rf "${BR:?}"; bproc $$ 1 9000 bash /s/x.sh; bsotd 9000 0
breq 2 "windows walk: sotd prints no line, the top, refused for a row" "$R_ROW_TEXT" "$SA" ""
breq 0 "windows walk: the same passes with no row named" "" "" ""
# Unreadable: a head with no winpid; a sotd line with no pid.
rm -rf "${BR:?}"; bproc $$ 1 9000 bash /s/x.sh; rm -f "${BR:?}/$$/winpid"
breq 2 "windows walk: a head whose winpid cannot be read is refused" "$R_TREE_TEXT" "" ""
rm -rf "${BR:?}"; bproc $$ 1 9000 bash /s/x.sh; bsotd 9000 0 "bash.exe${TAB}bash.exe -c x" "$CLA" "sot-capsule.exe${TAB}sot-capsule.exe run"
breq 2 "windows walk: a sotd line with no pid (built before --from) is refused" "$R_TREE_TEXT" "" ""
# The 64 cap counts both kinds of record: 41 from /proc, then a capsule at sotd's 31st line.
rm -rf "${BR:?}"; bproc $$ 7001 9000 bash x
for ((i = 7001; i < 7040; i++)); do bproc "$i" "$((i + 1))" "$((i + 2000))" bash x; done; bproc 7040 1 9040 bash x
CAPL=(); for ((i = 1; i <= 30; i++)); do CAPL+=("$((9900 + i))${TAB}bash.exe${TAB}bash.exe"); done
bsotd 9040 0 "${CAPL[@]}" "9999${TAB}$CAPA"
breq 2 "windows walk: the 64 cap spans both kinds of record, so a capsule past it is not reached" "$R_TREE_TEXT" "$SA" ""

# --- 2. end to end --------------------------------------------------------------
printf '%s\n' 'n=$1; shift; exec -a "$n" bash "$@"' > "$WORK/fake.sh"
printf '%s\n' '"$@"; exit $?' > "$WORK/hold.sh"
# An npm codex: the host script forwards its own arguments to the native process.
mkdir -p "$WORK/npm"; printf '%s\n' 'bash "'"$WORK"'/fake.sh" codex "$@"; exit $?' > "$WORK/npm/codex"
F() { bash "$WORK/fake.sh" "$@"; }
# chain KIND SELF CMD... : run CMD in a tool shell under a fake process chain,
# from $WORK, as the handle SELF names. Tool shell last, agents above it.
chain() {
    local kind="$1" self="$2"; shift 2
    # The tool shell writes CMD's status to $RCF: a shell that outlives its
    # command keeps the agent's own process shape, and its own status is not CMD's.
    local tool=(bash -c '"$@"; echo $? > "$RCF"' _ "$@") cap=(sot-capsule hold.sh)
    cd "$WORK" || return 1
    export SOT_COMM_SELF_FILE="$self"
    case "$kind" in
        own)   F "${cap[@]}" bash "$WORK/fake.sh" claude hold.sh "${tool[@]}" ;;
        child) F "${cap[@]}" bash "$WORK/fake.sh" claude hold.sh bash "$WORK/fake.sh" codex hold.sh "${tool[@]}" ;;
        npm)   F "${cap[@]}" bash "$WORK/fake.sh" node "$WORK/npm/codex" hold.sh "${tool[@]}" ;;
        bash)  F "${cap[@]}" bash hold.sh "${tool[@]}" ;;
        # One agent beneath the stand-in for row $ROWID's capsule (lib-home-guard.sh).
        rown)  in_row "$ROWID" bash "$WORK/fake.sh" claude hold.sh "${tool[@]}" ;;
        # A Windows row (SIMWIN): a native claude.exe beneath row $ROWID's native capsule.
        wown)  in_wrow "$ROWID" bash "$WORK/fake.sh" claude.exe hold.sh "${tool[@]}" ;;
        # An npm agent is `node <script>`; the script runs CMD in its own shell.
        # NODE_ARGS is node's argv after `node`; "$@" is CMD, which the script runs.
        nodechild) F "${cap[@]}" bash "$WORK/fake.sh" claude hold.sh node "${NODE_ARGS[@]}" "$@" ;;
        nodeown)   F "${cap[@]}" node "${NODE_ARGS[@]}" "$@" ;;
        # DEPTH launchers between the tool shell and the agent, past the walk's cap.
        deep)  local a=(bash hold.sh "${tool[@]}") i
               for ((i = 0; i < 70; i++)); do a=(bash "$WORK/hold.sh" "${a[@]}"); done
               F "${cap[@]}" bash "$WORK/fake.sh" claude hold.sh "${a[@]}" ;;
    esac
}
RCF="$WORK/rc"; export RCF
rc_of() { RC="$(cat "$RCF" 2>/dev/null)"; rm -f "${RCF:?}"; }
run() { OUT="$(chain "$@" 2>&1)"; rc_of; }   # -> OUT / RC, in the caller's shell

REG="$SOT_COMM_HOME/registry.json"
ROW="row-agent"; PEER="peer"
SELF_ROW="$WORK/self-row.txt"; SELF_PEER="$WORK/self-peer.txt"
CURSOR="$SOT_COMM_HOME/read/$ROW.cursor"; INBOX="$SOT_COMM_HOME/inbox/$ROW.jsonl"
sum() { cat "$@" 2>/dev/null | cksum; }   # a missing file sums as empty, the same every time

run own "$SELF_ROW" "$JOIN" --name "$ROW"   || true; [ "$RC" -eq 0 ] || { echo "FATAL: setup join $ROW: $OUT" >&2; exit 1; }
run own "$SELF_PEER" "$JOIN" --name "$PEER" || true; [ "$RC" -eq 0 ] || { echo "FATAL: setup join $PEER: $OUT" >&2; exit 1; }
run own "$SELF_PEER" "$SEND" "@$ROW" "frame-one"; [ "$RC" -eq 0 ] && [ -s "$INBOX" ] || { echo "FATAL: setup send: $OUT" >&2; exit 1; }
run own "$SELF_ROW" "$STATUS" waiting x;          [ "$RC" -eq 0 ] || { echo "FATAL: setup status: $OUT" >&2; exit 1; }

# The heartbeat hook finds comm-context.sh and comm-lib.sh beside itself.
FLAT="$WORK/flat"; mkdir -p "$FLAT"
cp "$HOOKS_DIR/comm-status-heartbeat.sh" "$FLAT/"
ln -s "$SCRIPTS_DIR/comm-context.sh" "$FLAT/comm-context.sh"
ln -s "$SCRIPTS_DIR/comm-lib.sh" "$FLAT/comm-lib.sh"
# A turn is running (a floor) and the row's stamp is old: what the heartbeat refreshes.
# The shims npm installs: `node <package>/bin/codex.js`. Each runs its arguments
# in a shell with stdio inherited, as the real agents run their tools; the shell
# writes the command's status to $RCF.
SHIM='const r = require("child_process").spawnSync("bash", ["-c", "\"$@\"; echo $? > \"$RCF\"", "_"].concat(process.argv.slice(2)), { stdio: "inherit" }); process.exit(r.status === null ? 1 : r.status);'
mkdir -p "$WORK/nm/node_modules/@openai/codex/bin" "$WORK/nm/node_modules/@anthropic-ai/claude-code" "$WORK/x y"
printf '%s\n' "$SHIM" > "$WORK/nm/node_modules/@openai/codex/bin/codex.js"
printf '%s\n' "$SHIM" > "$WORK/nm/node_modules/@anthropic-ai/claude-code/cli.js"
printf '%s\n' "$SHIM" > "$WORK/x y/codex.js"
backdate() { jq --arg n "$ROW" '.agents[$n] += {floor: "user", status_at: "2020-01-01T00:00:00Z"}' "$REG" > "$WORK/reg.tmp" && mv "$WORK/reg.tmp" "$REG"; }
state_of() { jq -r --arg n "$ROW" '.agents[$n].state' "$REG"; }
stop_hook() {  # KIND — the Stop hook, as a hook runs it: JSON on stdin
    OUT="$(printf '{}' | CLAUDE_CODE_SESSION_ID=agl-stop chain "$1" "$SELF_ROW" bash "$HOOKS_DIR/comm-status-idle.sh" 2>&1)"; rc_of
}
heartbeat() {  # KIND SESSION
    rm -f "${SOT_COMM_HOME:?}"/state/hb-*.tick 2>/dev/null
    OUT="$(printf '{"tool_name":"Bash"}' | CLAUDE_CODE_SESSION_ID="$2" chain "$1" "$SELF_ROW" bash "$FLAT/comm-status-heartbeat.sh" 2>&1)"; rc_of
}

# --- the child first: a codex exec under a claude row ---------------------------
cur0="$(sum "$CURSOR")"; inbox0="$(sum "$INBOX")"
run child "$SELF_ROW" "$POLL"
eq  "child: poll refuses" "$RC" 1
has "child: poll names the cause" "$OUT" "has no comm identity"
hasnt "child: poll shows no frame" "$OUT" "frame-one"
eq  "child: poll leaves the cursor alone" "$(sum "$CURSOR")" "$cur0"

run child "$SELF_PEER" "$SEND" "@$ROW" "from-child"
eq  "child: send refuses" "$RC" 1
has "child: send says FAILED" "$OUT" "FAILED -> @$ROW:"
has "child: send names the cause" "$OUT" "has no comm identity"
eq  "child: send leaves the inbox alone" "$(sum "$INBOX")" "$inbox0"

reg0="$(sum "$REG")"; selfrow0="$(sum "$SELF_ROW")"
run child "$SELF_ROW" "$JOIN" --name "$ROW"
eq  "child: join refuses" "$RC" 1
has "child: join names the cause" "$OUT" "has no comm identity"
eq  "child: join leaves the self file alone" "$(sum "$SELF_ROW")" "$selfrow0"
eq  "child: join leaves the registry alone" "$(sum "$REG")" "$reg0"

run child "$SELF_ROW" "$STATUS" working y
eq  "child: status working refuses" "$RC" 1
has "child: status working names the cause" "$OUT" "has no comm identity"
run child "$SELF_ROW" "$STATUS" prompt
eq  "child: status prompt is a silent no-op" "$RC" 0
eq  "child: status leaves the registry alone" "$(sum "$REG")" "$reg0"

stop_hook child
hasnt "child: the Stop hook does not block" "$OUT" '"decision"'
eq    "child: the Stop hook leaves the registry alone" "$(sum "$REG")" "$reg0"

backdate; reg1="$(sum "$REG")"
heartbeat child agl-hb-child
eq  "child: the heartbeat leaves the registry alone" "$(sum "$REG")" "$reg1"
eq  "child: the row is still waiting" "$(state_of)" waiting

# --- then the row's own agent ---------------------------------------------------
stop_hook own
has "own: the Stop hook blocks on unread mail" "$OUT" '"decision":"block"'
backdate; reg2="$(sum "$REG")"
heartbeat own agl-hb-own
if [ "$(sum "$REG")" != "$reg2" ]; then ok "own: the heartbeat refreshes the row"; else bad "own: the heartbeat refreshes the row"; fi
run own "$SELF_ROW" "$POLL"
eq  "own: poll succeeds" "$RC" 0
has "own: poll shows the frame" "$OUT" "frame-one"
run npm "$SELF_ROW" "$POLL"
eq  "own: poll under an npm codex row (two processes, one agent)" "$RC" 0
run bash "$SELF_ROW" "$POLL"
eq  "own: poll under a bash row (no agent)" "$RC" 0

# --- the npm agents, with a real node: a node-run second agent is a child ---------
# A missing node FAILS this suite; the shims below are not skipped.
if command -v node >/dev/null 2>&1; then ok "node is present"; else bad "node is required for the npm agent cases"; fi
cur0="$(sum "$CURSOR")"
NODE_ARGS=("$WORK/nm/node_modules/@openai/codex/bin/codex.js")
run nodeown "$SELF_ROW" "$POLL"
eq  "node codex.js alone under the capsule is one agent: poll succeeds" "$RC" 0
run nodechild "$SELF_ROW" "$POLL"
eq  "child: poll under claude then node codex.js (npm package path) refuses" "$RC" 1
has "child: that refusal names the cause" "$OUT" "has no comm identity"
NODE_ARGS=("$WORK/nm/node_modules/@anthropic-ai/claude-code/cli.js")
run nodechild "$SELF_ROW" "$POLL"
eq  "child: poll under claude then node claude-code/cli.js refuses" "$RC" 1
NODE_ARGS=(--no-warnings "$WORK/x y/codex.js")
run nodechild "$SELF_ROW" "$POLL"
eq  "child: poll under claude then node --no-warnings 'x y/codex.js' refuses" "$RC" 1
: > "$WORK/x.cjs"
NODE_ARGS=(--require "$WORK/x.cjs" "$WORK/nm/node_modules/@anthropic-ai/claude-code/cli.js")
run nodechild "$SELF_ROW" "$POLL"
eq  "child: poll under claude then node --require x.cjs claude-code/cli.js refuses" "$RC" 1
has "child: that refusal names the cause" "$OUT" "has no comm identity"
eq  "child: none of those moved the cursor" "$(sum "$CURSOR")" "$cur0"

# --- a node host and a native child: one layer only when the child is the host's own forwarding ---
# A copy of bash named claude / codex is the native binary; the -c command is compound, so
# bash cannot exec its last command and drop the native process from the chain.
mkdir -p "$WORK/natbin" "$WORK/nat/node_modules/@anthropic-ai/claude-code" "$WORK/nat/node_modules/@openai/codex/bin"
cp "$(command -v bash)" "$WORK/natbin/claude"; cp "$(command -v bash)" "$WORK/natbin/codex"
# claude: a tool run by node's cli.js, started with its own arguments (they differ from the host's).
printf '%s\n' 'const r = require("child_process").spawnSync(process.env.NAT_BIN, ["-c", process.env.NAT_CMD], { stdio: "inherit" }); process.exit(r.status === null ? 1 : r.status);' > "$WORK/nat/node_modules/@anthropic-ai/claude-code/cli.js"
# codex: the npm launcher forwards its own arguments to the native binary.
printf '%s\n' 'const r = require("child_process").spawnSync(process.env.NAT_BIN, process.argv.slice(2), { stdio: "inherit" }); process.exit(r.status === null ? 1 : r.status);' > "$WORK/nat/node_modules/@openai/codex/bin/codex.js"
NATCHAIN="$WORK/natchain.txt"
NAT_CMD="$(printf '%q; rc=$?; bash -c %q _ %q > %q; exit $rc' "$POLL" '. "$1"; _sot_ancestor_chain' "$SCRIPTS_DIR/comm-lib.sh" "$NATCHAIN")"
natrun() {  # NATIVE_NAME HOST_ARGS...  (NAT_BIN, NAT_CMD in the environment)
    local nat="$1"; shift; rm -f "${NATCHAIN:?}"; cd "$WORK" || return 1
    OUT="$(SOT_COMM_SELF_FILE="$SELF_ROW" NAT_BIN="$WORK/natbin/$nat" NAT_CMD="$NAT_CMD" F sot-capsule hold.sh node "$@" 2>&1)"; RC=$?
}
natchain_has() { case "$(tr "$US" '|' < "$NATCHAIN" 2>/dev/null)" in *"$1"*) return 0 ;; *) return 1 ;; esac; }
cur0="$(sum "$CURSOR")"
natrun claude "$WORK/nat/node_modules/@anthropic-ai/claude-code/cli.js" --permission-mode auto
eq  "child: node cli.js whose tool runs native claude -c with other arguments is refused" "$RC" 1
has "child: that refusal names the cause" "$OUT" "has no comm identity"
if natchain_has "$WORK/natbin/claude|-c|"; then ok "child: the native claude is in the recorded chain"; else bad "child: the native claude is in the recorded chain"; fi
eq  "child: that refusal did not move the cursor" "$(sum "$CURSOR")" "$cur0"
natrun codex "$WORK/nat/node_modules/@openai/codex/bin/codex.js" -c "$NAT_CMD"
eq  "own: node codex.js forwarding its arguments to the native codex is one layer: poll succeeds" "$RC" 0
if natchain_has "$WORK/natbin/codex|-c|"; then ok "own: the native codex is in the recorded chain"; else bad "own: the native codex is in the recorded chain"; fi
# Exact arguments: the host's and the native child's differ only by a newline against a space
# (the command is the same either way): two layers, refused. Identical ones are one layer.
NAT_CMD_NL="${NAT_CMD/; /;$'\n'}"
[ "$NAT_CMD_NL" != "$NAT_CMD" ] || { echo "FATAL: the newline variant of the native command did not differ" >&2; exit 1; }
cur0="$(sum "$CURSOR")"
natrun claude "$WORK/nat/node_modules/@anthropic-ai/claude-code/cli.js" -c "$NAT_CMD_NL"
eq  "child: a node host and a native child whose arguments differ only by a newline against a space are two layers: refused" "$RC" 1
has "child: that refusal names the cause" "$OUT" "has no comm identity"
if natchain_has "$WORK/natbin/claude|-c|"; then ok "child: the native claude is in the recorded chain (newline vs space)"; else bad "child: the native claude is in the recorded chain (newline vs space)"; fi
eq  "child: that refusal did not move the cursor" "$(sum "$CURSOR")" "$cur0"
natrun claude "$WORK/nat/node_modules/@anthropic-ai/claude-code/cli.js" -c "$NAT_CMD"
eq  "own: a node host and a native child with identical arguments are one layer: poll succeeds" "$RC" 0

# --- a refused child changes nothing under the comm home --------------------------
# Every verb, both hooks: the files (by content, size and mtime), and the listing
# (no new file, no registry skeleton), before and after. A legacy self file (no root=) is one
# the context call would heal, and a refusal comes before that.
snap() { { find "$1" | sort; find "$1" -type f -exec sha256sum {} + | sort; find "$1" -type f -exec stat -c '%n %s %Y' {} + | sort; } 2>&1; }
SELF_LEG="$WORK/self-legacy.txt"; head -2 "$SELF_ROW" > "$SELF_LEG"
NODE_ARGS=("$WORK/nm/node_modules/@openai/codex/bin/codex.js")
nochange() {  # DESC KIND CMD...
    local desc="$1" kind="$2"; shift 2
    local before after; head -2 "$SELF_ROW" > "$SELF_LEG"
    before="$(snap "$SOT_COMM_HOME"; cat "$SELF_LEG")"
    printf '%s' "${NC_IN:-{\}}" | "$@" >/dev/null 2>&1 || true
    after="$(snap "$SOT_COMM_HOME"; cat "$SELF_LEG")"
    if [ "$after" = "$before" ]; then ok "child: $desc changes nothing under the comm home, nor heals the legacy self file"
    else bad "child: $desc changed the comm home or healed the legacy self file: $(diff <(printf '%s\n' "$before") <(printf '%s\n' "$after") | tr '\n' ' ' | cut -c1-300)"; fi
}
for kind in child nodechild; do
    run_k() { chain "$kind" "$SELF_LEG" "$@" > /dev/null 2>&1; }
    nochange "poll ($kind)"   "$kind" run_k "$POLL"
    nochange "send ($kind)"   "$kind" run_k "$SEND" "@$ROW" "from-child"
    nochange "join ($kind)"   "$kind" run_k "$JOIN" --name "$ROW"
    nochange "status working ($kind)" "$kind" run_k "$STATUS" working z
    nochange "status waiting ($kind)" "$kind" run_k "$STATUS" waiting z
    nochange "leave ($kind)"  "$kind" run_k "$SCRIPTS_DIR/comm-leave.sh"
    nochange "leave --name ($kind)" "$kind" run_k "$SCRIPTS_DIR/comm-leave.sh" --name "$PEER"
    nochange "the Stop hook ($kind)" "$kind" run_k env CLAUDE_CODE_SESSION_ID="agl-nc-stop-$kind" bash "$HOOKS_DIR/comm-status-idle.sh"
    nochange "comm-context.sh ($kind)"       "$kind" run_k "$SCRIPTS_DIR/comm-context.sh"
    nochange "comm-session-start.sh ($kind)" "$kind" run_k "$SCRIPTS_DIR/comm-session-start.sh"
    nochange "comm-relay.sh send ($kind)"    "$kind" run_k "$SCRIPTS_DIR/comm-relay.sh" send "@$PEER" from-child
    nochange "comm-bootstrap.sh ($kind)"     "$kind" run_k "$SCRIPTS_DIR/comm-bootstrap.sh" some-row
    NC_IN='{"tool_name":"Bash"}' nochange "the heartbeat ($kind)" "$kind" run_k env CLAUDE_CODE_SESSION_ID="agl-nc-$kind" bash "$FLAT/comm-status-heartbeat.sh"
    # An AskUserQuestion answer marker is the owner's: a child's hook must not consume it.
    mkdir -p "$SOT_COMM_HOME/state"; ASKQ="$SOT_COMM_HOME/state/askq-agl-q-$kind.marker"; : > "$ASKQ"
    NC_IN='{"tool_name":"AskUserQuestion","tool_use_id":"agl-q-'"$kind"'"}' nochange "the heartbeat on an AskUserQuestion answer ($kind)" "$kind" run_k env CLAUDE_CODE_SESSION_ID="agl-nc-q-$kind" bash "$FLAT/comm-status-heartbeat.sh"
    if [ -f "$ASKQ" ]; then ok "child: the AskUserQuestion marker survives the heartbeat ($kind)"; else bad "child: the AskUserQuestion marker survives the heartbeat ($kind)"; fi
    # The PreToolUse hook of that dialog writes no marker for a child either.
    NC_IN='{"tool_name":"AskUserQuestion","tool_use_id":"agl-qb-'"$kind"'"}' nochange "the AskUserQuestion PreToolUse hook ($kind)" "$kind" run_k bash "$HOOKS_DIR/comm-status-blocked.sh"
    if [ ! -e "$SOT_COMM_HOME/state/askq-agl-qb-$kind.marker" ]; then ok "child: the AskUserQuestion PreToolUse hook writes no marker ($kind)"; else bad "child: the AskUserQuestion PreToolUse hook writes no marker ($kind)"; fi
done
# The row's own agent does consume it (the answer earns the prompt).
ASKQ="$SOT_COMM_HOME/state/askq-agl-q-own.marker"; : > "$ASKQ"
printf '%s' '{"tool_name":"AskUserQuestion","tool_use_id":"agl-q-own"}' | CLAUDE_CODE_SESSION_ID=agl-nc-q-own chain own "$SELF_ROW" bash "$FLAT/comm-status-heartbeat.sh" >/dev/null 2>&1 || true
if [ ! -f "$ASKQ" ]; then ok "own: the heartbeat consumes the AskUserQuestion marker"; else bad "own: the heartbeat consumes the AskUserQuestion marker"; fi
# The row's own agent: the PreToolUse hook stamps blocked and the marker appears (written by
# comm-status.sh, not by the hook); the heartbeat's PostToolUse consumes it. A manual
# `comm-status.sh blocked` carries no tool_use_id and writes no marker.
QM="$SOT_COMM_HOME/state/askq-agl-qb-own.marker"; rm -f "${QM:?}"
printf '%s' '{"tool_name":"AskUserQuestion","tool_use_id":"agl-qb-own"}' | CLAUDE_CODE_SESSION_ID=agl-qb-own chain own "$SELF_ROW" bash "$HOOKS_DIR/comm-status-blocked.sh" >/dev/null 2>&1 || true
if [ -f "$QM" ]; then ok "own: the AskUserQuestion PreToolUse hook leaves its marker"; else bad "own: the AskUserQuestion PreToolUse hook leaves its marker"; fi
printf '%s' '{"tool_name":"AskUserQuestion","tool_use_id":"agl-qb-own"}' | CLAUDE_CODE_SESSION_ID=agl-qb-own2 chain own "$SELF_ROW" bash "$FLAT/comm-status-heartbeat.sh" >/dev/null 2>&1 || true
if [ ! -e "$QM" ]; then ok "own: the heartbeat's PostToolUse consumes that marker"; else bad "own: the heartbeat's PostToolUse consumes that marker"; fi
nq0="$(ls "$SOT_COMM_HOME/state" | grep -c '^askq-' || true)"
run own "$SELF_ROW" "$STATUS" blocked "a manual question"; eq "own: a manual comm-status.sh blocked succeeds" "$RC" 0
eq  "own: a manual comm-status.sh blocked writes no marker" "$(ls "$SOT_COMM_HOME/state" | grep -c '^askq-' || true)" "$nq0"
run own "$SELF_ROW" "$STATUS" working y; run own "$SELF_ROW" "$STATUS" waiting x   # leave the row as it was
# A child neither spawns nor despawns rows: every form refuses, whatever --task says,
# and the comm home (registry, inboxes, the row's own entry) is byte for byte as it was.
for v in "--name h $WORK" "--name h $WORK --task t" "h $WORK" "h $WORK --task t" "$WORK" "--name h $WORK --endpoint unix:/nonexistent"; do
    for kind in child nodechild; do
        before="$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")"
        run "$kind" "$SELF_ROW" "$SCRIPTS_DIR/comm-spawn.sh" $v
        eq  "child: comm-spawn.sh $v ($kind) exits 1" "$RC" 1
        has "child: comm-spawn.sh $v ($kind) names the cause" "$OUT" "has no comm identity"
        if [ "$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")" = "$before" ]; then ok "child: comm-spawn.sh $v ($kind) changes nothing under the comm home"; else bad "child: comm-spawn.sh $v ($kind) changed the comm home"; fi
    done
done
for v in "h" "$ROW" "h --endpoint unix:/nonexistent" "--endpoint unix:/nonexistent h"; do
    for kind in child nodechild; do
        before="$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")"
        run "$kind" "$SELF_ROW" "$SCRIPTS_DIR/comm-despawn.sh" $v
        eq  "child: comm-despawn.sh $v ($kind) exits 1" "$RC" 1
        has "child: comm-despawn.sh $v ($kind) names the cause" "$OUT" "has no comm identity"
        if [ "$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")" = "$before" ]; then ok "child: comm-despawn.sh $v ($kind) changes nothing under the comm home"; else bad "child: comm-despawn.sh $v ($kind) changed the comm home"; fi
    done
done
# comm-probe.sh makes and stops rows: a child refuses it, before the home or any request.
for v in up down serve status "" --help bogus; do
    for kind in child nodechild; do
        before="$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")"
        run "$kind" "$SELF_ROW" "$SCRIPTS_DIR/comm-probe.sh" $v
        eq  "child: comm-probe.sh $v ($kind) exits 1" "$RC" 1
        has "child: comm-probe.sh $v ($kind) names the cause" "$OUT" "has no comm identity"
        if [ "$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")" = "$before" ]; then ok "child: comm-probe.sh $v ($kind) changes nothing under the comm home"; else bad "child: comm-probe.sh $v ($kind) changed the comm home"; fi
    done
done
# comm-worktree-new.sh gates before any git write: a child refuses it, and no worktree or branch is made.
WTR="$WORK/wtp/repo"; mkdir -p "$WTR"
{ git init -q "$WTR" && git -C "$WTR" config core.hooksPath /dev/null \
    && git -C "$WTR" -c user.name=t -c user.email=t@example.invalid -c commit.gpgsign=false commit -q --allow-empty -m init; } >/dev/null 2>&1 \
    || { echo "FATAL: setup git repo for comm-worktree-new.sh" >&2; exit 1; }
for v in "c1" "c1 --no-spawn"; do
    for kind in child nodechild; do
        before="$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")"
        run "$kind" "$SELF_ROW" bash -c 'cd "$1" && shift && exec "$@"' _ "$WTR" "$SCRIPTS_DIR/comm-worktree-new.sh" $v
        eq  "child: comm-worktree-new.sh $v ($kind) exits 1" "$RC" 1
        has "child: comm-worktree-new.sh $v ($kind) names the cause" "$OUT" "comm-worktree-new.sh: this process runs under"
        if [ ! -e "$WORK/wtp/worktrees" ] && ! git -C "$WTR" show-ref --quiet --verify refs/heads/wt/c1; then ok "child: comm-worktree-new.sh $v ($kind) makes no worktree or branch"; else bad "child: comm-worktree-new.sh $v ($kind) made a worktree or branch"; fi
        if [ "$(snap "$SOT_COMM_HOME"; cat "$SELF_ROW")" = "$before" ]; then ok "child: comm-worktree-new.sh $v ($kind) changes nothing under the comm home"; else bad "child: comm-worktree-new.sh $v ($kind) changed the comm home"; fi
    done
done
run own "$SELF_ROW" bash -c 'cd "$1" && shift && exec "$@"' _ "$WTR" "$SCRIPTS_DIR/comm-worktree-new.sh" c2 --no-spawn
if [ "$RC" -eq 0 ] && [ -d "$WORK/wtp/worktrees/repo-wt-c2" ] && git -C "$WTR" show-ref --quiet --verify refs/heads/wt/c2; then ok "own: comm-worktree-new.sh c2 --no-spawn makes the worktree and branch"; else bad "own: comm-worktree-new.sh c2 --no-spawn (rc $RC: $OUT)"; fi
run own "$SELF_ROW" bash -c 'cd "$1" && shift && exec "$@"' _ "$WTR" "$SCRIPTS_DIR/comm-worktree-new.sh" --help
eq  "own: comm-worktree-new.sh --help exits 0" "$RC" 0
has "own: comm-worktree-new.sh --help describes --expertise" "$OUT" "--expertise \"...\" comma-separated"
has "own: comm-worktree-new.sh --help describes --display-prefix (the last option)" "$OUT" "--display-prefix L  override"
# A fresh comm home: a refused child makes no registry skeleton.
H2="$WORK/home2"; mkdir -p "$H2"
before="$(snap "$H2")"
for v in "$POLL" "$STATUS working q" "$SCRIPTS_DIR/comm-leave.sh" "$SCRIPTS_DIR/comm-list.sh"; do
    ( export SOT_COMM_HOME="$H2"; chain child "$SELF_LEG" $v ) >/dev/null 2>&1 || true
done
for v in "$SCRIPTS_DIR/comm-context.sh" "$SCRIPTS_DIR/comm-session-start.sh" "$SCRIPTS_DIR/comm-relay.sh send @$PEER x" "$SCRIPTS_DIR/comm-bootstrap.sh some-row" \
         "$SCRIPTS_DIR/comm-spawn.sh --name h $WORK" "$SCRIPTS_DIR/comm-despawn.sh h" "$SCRIPTS_DIR/comm-probe.sh up" "$SCRIPTS_DIR/comm-probe.sh down" \
         "$SCRIPTS_DIR/comm-probe.sh" "$SCRIPTS_DIR/comm-probe.sh --help" "$SCRIPTS_DIR/comm-probe.sh bogus"; do
    ( export SOT_COMM_HOME="$H2"; chain child "$SELF_LEG" $v ) >/dev/null 2>&1 || true
done
if [ "$(snap "$H2")" = "$before" ]; then ok "child: no registry skeleton in a fresh comm home"; else bad "child: a refused verb made files in a fresh comm home: $(snap "$H2" | tr '\n' ' ' | cut -c1-300)"; fi

# A comm home with a registry and no state directory: a refused child's heartbeat
# makes no state directory and no tick; the row's own agent makes both.
H3="$WORK/home3"; mkdir -p "$H3"; cp "$REG" "$H3/registry.json"
before="$(snap "$H3")"
printf '{"tool_name":"Bash"}' | ( export SOT_COMM_HOME="$H3" CLAUDE_CODE_SESSION_ID=agl-h3; chain child "$SELF_LEG" bash "$FLAT/comm-status-heartbeat.sh" ) >/dev/null 2>&1 || true
if [ "$(snap "$H3")" = "$before" ]; then ok "child: the heartbeat makes no state directory or tick in a comm home with a registry"; else bad "child: the heartbeat made files in a comm home with a registry: $(snap "$H3" | tr '\n' ' ' | cut -c1-300)"; fi
printf '{"tool_name":"Bash"}' | ( export SOT_COMM_HOME="$H3" CLAUDE_CODE_SESSION_ID=agl-h3; chain own "$SELF_LEG" bash "$FLAT/comm-status-heartbeat.sh" ) >/dev/null 2>&1 || true
if [ "$(snap "$H3")" != "$before" ]; then ok "own: the same heartbeat makes its tick"; else bad "own: the same heartbeat makes its tick"; fi

# --- the leave gate: a child cannot remove a row, the row's own agent can --------
LV="leaver"
run own "$WORK/self-lv.txt" "$JOIN" --name "$LV"; eq "setup: join $LV" "$RC" 0
run child "$WORK/self-lv.txt" "$SCRIPTS_DIR/comm-leave.sh"
eq  "child: leave refuses" "$RC" 1
has "child: leave names the cause" "$OUT" "has no comm identity"
if jq -e --arg n "$LV" '.agents[$n]' "$REG" >/dev/null 2>&1; then ok "child: the row is still registered"; else bad "child: the row is still registered"; fi
run own "$WORK/self-lv.txt" "$SCRIPTS_DIR/comm-leave.sh"
eq  "own: leave succeeds" "$RC" 0
if jq -e --arg n "$LV" '.agents[$n]' "$REG" >/dev/null 2>&1; then bad "own: leave removed the row"; else ok "own: leave removed the row"; fi

# --- an ancestry that cannot be read is refused, not trusted ------------------------
# The library is a copy with the chain walk forced to fail (a box with no /proc
# and no ps); the scripts and hooks run from it through $SOT_COMM_HOME/bin.
# (An unreadable /proc cannot be simulated, so that route is covered by the
# table's truncated-record rows above only.)
BIN_T="$WORK/scripts-notree"; cp -r "$SCRIPTS_DIR" "$BIN_T" && chmod -R u+w "$BIN_T" \
    && printf '\n_sot_ancestor_chain() { return 1; }\n' >> "$BIN_T/comm-lib.sh" \
    && grep -q '^_sot_ancestor_chain() { return 1; }$' "$BIN_T/comm-lib.sh" || { echo "FATAL: cannot build the no-ancestry copy" >&2; exit 1; }
ln -sfn "$BIN_T" "$SOT_COMM_HOME/bin"
run own "$SELF_ROW" "$BIN_T/comm-poll.sh"
eq  "no ancestry: poll exits 1" "$RC" 1
has "no ancestry: poll says why" "$OUT" "$R_TREE_TEXT"
run own "$SELF_ROW" "$BIN_T/comm-status.sh" working y
eq  "no ancestry: status working exits 1" "$RC" 1
has "no ancestry: status says why" "$OUT" "$R_TREE_TEXT"
run own "$SELF_ROW" "$BIN_T/comm-send.sh" "@$PEER" x
eq  "no ancestry: send exits 1" "$RC" 1
stop_hook own
has   "no ancestry: the Stop hook says why in a systemMessage" "$OUT" '"systemMessage":"sot-comm: '
hasnt "no ancestry: the Stop hook does not block" "$OUT" '"decision"'
backdate; reg4="$(sum "$REG")"
heartbeat own agl-hb-notree
eq  "no ancestry: the heartbeat leaves the registry alone" "$(sum "$REG")" "$reg4"
has "no ancestry: the heartbeat says why on stderr" "$OUT" "$R_TREE_TEXT"
ln -sfn "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"
# A chain of 70 launchers between the tool and its agent is past the cap.
run deep "$SELF_ROW" "$POLL"
eq  "a 70-deep chain of launchers is refused" "$RC" 1
has "a 70-deep chain says the ancestry cannot be read" "$OUT" "$R_TREE_TEXT"
# A library too old to hold the gate (rc 127) is not a child either.
BIN_O="$WORK/scripts-oldlib"; cp -r "$SCRIPTS_DIR" "$BIN_O" && chmod -R u+w "$BIN_O" \
    && printf '\nsot_require_agent() { return 127; }\n' >> "$BIN_O/comm-lib.sh" || { echo "FATAL: cannot build the old-lib copy" >&2; exit 1; }
ln -sfn "$BIN_O" "$SOT_COMM_HOME/bin"
stop_hook own
has   "an old lib (rc 127): the Stop hook says so in a systemMessage" "$OUT" '"systemMessage":"sot-comm: '
hasnt "an old lib (rc 127): the Stop hook does not block" "$OUT" '"decision"'
ln -sfn "$SCRIPTS_DIR" "$SOT_COMM_HOME/bin"

# --- the row rule, end to end: a process naming a row must run inside that row's capsule ---
mkdir -p "$WORK/self"
RA="row-a"; SELF_WA="$WORK/self/testhost__ws-a.txt"; CUR_A="$SOT_COMM_HOME/read/$RA.cursor"
ROWID=ws-a; run rown "$SELF_WA" "$JOIN" --name "$RA" || true; [ "$RC" -eq 0 ] || { echo "FATAL: setup join $RA in its row: $OUT" >&2; exit 1; }
run own "$SELF_PEER" "$SEND" "@$RA" "frame-row"; [ "$RC" -eq 0 ] || { echo "FATAL: setup send to $RA: $OUT" >&2; exit 1; }
cur0="$(sum "$CUR_A")"
ROWID=ws-b; run rown "$SELF_WA" "$POLL"
eq  "row rule: poll beneath ws-b's capsule, naming ws-a, exits 1 (the library's rc 2)" "$RC" 1
has "row rule: that refusal names the cause" "$OUT" "$R_ROW_TEXT"
hasnt "row rule: that poll shows no frame" "$OUT" "frame-row"
eq  "row rule: that poll leaves the cursor alone" "$(sum "$CUR_A")" "$cur0"
ROWID=ws-a; run rown "$SELF_WA" "$POLL"
eq  "row rule: poll beneath ws-a's capsule, naming ws-a, succeeds" "$RC" 0
# The Stop hook shows the refusal and stamps nothing.
reg0="$(sum "$REG")"
OUT="$(printf '{}' | CLAUDE_CODE_SESSION_ID=agl-stop-row ROWID=ws-b chain rown "$SELF_WA" bash "$HOOKS_DIR/comm-status-idle.sh" 2>&1)"; rc_of
eq  "row rule: the Stop hook beneath ws-b, naming ws-a, exits 0" "$RC" 0
has "row rule: the Stop hook shows a systemMessage" "$OUT" '"systemMessage"'
has "row rule: the Stop hook names the cause" "$OUT" "$R_ROW_TEXT"
eq  "row rule: the Stop hook leaves the registry alone" "$(sum "$REG")" "$reg0"
# An orphan: its shell exits at once, so no ws-a capsule is above it.
ORC="$WORK/orphan.rc"; rm -f "${ORC:?}"
( cd "$WORK" && export SOT_COMM_SELF_FILE="$SELF_WA" RCF && in_row ws-a bash -c '( sleep 1; "$@" > "$0.out" 2>&1; echo $? > "$0" ) < /dev/null > /dev/null 2>&1 & exit 0' "$ORC" "$POLL" ) > /dev/null 2>&1
for ((i = 0; i < 100; i++)); do [ -s "$ORC" ] && break; sleep 0.1; done
eq  "row rule: an orphan naming ws-a, reparented away from its capsule, exits 1 (the library's rc 2)" "$(cat "$ORC" 2>/dev/null)" 1
has "row rule: the orphan's refusal names the cause" "$(cat "$ORC.out" 2>/dev/null)" "$R_ROW_TEXT"

# --- the Windows walk, end to end: every comm script started as a script, as on the box ---
# SIMWIN turns on the stand-ins appended to the scripts' copy: an argv[0] ending in .exe
# is native, anything else MSYS, and an MSYS process started by an MSYS program other
# than a fork of itself has no live Windows parent (an MSYS exec).
SIMWIN="$WORK/simwin"; mkdir -p "$SIMWIN"
cat > "$SIMWIN/sotd" <<'SIMSOTD'
#!/usr/bin/env bash
# sotd.exe ancestors on a Linux "Windows": from the parent of --from's process
# (default: this one), parent first, one `<pid>\t<exe>\t<command line>` line each,
# stopping after an MSYS process whose parent is an MSYS process with another
# command line (an MSYS exec: its old Windows process has exited).
case "$#:${1:-}:${2:-}" in
    1:ancestors:) s=$$ ;;
    3:ancestors:--from) s=$3 ;;
    *) exit 2 ;;
esac
a0()   { local a=""; IFS= read -r -d '' a < "/proc/$1/cmdline" 2>/dev/null; printf '%s' "$a"; }
cl()   { local a out=""; while IFS= read -r -d '' a; do case "$a" in *[\ \"]*) a="\"${a//\"/\\\"}\"" ;; esac; out="$out${out:+ }$a"; done < "/proc/$1/cmdline" 2>/dev/null; printf '%s' "$out"; }
ppid() { local l; IFS= read -r l < "/proc/$1/stat" 2>/dev/null || return 1; l="${l##*) }"; l="${l#* }"; printf '%s' "${l%% *}"; }
p="$(ppid "$s")" || exit 1
while [ "$p" -gt 1 ] && [ -r "/proc/$p/cmdline" ]; do
    n0="$(a0 "$p")"; c="$(cl "$p")"
    printf '%s\t%s\t%s\n' "$p" "${n0##*/}" "$c"
    pp="$(ppid "$p")" || break
    case "$n0" in *.exe) ;; *)
        case "$(a0 "$pp")" in *.exe|'') ;; *) [ "$c" = "$(cl "$pp")" ] || break ;; esac ;;
    esac
    p="$pp"
done
exit 0
SIMSOTD
chmod +x "$SIMWIN/sotd"
# The 6b probe: run by a second agent's own process, as a script.
cat > "$SIMWIN/probe.sh" <<PROBE
export SOT_COMM_HOME="$SIMWIN/home-6b"
. "$SCRIPTS_DIR/comm-lib.sh"
why="\$(sot_require_agent)"; echo "require rc=\$? \$why"
bash "$SCRIPTS_DIR/comm-send.sh" @$PEER from-6b 2>&1; echo "send rc=\$?"
bash "$SCRIPTS_DIR/comm-context.sh" > /dev/null 2>&1; echo "context rc=\$?"
echo "home entries: \$(ls -A "$SIMWIN/home-6b" | wc -l | tr -d ' ')"
PROBE
in_wrow() {  # ID CMD... : CMD beneath a native stand-in for row ID's capsule
    local id="$1"; shift
    ( exec -a sot-capsule.exe bash -c 'shift; "$@"; exit $?' _ "/s/workspaces/$id/voyages/v0" "$@" )
}
RW="row-w"; SELF_WW="$WORK/self/testhost__ws-w.txt"
ROWID=ws-w; run rown "$SELF_WW" "$JOIN" --name "$RW" || true
eq  "windows walk: setup, $RW joins in its row (a Linux walk)" "$RC" 0
run own "$SELF_PEER" "$SEND" "@$RW" "frame-w"
eq  "windows walk: setup, the peer sends to $RW (a Linux walk)" "$RC" 0
export SIMWIN
run wown "$SELF_WW" "$POLL"
eq  "windows walk: the row's own agent polls from a script started as a script" "$RC" 0
has "windows walk: that poll shows the frame" "$OUT" "frame-w"
run wown "$SELF_WW" bash "$WORK/hold.sh" "$POLL"
eq  "windows walk: the row's own agent polls from a script started from a script" "$RC" 0
WCOD=(bash "$WORK/fake.sh" codex.exe hold.sh bash -c '"$@"; exit $?' _ "$POLL")
run wown "$SELF_WW" "${WCOD[@]}"
eq  "windows walk: a poll under a native codex.exe in the tool shell refuses" "$RC" 1
has "windows walk: that refusal names claude's session" "$OUT" "$WCX"
for v in "$SELF_WW" "$WORK/self/self-x.txt"; do
    case "$v" in "$SELF_WW") lbl="naming ws-w" ;; *) lbl="naming no row" ;; esac
    run wown "$v" bash "$WORK/hold.sh" "${WCOD[@]}"
    eq  "windows walk: codex.exe started by a script, $lbl: poll refuses" "$RC" 1
    has "windows walk: codex.exe started by a script, $lbl: the walk goes on past that script to claude" "$OUT" "$WCX"
    rm -rf "${SIMWIN:?}/home-6b"; mkdir -p "$SIMWIN/home-6b"
    run wown "$v" bash "$WORK/fake.sh" codex "$SIMWIN/probe.sh"
    has "windows walk: 6b, $lbl: the probe's own gate counts claude above codex" "$OUT" "require rc=1 this process $WCX"
    has "windows walk: 6b, $lbl: its send is refused at the gate" "$OUT" "FAILED -> @$PEER: this process $WCX"
    has "windows walk: 6b, $lbl: send rc=1" "$OUT" "send rc=1"
    has "windows walk: 6b, $lbl: neither the send nor comm-context.sh wrote to the empty comm home" "$OUT" "home entries: 0"
done
unset SIMWIN

# --- the matrix runner's private copy of the probe row's self file ---------------
SELF_P2="$WORK/self/testhost__ws-p2.txt"; PRIVD="$WORK/matrix-priv"; mkdir -p "$PRIVD"
ROWID=ws-p2; run rown "$SELF_P2" "$JOIN" --name probe2-testhost || true; [ "$RC" -eq 0 ] || { echo "FATAL: setup join probe2-testhost: $OUT" >&2; exit 1; }
PRIV="$( . "$SCRIPT_DIR/comm-matrix.sh" > /dev/null 2>&1; matrix_private_self "$SELF_P2" "$PRIVD" 2> /dev/null )"
PINBOX="$SOT_COMM_HOME/inbox/$PEER.jsonl"; pin0="$(sum "$PINBOX")"
ROWID=ws-dev; run rown "$SELF_P2" "$SEND" "@$PEER" "matrix-orig"
eq  "matrix: sending from the developer's row with the probe row's own self file exits 1 (the library's rc 2)" "$RC" 1
has "matrix: that refusal names the cause" "$OUT" "$R_ROW_TEXT"
eq  "matrix: that send filed nothing" "$(sum "$PINBOX")" "$pin0"
run rown "$PRIV" "$SEND" "@$PEER" "matrix-copy"
eq  "matrix: sending with the private copy exits 0" "$RC" 0
has "matrix: the private copy's send is filed" "$OUT" "filed -> @$PEER"
has "matrix: the peer's inbox line carries the probe row as sender" "$(tail -n 1 "$PINBOX" 2>/dev/null)" "probe2-testhost"

echo "agent layers: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
