#!/usr/bin/env bash
# done-test.sh — the 0.6.6 acceptance run. The Claude session of a box runs it
# from its own row (Linux bash or Windows git-bash), output captured to a file
# it names. One line per item — `PASS <id> <detail>`, `FAIL <id> <detail>` or
# `MANUAL <id> <what to look at>` — then one summary line
# `DONE-TEST <sotd version> <n> PASS <n> FAIL <n> MANUAL`. Exit 0 only with no
# FAIL. NOT product code: never merged into a release branch.
#
# Usage:
#   done-test.sh run [--peer @handle] [--only M1,M2,...] --out <file>
#   done-test.sh snapshot <file>   before an install: rows + a kept row P
#   done-test.sh compare <file>    after the install: I1, I2, then removes P
#   done-test.sh list
#
# Every row this script spawns lives under dev/output/done-test-rows/ of the
# checkout it runs from and is despawned on exit (row P of `snapshot` is kept
# for `compare`). It never types into, reads or closes any other row. It
# reads the driver's inbox by line count and never polls it, so the replies
# stay unread for the driver.
set -uo pipefail

BIN="$HOME/.sot-comm/bin"
# DT_SPAWN_BIN: where comm-spawn.sh/comm-despawn.sh come from (a checkout's comm/core/scripts for a test run).
SPAWN_BIN="${DT_SPAWN_BIN:-$BIN}"
SOTFE="$BIN/sot-fe"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
ROWS_DIR="$ROOT/dev/output/done-test-rows"
NONCE="$(printf '%04x%04x' "$RANDOM" "$RANDOM")"
WAKE_TEXT='[sot-comm] you have mail'
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) WIN=1 ;; *) WIN=0 ;; esac

ITEMS="M1 M2 M3 M4 M5 M6 M7 C1 C2 C3"

list_items() {
    cat <<'EOF'
M1  own send and poll: spawned row A replies ok to the driver within 120 s
M2  idle row woken: mail to idle row A shows the wake line within 10 s, reply within 120 s
M3  renamed row woken: as M2, for row B whose --name differs from its folder name
M4  background sub-agent: as M2, while A has a background sub-agent running (+ MANUAL: agent view refused)
M5  second agent refused: a `claude -p` under the driver cannot send as the driver
M6  background service refused: /background in row B is refused, session id kept, nothing moved out
M7  cross-machine mail: --peer @h replies with a nonce within 15 min (SKIP without --peer)
C1  close leaves nothing behind: despawn A, then /exit in B, start no background service
C2  MANUAL: the Ctrl-Q dialog
C3  MANUAL: the window (hull)
I1  (snapshot/compare) an install or converge keeps every row and its phase
I2  (snapshot/compare) /background in a row open across the install is refused as in M6
EOF
}

usage() {
    sed -n '9,13p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

# --- output -----------------------------------------------------------------
OUT=""; LOG="/dev/null"
NPASS=0; NFAIL=0; NMANUAL=0
emit() {
    local line="$*"
    printf '%s\n' "$line"
    [ -n "$OUT" ] && printf '%s\n' "$line" >> "$OUT"
    case "$1" in
        PASS) NPASS=$((NPASS + 1)) ;;
        FAIL) NFAIL=$((NFAIL + 1)) ;;
        MANUAL) NMANUAL=$((NMANUAL + 1)) ;;
    esac
    log "$line"
}
log() { printf '%s %s\n' "$(date -u +%H:%M:%S)" "$*" >> "$LOG"; }
summary() { emit "DONE-TEST ${SOTD_VERSION:-unknown} $NPASS PASS $NFAIL FAIL $NMANUAL MANUAL"; }

now_ms() { date +%s%3N; }
since() { awk -v a="$1" -v b="$(now_ms)" 'BEGIN { printf "%.1f", (b - a) / 1000 }'; }

