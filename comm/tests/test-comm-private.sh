#!/usr/bin/env bash
# test-comm-private.sh — ADR 0049, User isolation: the comm folder and everything in it are its user's
# alone. Hermetic: a temp home (never the live comm folder), scripts run from a copy of the staged bin
# whose comm-lib.sh ends with test-hub-files.sh's fixture stubs (no daemon, a fixture mount), a pinned
# $SOT_COMM_TEST_HOST, and the self file left unpinned (the `nopane` slot).
#
#   1. Under umask 022 below a 0755 home no other account can read an inbox, and every folder is 0700
#      and every file 0600: every writer runs (join, the status marker, the record, a library append,
#      the cursor, the heartbeat and Stop hooks, the auditor), bin/ and VERSION excepted.
#   2. A comm folder an older release left (folders 755, files 644) is tightened at the next join;
#      bin/, VERSION and a symlink's target keep their modes.
#   3. Where a comm script starts a writer of the user's own files, that writer keeps the caller's
#      umask: (a) the auditor's `claude -p`, (b) the worktree `comm-worktree-new.sh` makes.
#   4. The Stop hook and the auditor, each the first writer of state/, make it private (a missing tool, a lock fault).
#   5. The tightening says what is true: a file that vanishes under it is no failure, a path it leaves open is
#      named in the warning, a comm folder whose root is already private is not walked, only the layout's own
#      entries are touched (an unknown file or folder keeps its mode), and a comm folder that is the home folder,
#      the root or a git checkout is refused with its permissions unchanged.
#   6. The tightening closes one entry of every name in its list, follows no layout folder that is a symlink,
#      closes the comm folder last, and takes a relative comm folder as the folder under the current directory.
#
# Windows has no umask: a folder under the profile inherits the profile's access list, so this suite
# prints one SKIP there. The locked local append and the hooks' mail read are Linux's, so other systems skip too.
#
# Usage: comm/tests/test-comm-private.sh
# Exit: 0 if every case PASSes (or the suite skips), 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home

umask 022
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-comm-private-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "FATAL: mktemp failed" >&2; exit 1; }
trap 'rm -rf "${WORK:?}"' EXIT
guard_fresh_home "$WORK"

if [ "$(uname -s)" != Linux ]; then
    echo "SKIP: comm folder modes: the access list under the profile is the mechanism here, and the locked local append is Linux's"
    exit 0
fi

chmod 755 "$HOME"
export SOT_COMM_HOME="$HOME/.sot-comm"
guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
export SOT_COMM_TEST_HOST="testhost"
unset SOT_COMM_NAME SOT_COMM_SELF_FILE CLAUDE_CODE_SESSION_ID

# A copy of the staged bin whose library finds no daemon and a fixture mount, so a library append is local.
BIN="$WORK/bin"
cp -r "$SCRIPTS_DIR" "$BIN" && chmod -R u+w "$BIN" || { echo "FATAL: cannot copy the scripts" >&2; exit 1; }
cat >> "$BIN/comm-lib.sh" <<'STUB'

# ---- no daemon, a fixture mount (test only) ---------------------------------
sot_daemon_endpoint() { return 1; }
sot_relay_endpoint() { [ -n "${1:-}" ] || return 1; printf '%s\n' "$1"; }
_sot_findmnt() { printf '%s\n' "${FAKE_MNT-nfs4 rw,vers=4.2,local_lock=none filer.example:/export/home}"; }
_sot_machine_id() { printf '0123456789abcdef0123456789abcdef'; }
STUB
grep -q '^_sot_machine_id() { printf' "$BIN/comm-lib.sh" || { echo "FATAL: the fixture stub did not land in the copy" >&2; exit 1; }
RECORD="nfs4 filer.example:/export/home"

