# Part of ../test-agent-layers.sh: sections 1 to 1d, the layer table and its parsers; sourced where they stood.
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

[ -z "$LINUX" ] || {  # the real chain walk reads Linux's /proc
# A parse that does not finish is refused too: a filter that dies with no output
# (no awk), and a sotd.exe whose exit status is not 0 or 3.
mkdir -p "$WORK/badawk"; printf '#!/bin/sh\nexit 1\n' > "$WORK/badawk/awk"; chmod +x "$WORK/badawk/awk"
res="$( PATH="$WORK/badawk:$PATH"; o="$(sot_require_agent)"; echo "rc=$?|$o" )"
case "$res" in "rc=2|"*"$R_TREE_TEXT"*) ok "require: an awk that exits 1 with no output is refused" ;; *) bad "require: an awk that exits 1 with no output is refused (got: $res)" ;; esac
}
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

if [ -z "$LINUX" ]; then
    echo "SKIP: the end-to-end chains, the badawk case and SIMWIN read Linux's /proc; the tables and bridge fixtures above ran"
    echo "agent layers: $PASS passed, $FAIL failed"
    [ "$FAIL" -eq 0 ]; exit
fi