# --- the driver -------------------------------------------------------------
# comm-context.sh prints either a NAME=/INBOX_DIR= block or the bare handle.
load_driver() {
    local ctx
    ctx="$("$BIN/comm-context.sh" 2>/dev/null)" || true
    DRIVER="$(printf '%s\n' "$ctx" | sed -n 's/^NAME=//p' | head -n1)"
    INBOX_DIR="$(printf '%s\n' "$ctx" | sed -n 's/^INBOX_DIR=//p' | head -n1)"
    if [ -z "$DRIVER" ] && [ "$(printf '%s\n' "$ctx" | grep -c .)" = 1 ]; then
        DRIVER="$(printf '%s' "$ctx" | tr -d '@[:space:]')"
    fi
    [ -n "$INBOX_DIR" ] || INBOX_DIR="$HOME/.sot-comm/inbox"
    if [ -z "$DRIVER" ]; then
        echo "done-test: comm-context.sh named no handle; run this from a joined row" >&2
        exit 2
    fi
    INBOX="$INBOX_DIR/$DRIVER.jsonl"
}

load_version() {
    VERSION_OUT="$("$SOTFE" version 2>/dev/null)" || true
    SOTD_VERSION="$(printf '%s\n' "$VERSION_OUT" | awk '$1 ~ /:/ && $2 ~ /^[0-9]+\./ { print $2; exit }')"
    [ -n "$SOTD_VERSION" ] || SOTD_VERSION="unknown"
}
row_lines() { printf '%s\n' "$VERSION_OUT" | grep '^row ' | awk '{ $1 = $1; print }'; }

# --- the driver's inbox, read by line count, never polled -------------------
inbox_count() {
    if [ -f "$INBOX" ]; then wc -l < "$INBOX" | tr -d '[:space:]'; else echo 0; fi
}
inbox_new() { [ -f "$INBOX" ] && tail -n +"$(($1 + 1))" "$INBOX"; }
# wait_from BASE HANDLE SECS — a line from HANDLE lands after line BASE.
wait_from() {
    local end=$(($(date +%s) + $3))
    while [ "$(date +%s)" -lt "$end" ]; do
        inbox_new "$1" | jq -R -r 'fromjson? | .from // empty' 2>/dev/null | tr -d '\r' \
            | sed 's/^@//' | grep -qxF "$2" && return 0
        sleep 2
    done
    return 1
}
# wait_text BASE TEXT SECS — a line containing TEXT lands after line BASE.
wait_text() {
    local end=$(($(date +%s) + $3))
    while [ "$(date +%s)" -lt "$end" ]; do
        inbox_new "$1" | grep -qF "$2" && return 0
        sleep 2
    done
    return 1
}

# --- rows ---------------------------------------------------------------------
SPAWNED=(); GONE=(); KEEP=()
cleanup() {
    local ws
    for ws in ${SPAWNED[@]+"${SPAWNED[@]}"}; do
        case " ${GONE[*]-} ${KEEP[*]-} " in *" $ws "*) continue ;; esac
        log "despawn $ws (exit)"
        "$SPAWN_BIN/comm-despawn.sh" "$ws" >> "$LOG" 2>&1 \
            || echo "done-test: could not despawn $ws; remove it with $SPAWN_BIN/comm-despawn.sh $ws" >&2
    done
}
trap cleanup EXIT
trap 'exit 130' INT TERM

despawn() {
    log "despawn $1"
    "$SPAWN_BIN/comm-despawn.sh" "$1" >> "$LOG" 2>&1 && GONE+=("$1")
}