# A stub `claude` on PATH stands in for the auditor's tier 2 and records the umask it was started under.
STUB_DIR="$WORK/claude-stub"; mkdir -p "$STUB_DIR"
cat > "$STUB_DIR/claude" <<'STUB'
#!/usr/bin/env bash
cat >/dev/null
umask > "$S2_CLAUDE_UMASK"
printf '%s' '{"findings":[]}'
STUB
chmod +x "$STUB_DIR/claude"
export S2_CLAUDE_UMASK="$WORK/claude-umask"
export PATH="$STUB_DIR:$PATH"

# A git repo to run from: the scripts' project, and case 3's worktree source.
FIX="$WORK/fixture"; mkdir -p "$FIX"
( cd "$FIX" && git init -q . && echo one > file.txt && git add file.txt \
    && git -c user.name=t -c user.email=t@example.invalid commit -q -m one ) || { echo "FATAL: no fixture repo" >&2; exit 1; }

pass=0; fail=0
ok()  { echo "PASS: $1"; pass=$((pass+1)); }
bad() { echo "FAIL: $1"; fail=$((fail+1)); }

mode() { perl -e 'printf "%o", (lstat $ARGV[0])[2] & 07777' "$1"; }
# Every path under ROOT but bin/ and VERSION that is not 700 (a folder) or 600 (a file), one indented line each.
loose_paths() {  # ROOT
    local p
    find "$1" \( -path "$1/bin" -o -path "$1/VERSION" \) -prune -o ! -type l \
        \( \( -type d ! -perm 700 \) -o \( -type f ! -perm 600 \) \) -print | while IFS= read -r p; do
        printf '    %s %s\n' "$(mode "$p")" "${p#"$WORK"/}"
    done
}
# can_read CLASS FILE: whether an account of class g (group) or o (other) can open FILE: its x bit on every folder
# from $HOME down to the file's own, and its r bit on the file.
can_read() {  # g|o FILE
    local xb=1 rb=4 d
    [ "$1" = g ] && { xb=8; rb=32; }
    d="$(dirname "$2")"
    while :; do
        [ $(( 8#$(mode "$d") & xb )) -ne 0 ] || return 1
        [ "$d" = "$HOME" ] && break
        d="$(dirname "$d")"
    done
    [ $(( 8#$(mode "$2") & rb )) -ne 0 ]
}
# S2 CMD...: run a comm script from the fixture repo (its project), output kept in $WORK/last.out.
S2() { ( cd "$FIX" && "$@" ) > "$WORK/last.out" 2>&1; }
# missing PATH...: the paths that do not exist, one indented line each.
missing() { local p; for p in "$@"; do [ -e "$p" ] || printf '    missing %s\n' "${p#"$WORK"/}"; done; }
line() { printf '{"ts":"2026-10-04T00:00:00Z","from":"%s","to":"%s","msg":"%s"}\n' "$1" "$2" "$3"; }
append() {  # TO FROM MSG: one line through the library's append
    line "$2" "$1" "$3" | bash -c 'source "$1/comm-lib.sh"; sot_inbox_append "$2"' _ "$BIN" "$1"
}

C="$SOT_COMM_HOME"
# ---- 1: every writer, under umask 022 below a 0755 home ----
fails=""
S2 "$BIN/comm-join.sh" --name s2-a || fails+=$'\n'"    comm-join.sh failed: $(tail -n 3 "$WORK/last.out" | tr '\n' ' ')"
fails+="$(missing "$C" "$C/inbox" "$C/self" "$C/read" "$C/registry.json" "$C/inbox/s2-a.jsonl" "$C/self/testhost__nopane.txt")"
ln -s "$BIN" "$C/bin"
S2 env SOT_COMM_ASKQ_ID=s2 "$BIN/comm-status.sh" blocked "q" || fails+=$'\n'"    comm-status.sh blocked failed: $(tail -n 3 "$WORK/last.out" | tr '\n' ' ')"
S2 "$BIN/comm-status.sh" idle
fails+="$(missing "$C/state" "$C/state/askq-s2.marker")"
( umask 077; printf '%s\n' "$RECORD" > "$C/inbox-lock-manager" )   # the record the daemon writes at boot
append s2-b s2-a "to b" >/dev/null 2>&1
append s2-a s2-b "one" >/dev/null 2>&1
fails+="$(missing "$C/inbox/s2-b.jsonl" "$C/inbox/s2-b.lock" "$C/inbox/s2-a.lock")"
S2 "$BIN/comm-poll.sh"
fails+="$(missing "$C/read/s2-a.cursor")"
( cd "$FIX" && printf '{"tool_name":"Bash"}' | bash "$BIN/comm-status-heartbeat.sh" ) >/dev/null 2>&1
set -- "$C"/state/hb-*.tick; fails+="$(missing "$1")"
append s2-a s2-b "two" >/dev/null 2>&1
TR1="$WORK/transcript-1.jsonl"
{ jq -nc '{type:"user",message:{content:"go"}}'; jq -nc '{type:"assistant",message:{content:[{type:"text",text:"Done."}]}}'; } > "$TR1"
( cd "$FIX" && jq -nc --arg p "$TR1" '{transcript_path:$p, stop_hook_active:false}' | bash "$BIN/comm-status-idle.sh" ) > "$WORK/stop-1.out" 2>&1
set -- "$C"/state/mail-*.tick; fails+="$(missing "$1")"
set -- "$C"/state/stop-feedback-*.jsonl; fails+="$(missing "$1")"
S2 "$BIN/comm-poll.sh"
TR2="$WORK/transcript-2.jsonl"
{ jq -nc '{type:"user",message:{content:"go"}}'; jq -nc '{type:"assistant",message:{content:[{type:"text",text:"Shall I go on?"}]}}'; } > "$TR2"
( cd "$FIX" && jq -nc --arg p "$TR2" '{transcript_path:$p, stop_hook_active:false}' | bash "$BIN/comm-status-idle.sh" ) > "$WORK/stop-2.out" 2>&1
fails+="$(missing "$C/state/auditor-s2-a")"
fails+="$(loose_paths "$C")"
for cls in g o; do
    ! can_read "$cls" "$C/inbox/s2-a.jsonl" || fails+=$'\n'"    class $cls can read .sot-comm/inbox/s2-a.jsonl"
done
if [ -z "$fails" ]; then ok "under umask 022 below a 0755 home no other account can read an inbox, and every folder is 0700 and every file 0600"
else bad "under umask 022 below a 0755 home no other account can read an inbox, and every folder is 0700 and every file 0600"; printf '%s\n' "${fails#$'\n'}"; fi

# ---- 2: a folder an older release left ----
OLD="$WORK/old-home/.sot-comm"
guard_refuse_live_home "$OLD"
mkdir -p "$OLD/inbox" "$OLD/read" "$OLD/self" "$OLD/state" "$OLD/bin"
printf '{"protocol_version": 1, "agents": {}}\n' > "$OLD/registry.json"
: > "$OLD/inbox/s2-a.jsonl"; : > "$OLD/inbox/s2-a.lock"; : > "$OLD/read/s2-a.cursor"
printf 's2-a\nrepo=fixture\nroot=%s\n' "$FIX" > "$OLD/self/testhost__nopane.txt"
: > "$OLD/state/x.tick"; printf '%s\n' "$RECORD" > "$OLD/inbox-lock-manager"; : > "$OLD/.registry.lock.reclaim.x"
printf 'stamp\n' > "$OLD/VERSION"; printf '#!/bin/sh\n' > "$OLD/bin/s.sh"; chmod 755 "$OLD/bin/s.sh"
printf 'outside\n' > "$WORK/outside.txt"; ln -s "$WORK/outside.txt" "$OLD/state/link"
fails=""
S2 env SOT_COMM_HOME="$OLD" "$BIN/comm-join.sh" --name s2-a || fails+=$'\n'"    comm-join.sh failed: $(tail -n 3 "$WORK/last.out" | tr '\n' ' ')"
fails+="$(loose_paths "$OLD")"
for kept in "bin 755" "bin/s.sh 755" "VERSION 644"; do
    [ "$(mode "$OLD/${kept% *}")" = "${kept#* }" ] || fails+=$'\n'"    ${kept% *} is $(mode "$OLD/${kept% *}"), not ${kept#* }"
done
[ "$(cat "$OLD/VERSION")" = stamp ] || fails+=$'\n'"    VERSION's bytes changed"
[ -L "$OLD/state/link" ] && [ "$(mode "$WORK/outside.txt")" = 644 ] && [ "$(cat "$WORK/outside.txt")" = outside ] \
    || fails+=$'\n'"    the symlink or its target changed (target mode $(mode "$WORK/outside.txt"))"
if [ -z "$fails" ]; then ok "a comm folder an older release left is tightened at the next join; bin/, VERSION and a symlink's target keep their modes"
else bad "a comm folder an older release left is tightened at the next join; bin/, VERSION and a symlink's target keep their modes"; printf '%s\n' "${fails#$'\n'}"; fi

# ---- 3: a writer of the user's own files keeps the caller's umask ----
export SOT_COMM_HOME="$WORK/h3/.sot-comm"
guard_refuse_live_home "$SOT_COMM_HOME"
S2 "$BIN/comm-join.sh" --name s2-c
ln -s "$BIN" "$SOT_COMM_HOME/bin"
rm -f "${S2_CLAUDE_UMASK:?}"
( cd "$FIX" && jq -nc --arg p "$TR2" '{transcript_path:$p, stop_hook_active:false}' | bash "$BIN/comm-status-idle.sh" ) > "$WORK/stop-3.out" 2>&1
got="$(cat "$S2_CLAUDE_UMASK" 2>/dev/null || echo none)"
# The same through the marker path's artifact audit: a closing SITREP after a Write of a file never shown.
TRM="$WORK/transcript-marker.jsonl"
{ jq -nc '{type:"user",message:{content:"go"}}'
  jq -nc '{type:"assistant",message:{content:[{type:"tool_use",id:"t0",name:"Write",input:{file_path:"/tmp/brief.md"}}]}}'
  jq -nc '{type:"user",message:{content:[{type:"tool_result",tool_use_id:"t0",content:"ok"}]}}'
  jq -nc '{type:"assistant",message:{content:[{type:"text",text:"SITREP: wrote the brief"}]}}'; } > "$TRM"
rm -f "${S2_CLAUDE_UMASK:?}"
( cd "$FIX" && jq -nc --arg p "$TRM" '{transcript_path:$p, stop_hook_active:false}' | bash "$BIN/comm-status-idle.sh" ) > "$WORK/stop-3m.out" 2>&1
got_marker="$(cat "$S2_CLAUDE_UMASK" 2>/dev/null || echo none)"
[ "$got" = 0022 ] && [ "$got_marker" = 0022 ] && ok "the auditor's claude -p starts under the caller's umask" \
    || { bad "the auditor's claude -p starts under the caller's umask"; echo "    umask there: $got after a question, $got_marker after a marker (want 0022)"; }
S2 "$BIN/comm-worktree-new.sh" s2 --no-spawn
WTD="$WORK/worktrees/fixture-wt-s2"
if [ -f "$WTD/file.txt" ] && [ "$(mode "$WTD/file.txt")" = 644 ] && [ "$(mode "$WORK/worktrees")" = 755 ]; then
    ok "comm-worktree-new.sh makes the worktree under the caller's umask"
else
    bad "comm-worktree-new.sh makes the worktree under the caller's umask"
    echo "    file.txt $(mode "$WTD/file.txt" 2>/dev/null || echo missing), worktrees/ $(mode "$WORK/worktrees" 2>/dev/null || echo missing); script said: $(tail -n 2 "$WORK/last.out" | tr '\n' ' ')"
fi

# ---- 4: the Stop hook and the auditor as the first writer of state/ ----
fails=""
BIN_F="$WORK/bin-fault"; cp -r "$BIN" "$BIN_F" && chmod -R u+w "$BIN_F" || { echo "FATAL: cannot copy the scripts" >&2; exit 1; }
cat >> "$BIN_F/comm-lib.sh" <<'STUB'
sot_mail_tools() { echo "jq flock perl s2-missing-tool"; }
sot_inbox_read_lock() { SOT_INBOX_READ_WARNING="s2 lock fault"; return 0; }
STUB
export SOT_COMM_HOME="$WORK/h4/.sot-comm"; guard_refuse_live_home "$SOT_COMM_HOME"
S2 "$BIN/comm-join.sh" --name s2-d
ln -s "$BIN_F" "$SOT_COMM_HOME/bin"
[ ! -e "$SOT_COMM_HOME/state" ] || fails+=$'\n'"    state/ existed before the Stop hook ran"
for _ in 1 2; do ( cd "$FIX" && jq -nc --arg p "$TR1" '{transcript_path:$p, stop_hook_active:false}' | bash "$BIN_F/comm-status-idle.sh" ) > "$WORK/stop-4.out" 2>&1; done
set -- "$SOT_COMM_HOME"/state/tool-fault-*.tick; fails+="$(missing "$1")"
set -- "$SOT_COMM_HOME"/state/lock-fault-*.tick; fails+="$(missing "$1")"
set -- "$SOT_COMM_HOME"/state/stop-feedback-*.jsonl; fails+="$(missing "$1")"
fails+="$(loose_paths "$SOT_COMM_HOME")"
export SOT_COMM_HOME="$WORK/h5/.sot-comm"; guard_refuse_live_home "$SOT_COMM_HOME"
S2 "$BIN/comm-join.sh" --name s2-e
ln -s "$BIN" "$SOT_COMM_HOME/bin"
[ ! -e "$SOT_COMM_HOME/state" ] || fails+=$'\n'"    state/ existed before the auditor ran"
( cd "$FIX" && jq -nc --arg p "$TR2" '{transcript_path:$p, stop_hook_active:false}' | bash "$BIN/comm-status-idle.sh" ) > "$WORK/stop-5.out" 2>&1
fails+="$(missing "$SOT_COMM_HOME/state/auditor-s2-e")"
fails+="$(loose_paths "$SOT_COMM_HOME")"
if [ -z "$fails" ]; then ok "the Stop hook and the auditor, each the first writer of state/, make it private (tool fault, lock fault, signature)"
else bad "the Stop hook and the auditor, each the first writer of state/, make it private (tool fault, lock fault, signature)"; printf '%s\n' "${fails#$'\n'}"; fi

# ---- 5: the tightening says what is true: churn is not a failure, a path left open is named, a private root is not walked ----
REAL_CHMOD="$(command -v chmod)"
# lay_old DIR: a small folder as an older release left it (folders 755, files 644), with the paths the cases below use.
lay_old() {
    mkdir -p "$1/inbox" "$1/read" "$1/self" "$1/state"
    printf '{"protocol_version": 1, "agents": {}}\n' > "$1/registry.json"
    : > "$1/inbox/s2-a.jsonl"; : > "$1/read/s2-a.cursor"; : > "$1/state/gone"; : > "$1/state/x.tick"
    printf 's2-a\nrepo=fixture\nroot=%s\n' "$FIX" > "$1/self/testhost__nopane.txt"
}
# A chmod that first removes one file find listed for it, as a writer's temp file does between find's listing and the chmod.
mkdir -p "$WORK/churn-bin"
printf '#!/bin/sh\nrm -f "$S2_GONE"\nexec "%s" "$@"\n' "$REAL_CHMOD" > "$WORK/churn-bin/chmod"
# A chmod that leaves one file alone and succeeds, as a mount that ignores modes does.
mkdir -p "$WORK/noop-bin"
cat > "$WORK/noop-bin/chmod" <<STUB
#!/bin/sh
n=\$#
while [ \$n -gt 0 ]; do a=\$1; shift; n=\$((n - 1)); [ "\$a" = state/x.tick ] || set -- "\$@" "\$a"; done
exec "$REAL_CHMOD" "\$@"
STUB
chmod +x "$WORK/churn-bin/chmod" "$WORK/noop-bin/chmod"
N="$WORK/churn-home/.sot-comm"; guard_refuse_live_home "$N"; lay_old "$N"
S2 env SOT_COMM_HOME="$N" S2_GONE="$N/state/gone" PATH="$WORK/churn-bin:$PATH" "$BIN/comm-join.sh" --name s2-a
fails="$(loose_paths "$N")"
grep -q WARNING "$WORK/last.out" && fails+=$'\n'"    a path that vanished during the tightening printed: $(grep WARNING "$WORK/last.out")"
if [ -z "$fails" ]; then ok "a file that vanishes during the tightening is no failure: no warning, the rest is private"
else bad "a file that vanishes during the tightening is no failure: no warning, the rest is private"; printf '%s\n' "${fails#$'\n'}"; fi
N="$WORK/noop-home/.sot-comm"; guard_refuse_live_home "$N"; lay_old "$N"
S2 env SOT_COMM_HOME="$N" PATH="$WORK/noop-bin:$PATH" "$BIN/comm-join.sh" --name s2-a
if grep -q "WARNING: the comm folder .* could not be made private: .*state/x.tick" "$WORK/last.out"; then
    ok "a path the tightening leaves open is named in the warning"
else bad "a path the tightening leaves open is named in the warning"; echo "    join said: $(tr '\n' ' ' < "$WORK/last.out")"; fi
N="$WORK/private-home/.sot-comm"; guard_refuse_live_home "$N"; lay_old "$N"; "$REAL_CHMOD" 700 "$N"
S2 env SOT_COMM_HOME="$N" "$BIN/comm-join.sh" --name s2-a
[ "$(mode "$N/state/x.tick")" = 644 ] && ok "a comm folder that is already private is not walked" \
    || { bad "a comm folder that is already private is not walked"; echo "    state/x.tick is $(mode "$N/state/x.tick"), the walk ran"; }

# An unknown file or folder in the comm folder keeps its mode; the layout's own entries do not.
N="$WORK/unknown-home/.sot-comm"; guard_refuse_live_home "$N"; lay_old "$N"
mkdir -p "$N/extra" "$N/state/sub"; : > "$N/notes.txt"; : > "$N/extra/f"; : > "$N/inbox/notes.txt"; : > "$N/state/sub/g"
: > "$N/read/notes.txt"; : > "$N/self/notes.json"; : > "$N/registry.json.bak"; : > "$N/.registry.lockX"
S2 env SOT_COMM_HOME="$N" "$BIN/comm-join.sh" --name s2-a
fails=""
for kept in notes.txt extra extra/f inbox/notes.txt read/notes.txt self/notes.json registry.json.bak .registry.lockX state/sub state/sub/g; do
    [ "$(mode "$N/$kept")" = "$([ -d "$N/$kept" ] && echo 755 || echo 644)" ] || fails+=$'\n'"    $kept is $(mode "$N/$kept"): an unknown entry was changed"
done
for own in . inbox state registry.json inbox/s2-a.jsonl state/x.tick; do
    [ $(( 8#$(mode "$N/$own") & 077 )) -eq 0 ] || fails+=$'\n'"    $own is $(mode "$N/$own"): a layout entry was left open"
done
if [ -z "$fails" ]; then ok "the tightening touches the layout's own entries only: an unknown file or folder keeps its mode"
else bad "the tightening touches the layout's own entries only: an unknown file or folder keeps its mode"; printf '%s\n' "${fails#$'\n'}"; fi
# refused: the comm folder is the home folder, or a git checkout; nothing changes and the join still succeeds.
refused() {  # NAME DIR WORD [env VAR=VAL...]
    local name="$1" dir="$2" word="$3" rc=0; shift 3
    lay_old "$dir"; : > "$dir/notes.txt"
    S2 env "$@" SOT_COMM_HOME="$dir" "$BIN/comm-join.sh" --name s2-a || rc=$?
    local f=""
    [ "$rc" = 0 ] || f+=$'\n'"    the join failed ($rc): $(tail -n 2 "$WORK/last.out" | tr '\n' ' ')"
    grep -q "WARNING: the comm folder .* is $word, so its permissions were not changed" "$WORK/last.out" || f+=$'\n'"    no refusal line naming $word"
    for kept in . inbox inbox/s2-a.jsonl state/x.tick notes.txt; do
        [ "$(mode "$dir/$kept")" = "$([ -d "$dir/$kept" ] && echo 755 || echo 644)" ] || f+=$'\n'"    $kept is $(mode "$dir/$kept"): the refusal changed it"
    done
    if [ -z "$f" ]; then ok "a comm folder that is $word is not tightened: its permissions are unchanged"
    else bad "a comm folder that is $word is not tightened: its permissions are unchanged"; printf '%s\n' "${f#$'\n'}"; fi
}
P="$WORK/refuse-home"; guard_refuse_live_home "$P"; refused home "$P" "the home folder" HOME="$P"
G="$WORK/git-home/.sot-comm"; guard_refuse_live_home "$G"; mkdir -p "$G/.git"; refused git "$G" "a git checkout"
[ "$(bash -c 'source "$1"; COMM_HOME=/; _sot_comm_refusal' _ "$BIN/comm-lib.sh")" = "the root folder" ] \
    && ok "a comm folder that is the root folder is refused" || bad "a comm folder that is the root folder is refused"

# ---- 6: every name of the list, a layout folder that is a symlink, the order of the chmods, a relative comm folder ----
# ensure_direct DIR [VAR=VAL...]: ensure_home alone, from the library: no writer that rewrites a file along the way.
ensure_direct() {
    local dir="$1"; shift
    ( cd "$FIX" && env SOT_COMM_HOME="$dir" "$@" bash -c 'source "$1/comm-lib.sh"; ensure_home' _ "$BIN" ) > "$WORK/last.out" 2>&1
}
T="$WORK/table-home/.sot-comm"; guard_refuse_live_home "$T"
mkdir -p "$T/inbox" "$T/read" "$T/self" "$T/state" "$T/probe/p"
TABLE_FILES="registry.json registry.json.tmp registry.json.new.1 .registry.lock .registry.lock.tmp.x .registry.lock.reclaim.x inbox-lock-manager .inbox-lock-manager.m.1.0 gh-device-auth.json inbox/a.jsonl inbox/a.lock read/a.cursor self/h__w.txt state/t.tick state/.hb-ctx-1 state/auditor-x"
for f in $TABLE_FILES; do : > "$T/$f"; done
ensure_direct "$T"
fails=""
for d in . inbox read self state probe probe/p; do
    [ "$(mode "$T/$d")" = 700 ] || fails+=$'\n'"    $d is $(mode "$T/$d"), not 700"
done
for f in $TABLE_FILES; do
    [ "$(mode "$T/$f")" = 600 ] || fails+=$'\n'"    $f is $(mode "$T/$f"), not 600"
done
if [ -z "$fails" ]; then ok "the tightening closes one entry of every name in its list"
else bad "the tightening closes one entry of every name in its list"; printf '%s\n' "${fails#$'\n'}"; fi
# A layout folder that is a symlink is never followed: what lies behind it keeps its mode.
SL="$WORK/symlink-home/.sot-comm"; OUT="$WORK/outside"; guard_refuse_live_home "$SL"
mkdir -p "$SL" "$OUT/inbox" "$OUT/read" "$OUT/self" "$OUT/state" "$OUT/probe/proj"
for f in inbox/a.jsonl inbox/a.lock read/a.cursor self/h.txt state/t.tick state/.hb probe/f; do : > "$OUT/$f"; done
for d in inbox read self state probe; do ln -s "$OUT/$d" "$SL/$d"; done
ensure_direct "$SL"
fails=""
for f in inbox/a.jsonl inbox/a.lock read/a.cursor self/h.txt state/t.tick state/.hb probe/f; do
    [ "$(mode "$OUT/$f")" = 644 ] || fails+=$'\n'"    $f behind a symlink is $(mode "$OUT/$f"), not 644"
done
for d in inbox read self state probe probe/proj; do
    [ "$(mode "$OUT/$d")" = 755 ] || fails+=$'\n'"    $d behind a symlink is $(mode "$OUT/$d"), not 755"
done
if [ -z "$fails" ]; then ok "a layout folder that is a symlink is not followed: what lies behind it keeps its mode"
else bad "a layout folder that is a symlink is not followed: what lies behind it keeps its mode"; printf '%s\n' "${fails#$'\n'}"; fi
# The folder itself is closed last, so a pass cut short leaves it open and the next call walks again.
N="$WORK/order-home/.sot-comm"; guard_refuse_live_home "$N"; lay_old "$N"
mkdir -p "$WORK/log-bin"; : > "$WORK/chmod.log"
printf '#!/bin/sh\necho "$*" >> "%s"\nexec "%s" "$@"\n' "$WORK/chmod.log" "$REAL_CHMOD" > "$WORK/log-bin/chmod"; "$REAL_CHMOD" +x "$WORK/log-bin/chmod"
ensure_direct "$N" PATH="$WORK/log-bin:$PATH"
[ "$(tail -n 1 "$WORK/chmod.log")" = "go-rwx ." ] && ok "the comm folder itself is closed last" \
    || { bad "the comm folder itself is closed last"; echo "    chmod calls: $(tr '\n' ';' < "$WORK/chmod.log")"; }
# A relative comm folder is made absolute once: an exported CDPATH cannot send the tightening to another folder.
R="$WORK/cdpath"; mkdir -p "$R/x/rel/state" "$R/cwd"; : > "$R/x/rel/state/x"
( cd "$R/cwd" && env SOT_COMM_HOME=rel CDPATH="$R/x" bash -c 'source "$1/comm-lib.sh"; ensure_home' _ "$BIN" ) > "$WORK/last.out" 2>&1
rel_seen="$( cd "$R/cwd" && env SOT_COMM_HOME=rel bash -c 'source "$1/comm-lib.sh"; bash -c "printf %s \"\$SOT_COMM_HOME\""' _ "$BIN" 2>&1 )"
if [ "$(mode "$R/x/rel")" = 755 ] && [ "$(mode "$R/x/rel/state/x")" = 644 ] && ! grep -q WARNING "$WORK/last.out" && [ "$(mode "$R/cwd/rel")" = 700 ] && [ "$rel_seen" = "$R/cwd/rel" ]; then
    ok "a relative comm folder is the folder under the current directory, whatever CDPATH holds"
else
    bad "a relative comm folder is the folder under the current directory, whatever CDPATH holds"
    echo "    CDPATH folder rel is $(mode "$R/x/rel") (want 755), its state/x $(mode "$R/x/rel/state/x") (want 644), cwd/rel $(mode "$R/cwd/rel" 2>/dev/null || echo missing) (want 700), children see SOT_COMM_HOME=$rel_seen (want $R/cwd/rel); said: $(tr '\n' ' ' < "$WORK/last.out")"
fi

echo "$pass passed, $fail failed"
[ "$fail" = 0 ]