# spawn_row BASE NAME TASK — a throwaway row in $ROWS_DIR/BASE; an empty NAME
# lets comm-spawn derive the handle. Sets R_WS R_NAME R_SLUG R_DIR. The ws id
# is armed for despawn even when comm-spawn fails after creating the row.
spawn_row() {
    local base="$1" name="$2" task="$3" out rc args
    R_WS=""; R_NAME=""; R_SLUG=""
    mkdir -p "$ROWS_DIR/$base"
    R_DIR="$(cd "$ROWS_DIR/$base" && pwd -P)"
    # DT_AGENT_VIEW_OFF=1: the row starts with Claude Code's agent view off, as a row on the fixed version does.
    if [ "${DT_AGENT_VIEW_OFF:-}" = 1 ]; then
        mkdir -p "$R_DIR/.claude" && printf '{\n  "disableAgentView": true\n}\n' > "$R_DIR/.claude/settings.local.json"
    fi
    args=("$R_DIR")
    [ -n "$name" ] && args+=(--name "$name")
    [ -n "$task" ] && args+=(--task "$task")
    out="$("$SPAWN_BIN/comm-spawn.sh" "${args[@]}" 2>&1)"; rc=$?
    log "spawn $base rc=$rc"
    printf '%s\n' "$out" >> "$LOG"
    R_WS="$(printf '%s\n' "$out" | grep -o 'id=ws-[A-Za-z0-9_.-]*' | head -n1 | cut -d= -f2)"
    [ -n "$R_WS" ] && SPAWNED+=("$R_WS")
    R_NAME="$(printf '%s\n' "$out" | sed -n 's/^Spawned @\([^ ]*\) as workspace.*/\1/p' | head -n1)"
    R_SLUG="$(printf '%s\n' "$out" | sed -n 's/.*(slug=\([^,]*\),.*/\1/p' | head -n1)"
    [ "$rc" = 0 ] && [ -n "$R_WS" ] && [ -n "$R_NAME" ]
}

screen() { "$SOTFE" screen "$1" --timeout 3 2>/dev/null; }
dump_screen() { { printf -- '--- screen %s (%s)\n' "$1" "$2"; screen "$1"; } >> "$LOG" 2>&1; }
type_into() {
    log "type $1: $2"
    TYPED_MARK="${2:0:30}"
    printf '%s' "$2" | "$SOTFE" type "$1" --stdin --enter --origin "$DRIVER" >> "$LOG" 2>&1
}

# A row is idle when no turn is in progress, a turn-done line or a footer agent line ("◯ <name>", a turn that ended
# with a background sub-agent running prints no done line) is on screen, and the input line holds nothing typed.
# After such a turn Claude Code shows a ghost suggestion after the prompt; the screen text carries no attribute that
# tells it from typed input, so text after the last prompt is accepted only when a done or footer agent line sits below
# the last line this script typed (TYPED_MARK, its first 30 characters; unset or scrolled away counts as no constraint).
TYPED_MARK=""
is_idle() {
    printf '%s\n' "$1" | LC_ALL=C grep -qE '…[[:space:]]*\([0-9]+(m [0-9]+)?s' && return 1
    printf '%s\n' "$1" | LC_ALL=C awk -v m="$TYPED_MARK" '
        { sub(/\xc2\xa0/, " ") }
        m != "" && index($0, m) { k = NR }
        /✻ .* done |^ *◯ / { if (NR > k) d = NR }
        /^❯/ { p = NR; t = $0; sub(/^❯[ \t]*/, "", t); sub(/[ \t]+$/, "", t); txt = t }
        END { exit !(d && (txt == "" || p > k)) }'
}
# wait_idle WS SECS — sets LAST_SCREEN.
wait_idle() {
    local end=$(($(date +%s) + $2))
    while :; do
        LAST_SCREEN="$(screen "$1")"
        is_idle "$LAST_SCREEN" && return 0
        [ "$(date +%s)" -lt "$end" ] || return 1
        sleep 2
    done
}
footer_id() { printf '%s\n' "$1" | grep -oE '\[[0-9a-f]{8}\]' | tail -n1 | tr -d '[]'; }
# A fresh wake line sits below the last turn-done line; an old one is above it.
wake_fresh() {
    printf '%s\n' "$1" | LC_ALL=C awk -v w="$WAKE_TEXT" '
        /✻ .* done / { d = NR }
        index($0, w) { k = NR }
        END { exit !(k > d) }'
}

# More than one [sot-comm] notice below the last turn-done line: two typers raced on one mail (an old watcher
# beside the daemon), and their lines land glued with the Enter lost.
wake_doubled() {
    printf '%s\n' "$1" | LC_ALL=C awk '/✻ .* done / { n = 0 } { n += gsub(/\[sot-comm\]/, "&") } END { exit !(n > 1) }'
}

# --- processes ----------------------------------------------------------------
# procs ERE — pid<TAB>cwd<TAB>argv0<TAB>command line for every process whose
# command line matches. Linux: this user's, cwd from /proc. Windows: cwd is
# empty and argv0 is the image name.
procs() {
    if [ "$WIN" = 1 ]; then
        powershell -NoProfile -Command \
            'Get-CimInstance Win32_Process | Where-Object CommandLine -match "'"$1"'" | ForEach-Object { "$($_.ProcessId)`t`t$($_.Name)`t$($_.CommandLine)" }' \
            2>/dev/null | tr -d '\r' | grep -v 'Get-CimInstance'
    else
        local pid cwd a0 cmd
        for pid in $(pgrep -u "$(id -u)" -f "$1"); do
            [ -r "/proc/$pid/cmdline" ] || continue
            cwd="$(readlink "/proc/$pid/cwd" 2>/dev/null)"
            a0="$(tr '\0' '\n' < "/proc/$pid/cmdline" 2>/dev/null | head -n1)"
            cmd="$(tr '\0' ' ' < "/proc/$pid/cmdline" 2>/dev/null)"
            [ -n "$cmd" ] && printf '%s\t%s\t%s\t%s\n' "$pid" "$cwd" "$a0" "$cmd"
        done
    fi
}
if [ "$WIN" = 1 ]; then BG_ERE='bg-pty-host|bg-spare|daemon run'; else BG_ERE='claude daemon run|bg-pty-host|bg-spare'; fi
bg_procs() { procs "$BG_ERE"; }
claude_procs() {
    procs 'claude' | awk -F'\t' '{ n = $3; gsub(/\\/, "/", n) }
        n ~ /(^|\/)claude(\.exe)?$/ || n ~ /\/claude\/versions\// { print }'
}
pids_of() { cut -f1 | sort -u; }
# at_dir DIR — keep the process lines tied to DIR: its cwd (Linux) or a
# command line naming it (Windows, compared slash- and case-folded).
at_dir() {
    if [ "$WIN" = 1 ]; then
        local want l
        want="$(cygpath -m "$1" | tr 'A-Z\\' 'a-z/')"
        while IFS= read -r l; do
            printf '%s' "$l" | cut -f4- | tr 'A-Z\\' 'a-z/' | sed 's#//*#/#g' | grep -qF "$want" \
                && printf '%s\n' "$l"
        done
    else
        awk -F'\t' -v d="$1" '$2 == d'
    fi
}
# new_pids BASE_PIDS — process lines on stdin whose pid is not in BASE_PIDS.
new_pids() { awk -F'\t' -v base=" $(printf '%s' "$1" | tr '\n' ' ') " 'index(base, " " $1 " ") == 0'; }

claude_bin() {
    local c
    c="$(command -v claude 2>/dev/null)" && { printf '%s' "$c"; return; }
    for c in "$HOME/.local/bin/claude" "$HOME/.local/bin/claude.exe"; do
        [ -x "$c" ] && { printf '%s' "$c"; return; }
    done
    printf 'claude'
}
with_timeout() { if command -v timeout >/dev/null 2>&1; then timeout "$@"; else shift; "$@"; fi; }

# stop_moved DIR — a session /background moved out of the row: `claude stop`
# every one `claude agents` lists, by full id or by its 8-hex prefix.
stop_moved() {
    local ids agents id short
    ids="$( { claude_procs; bg_procs; } | at_dir "$1" | cut -f4- \
        | grep -oE -- '--session-id [0-9a-f-]{36}|pty/[0-9a-f]{8}\.sock' \
        | sed 's/^--session-id //; s#^pty/##; s/\.sock$//' | sort -u)"
    [ -n "$ids" ] || { log "stop_moved: no session id found at $1"; return; }
    agents="$(with_timeout 30 "$(claude_bin)" agents --json 2>&1)"
    log "claude agents --json: $agents"
    for id in $ids; do
        short="${id:0:8}"
        if printf '%s' "$agents" | grep -qF "$id"; then
            log "claude stop $id: $(with_timeout 30 "$(claude_bin)" stop "$id" 2>&1)"
        elif printf '%s' "$agents" | grep -qF "$short"; then
            log "claude stop $short: $(with_timeout 30 "$(claude_bin)" stop "$short" 2>&1)"
        else
            log "stop_moved: $id is not listed by claude agents"
        fi
    done
}

# --- checks shared by items -----------------------------------------------------
# wake_check ID WS HANDLE MESSAGE [PREFIX] — mail to an idle row: the wake line
# within 10 s of `filed`, the reply within 120 s.
wake_check() {
    local id="$1" ws="$2" h="$3" msg="$4" pre="${5:-}" base out t0 tw="" s
    if ! wait_idle "$ws" 180; then
        dump_screen "$ws" "$id not idle"
        emit FAIL "$id" "${pre}row @$h was not idle within 180s"
        return
    fi
    base="$(inbox_count)"
    out="$("$BIN/comm-send.sh" "@$h" "$msg" 2>&1)"
    log "send @$h: $out"
    case "$out" in
        *'filed ->'*|*'(+inbox)'*) ;;
        *) emit FAIL "$id" "${pre}send to @$h was not filed: $(printf '%s' "$out" | tr '\n' ' ')"; return ;;
    esac
    t0="$(now_ms)"
    while [ $(($(now_ms) - t0)) -lt 10000 ]; do
        s="$(screen "$ws")"
        if wake_fresh "$s"; then tw="$(since "$t0")"; sleep 2; s="$(screen "$ws")"; break; fi
        sleep 0.5
    done
    if ! wait_from "$base" "$h" 120; then
        dump_screen "$ws" "$id no reply"
        emit FAIL "$id" "${pre}wake ${tw:-not seen in 10}s, no reply from @$h within 120s"
        return
    fi
    if [ -z "$tw" ]; then
        dump_screen "$ws" "$id no wake line"
        emit FAIL "$id" "${pre}no wake line within 10s of filed; reply after $(since "$t0")s"
        return
    fi
    if wake_doubled "$s"; then
        dump_screen "$ws" "$id doubled wake"
        emit FAIL "$id" "${pre}more than one [sot-comm] notice for one mail (two typers); reply after $(since "$t0")s"
        return
    fi
    emit PASS "$id" "${pre}one wake line ${tw}s after filed, submitted with no Enter by hand, reply after $(since "$t0")s"
}

# background_refused ID WS DIR — /background typed into an idle row is refused:
# the refusal shows within 10 s, the footer session id is unchanged and no
# background-service process is tied to the row folder.
background_refused() {
    local id="$1" ws="$2" dir="$3" id0 id1 t0 seen=no s moved n
    if ! wait_idle "$ws" 180; then
        dump_screen "$ws" "$id not idle"
        emit FAIL "$id" "row was not idle within 180s"
        return
    fi
    id0="$(footer_id "$LAST_SCREEN")"
    type_into "$ws" "/background"
    t0="$(date +%s)"
    while [ $(($(date +%s) - t0)) -lt 10 ]; do
        s="$(screen "$ws")"
        printf '%s\n' "$s" | grep -qF 'Unknown command: /background' && seen=yes
        sleep 1
    done
    s="$(screen "$ws")"
    printf '%s\n' "$s" | grep -qF 'Unknown command: /background' && seen=yes
    id1="$(footer_id "$s")"
    moved="$(bg_procs | at_dir "$dir")"
    n="$(printf '%s' "$moved" | grep -c .)"
    log "background service at $dir: ${moved:-none}"
    if [ "$seen" = yes ] && [ -n "$id0" ] && [ "$id0" = "$id1" ] && [ "$n" = 0 ]; then
        emit PASS "$id" "Unknown command: /background, session [$id0] kept, nothing moved out"
        return
    fi
    dump_screen "$ws" "$id after /background"
    emit FAIL "$id" "refusal shown: $seen, session [${id0:-?}] -> [${id1:-?}], $n background-service process(es) at the row folder"
    stop_moved "$dir"
}

# --- run ------------------------------------------------------------------------
ONLY=""; PEER=""
selected() { [ -z "$ONLY" ] || case ",$ONLY," in *",$1,"*) ;; *) return 1 ;; esac; }

# The rows the items share, brought up once: each greets the driver (that is
# M1 for row A) and is then waited idle. A_ERR / B_ERR say why one is missing.
A_WS=""; A_ERR=""; B_WS=""; B_ERR=""
greet_row() {  # BASE NAME — sets R_* plus R_ERR and R_SECS
    local n0 t0
    R_ERR=""; R_SECS=""
    n0="$(inbox_count)"
    if ! spawn_row "$1" "$2" "reply with the single word ok to @$DRIVER"; then
        R_ERR="comm-spawn failed for $1 (see $LOG)"; return 1
    fi
    t0="$(now_ms)"
    if ! wait_from "$n0" "$R_NAME" 120; then
        dump_screen "$R_WS" "$1 no greeting"
        R_ERR="no reply from @$R_NAME within 120s of its row being ready"; return 1
    fi
    R_SECS="$(since "$t0")"
    if wait_idle "$R_WS" 120; then log "$1 idle"; else log "$1 not idle within 120s"; fi
    dump_screen "$R_WS" "$1 after greeting"
}
ensure_a() {
    [ -n "$A_WS$A_ERR" ] && return
    greet_row "dta-$NONCE" ""
    A_WS="$R_WS"; A_NAME="$R_NAME"; A_DIR="$R_DIR"; A_ERR="$R_ERR"; A_SECS="$R_SECS"
    [ -n "$A_WS" ] || A_ERR="${A_ERR:-row A was not created}"
}
ensure_b() {
    [ -n "$B_WS$B_ERR" ] && return
    greet_row "dtb-$NONCE" "dtb-renamed-$NONCE"
    B_WS="$R_WS"; B_NAME="$R_NAME"; B_DIR="$R_DIR"; B_ERR="$R_ERR"
    [ -n "$B_WS" ] || B_ERR="${B_ERR:-row B was not created}"
}

item_M1() {
    ensure_a
    if [ -n "$A_ERR" ]; then emit FAIL M1 "$A_ERR"; else emit PASS M1 "reply from @$A_NAME ${A_SECS}s after its row was ready"; fi
}
item_M2() {
    ensure_a
    [ -z "$A_ERR" ] || { emit FAIL M2 "row A unavailable: $A_ERR"; return; }
    wake_check M2 "$A_WS" "$A_NAME" "reply ok2 to @$DRIVER"
}
item_M3() {
    ensure_b
    [ -z "$B_ERR" ] || { emit FAIL M3 "row B unavailable: $B_ERR"; return; }
    wake_check M3 "$B_WS" "$B_NAME" "reply ok2 to @$DRIVER" "folder $(basename "$B_DIR"), handle @$B_NAME: "
}
item_M4() {
    m4_check
    emit MANUAL M4 "in row A, opening the background sub-agent's view (agent view) is refused"
}
m4_check() {
    local t0
    ensure_a
    [ -z "$A_ERR" ] || { emit FAIL M4 "row A unavailable: $A_ERR"; return; }
    if ! wait_idle "$A_WS" 180; then emit FAIL M4 "row A was not idle within 180s"; return; fi
    type_into "$A_WS" 'Start one background sub-agent (Agent tool, run_in_background) that runs `timeout 120 tail -f /dev/null` in Bash (a 120 s wait; a plain sleep is blocked), then end your turn.'
    t0="$(date +%s)"
    sleep 5
    if ! wait_idle "$A_WS" 100; then
        dump_screen "$A_WS" "M4 not idle"
        emit FAIL M4 "row A did not end its turn within 105s of starting the sub-agent"
        return
    fi
    if ! printf '%s\n' "$LAST_SCREEN" | LC_ALL=C grep -qE '^ *◯ '; then
        dump_screen "$A_WS" "M4 sub-agent gone"
        emit FAIL M4 "sub-agent no longer running when row A went idle ($(($(date +%s) - t0))s after start); nothing sent"
        return
    fi
    dump_screen "$A_WS" "M4 at the send"
    wake_check M4 "$A_WS" "$A_NAME" "reply ok4 to @$DRIVER" "sub-agent still running at the send (started $(($(date +%s) - t0))s before): "
}
item_M5() {
    local base tmp rc probe
    base="$(inbox_count)"
    tmp="$(mktemp)"
    printf 'Run exactly this one command with the Bash tool, once, then reply with its complete output verbatim: %s\n' \
        "$BIN/comm-send.sh @$DRIVER second-agent-probe" \
        | with_timeout 300 "$(claude_bin)" -p --model haiku --allowedTools "Bash" \
            --output-format stream-json --verbose > "$tmp" 2>&1
    rc=$?
    { echo "--- claude -p (rc=$rc)"; cat "$tmp"; } >> "$LOG"
    probe="$(inbox_new "$base" | grep -cF 'second-agent-probe')"
    if grep -qF 'has no comm identity' "$tmp" && [ "$probe" = 0 ]; then
        emit PASS M5 "the second agent's send was refused (has no comm identity); the driver's inbox gained nothing"
    else
        emit FAIL M5 "refusal in its output: $(grep -qF 'has no comm identity' "$tmp" && echo yes || echo no), second-agent-probe lines in the driver's inbox: $probe (claude -p rc=$rc)"
    fi
    rm -f "$tmp"
}
item_M6() {
    ensure_b
    [ -z "$B_ERR" ] || { emit FAIL M6 "row B unavailable: $B_ERR"; return; }
    background_refused M6 "$B_WS" "$B_DIR"
}
item_M7() {
    local base nonce out t0
    if [ -z "$PEER" ]; then emit SKIP M7; return; fi
    nonce="dt$NONCE$RANDOM"
    base="$(inbox_count)"
    out="$("$BIN/comm-send.sh" "@$PEER" "done-test ping $nonce: reply with the nonce to @$DRIVER" 2>&1)"
    log "send @$PEER: $out"
    case "$out" in
        *'filed ->'*|*'(+inbox)'*) ;;
        *) emit FAIL M7 "send to @$PEER was not filed: $(printf '%s' "$out" | tr '\n' ' ')"; return ;;
    esac
    t0="$(now_ms)"
    if wait_text "$base" "$nonce" 900; then
        emit PASS M7 "@$PEER replied with the nonce after $(since "$t0")s"
    else
        emit FAIL M7 "no line with the nonce from @$PEER within 15 min"
    fi
}
item_C1() {
    local base gone="" t0 left newbg detail="" ok=1
    ensure_a; ensure_b
    if [ -n "$A_ERR" ] || [ -n "$B_ERR" ]; then
        emit FAIL C1 "rows unavailable: ${A_ERR:-A ok}; ${B_ERR:-B ok}"; return
    fi
    base="$(bg_procs | pids_of)"
    despawn "$A_WS" || detail="comm-despawn of A failed; "
    t0="$(date +%s)"
    while :; do
        newbg="$(bg_procs | new_pids "$base")"
        left="$(claude_procs | at_dir "$A_DIR")"
        [ -z "$newbg$left" ] && { gone="$(($(date +%s) - t0))"; break; }
        [ $(($(date +%s) - t0)) -lt 10 ] || break
        sleep 1
    done
    if [ -n "$gone" ]; then
        detail="${detail}despawn A: nothing left after ${gone}s"
    else
        ok=0
        log "after despawn A: new background service: ${newbg:-none}; claude at A: ${left:-none}"
        detail="${detail}despawn A: $(printf '%s' "$newbg" | grep -c .) new background-service, $(printf '%s' "$left" | grep -c .) claude process(es) at A after 10s"
    fi
    base="$(bg_procs | pids_of)"
    type_into "$B_WS" "/exit"
    sleep 10
    newbg="$(bg_procs | new_pids "$base")"
    if [ -n "$newbg" ]; then
        ok=0
        log "after /exit in B: new background service: $newbg"
        detail="$detail; /exit in B: $(printf '%s' "$newbg" | grep -c .) new background-service process(es)"
    else
        detail="$detail; /exit in B: no background service within 10s"
    fi
    if [ "$ok" = 1 ]; then emit PASS C1 "$detail"; else emit FAIL C1 "$detail"; fi
}
item_C2() { emit MANUAL C2 "Ctrl-Q dialog"; }
item_C3() { emit MANUAL C3 "the window (hull)"; }

cmd_run() {
    local it
    while [ $# -gt 0 ]; do
        case "$1" in
            --peer) PEER="${2#@}"; shift 2 ;;
            --only) ONLY="$(printf '%s' "$2" | tr -d '[:space:]')"; shift 2 ;;
            --out)  OUT="$2"; shift 2 ;;
            *) usage ;;
        esac
    done
    [ -n "$OUT" ] || usage
    for it in ${ONLY//,/ }; do
        case " $ITEMS " in *" $it "*) ;; *) echo "done-test: unknown item '$it' (see: done-test.sh list)" >&2; exit 2 ;; esac
    done
    mkdir -p "$(dirname "$OUT")"
    : > "$OUT"; LOG="$OUT.log"; : > "$LOG"
    load_driver; load_version
    log "driver @$DRIVER, sotd $SOTD_VERSION, rows under $ROWS_DIR, nonce $NONCE"
    for it in $ITEMS; do
        selected "$it" && "item_$it"
    done
    summary
    [ "$NFAIL" = 0 ]
}

# --- snapshot / compare -----------------------------------------------------------
cmd_snapshot() {
    local file="${1:-}" rows
    [ -n "$file" ] || usage
    LOG="$file.log"; : >> "$LOG"
    load_driver; load_version
    rows="$(row_lines)"
    if ! spawn_row "dtp-$NONCE" "" ""; then
        echo "done-test: could not spawn row P (see $LOG)" >&2
        exit 1
    fi
    wait_idle "$R_WS" 180 || log "row P not idle within 180s"
    {
        echo "# done-test snapshot $(date -u +%Y-%m-%dT%H:%M:%SZ) sotd $SOTD_VERSION"
        echo "ws $R_WS"
        echo "slug $R_SLUG"
        echo "dir $R_DIR"
        printf '%s\n' "$rows"
    } > "$file"
    KEEP+=("$R_WS")
    echo "snapshot $file: $(printf '%s\n' "$rows" | grep -c .) rows, row P kept open as $R_WS (@$R_NAME)"
}
cmd_compare() {
    local file="${1:-}" pws pslug pdir before after
    [ -n "$file" ] && [ -f "$file" ] || usage
    LOG="$file.log"; : >> "$LOG"
    load_driver; load_version
    pws="$(sed -n 's/^ws //p' "$file")"; pslug="$(sed -n 's/^slug //p' "$file")"; pdir="$(sed -n 's/^dir //p' "$file")"
    [ -n "$pws" ] && SPAWNED+=("$pws")
    before="$(grep '^row ' "$file" | awk -v p="$pslug" '$2 != p' | LC_ALL=C sort)"
    after="$(row_lines | awk -v p="$pslug" '$2 != p' | LC_ALL=C sort)"
    if [ "$before" = "$after" ]; then
        emit PASS I1 "$(printf '%s\n' "$after" | grep -c .) rows and phases unchanged"
    else
        log "rows before:"$'\n'"$before"$'\n'"rows after:"$'\n'"$after"
        emit FAIL I1 "rows changed: $(diff <(printf '%s\n' "$before") <(printf '%s\n' "$after") | grep '^[<>]' | tr '\n' ';')"
    fi
    if [ -z "$pws" ] || [ -z "$pdir" ]; then
        emit FAIL I2 "the snapshot file names no row P"
    else
        background_refused I2 "$pws" "$pdir"
    fi
    summary
    [ "$NFAIL" = 0 ]
}

case "${1:-}" in
    run)      shift; cmd_run "$@" ;;
    snapshot) shift; cmd_snapshot "$@" ;;
    compare)  shift; cmd_compare "$@" ;;
    list)     list_items ;;
    *)        usage ;;
esac
