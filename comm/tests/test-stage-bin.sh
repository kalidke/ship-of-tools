#!/usr/bin/env bash
# test-stage-bin.sh — stage-bin.sh publishes each staged file by a rename from an exclusive sibling, copying no
# source permissions: a reader of the public name sees all its old bytes or all its new ones, a failed copy, mode
# or rename leaves the old file whole with no temporary left behind, and staging stops at the failure. Runs the REAL
# stage-bin.sh over a private minimal bin-folder source (so it does not stage itself), with cp, chmod and mv
# replaced for the child only by absolute fixture wrappers.
#
# Usage: comm/tests/test-stage-bin.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/lib-home-guard.sh" || exit 2   # never the live comm home
. "$(dirname "${BASH_SOURCE[0]}")/lib-wait.sh" || exit 2

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-stage-bin-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
guard_fresh_home "$WORK"; guard_refuse_live_home "$WORK/home"
trap 'rm -rf "${WORK:?}"' EXIT

PASS=0
FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
}

REAL_CP="$(command -v cp)"; REAL_MV="$(command -v mv)"; REAL_CHMOD="$(command -v chmod)"

# SRC: a private tree with the real stage-bin.sh and two bin folders.
SRC="$WORK/src"
mkdir -p "$SRC/comm/tests" "$SRC/comm/one" "$SRC/comm/two" "$SRC/comm/empty"
cp "$SCRIPT_DIR/stage-bin.sh" "$SRC/comm/tests/stage-bin.sh"
printf 'comm/one\ncomm/two\n' > "$SRC/comm/bin-folders.txt"
printf '#!/bin/sh\necho a-new\n' > "$SRC/comm/one/a.sh"
printf '#!/bin/sh\necho b-new\n' > "$SRC/comm/one/b.sh"
printf '#!/bin/sh\necho tool\n' > "$SRC/comm/one/tool"; chmod 755 "$SRC/comm/one/tool"
printf 'data\n' > "$SRC/comm/one/c.txt"; chmod 644 "$SRC/comm/one/c.txt"
printf 'page\n' > "$SRC/comm/one/CLAUDE.md"
printf 'two\n' > "$SRC/comm/two/d.txt"
STAGE_SH="$SRC/comm/tests/stage-bin.sh"

# Fixture wrappers, first on PATH for the staging child only. Each reads its behaviour from variables the case sets.
FX="$WORK/fx"; mkdir -p "$FX"
cat > "$FX/cp" <<STUB
#!/bin/bash
# With CP_REJECT_P, refuses -p (a filesystem that cannot hold the source's permissions); otherwise drops it. Fails or pauses on request.
args=(); for a in "\$@"; do case "\$a" in -p|-pf|-fp|-pP|--preserve*) [ -z "\${CP_REJECT_P:-}" ] || { echo "cp: preserving permissions: Operation not supported" >&2; exit 1; } ;; -f) ;; *) args+=("\$a") ;; esac; done
src="\${args[0]}"; dst="\${args[1]}"
case "\$src" in
  *"/\${CP_FAIL_ON:-NONE}") head -c 3 "\$src" > "\$dst"; echo "cp: injected failure" >&2; exit 1 ;;
  *"/\${CP_PAUSE_ON:-NONE}") head -c 3 "\$src" > "\$dst"; : > "\$CP_STARTED"; read -r _ < "\$CP_RELEASE"; "$REAL_CP" "\$src" "\$dst"; exit \$? ;;
esac
exec "$REAL_CP" "\$@"
STUB
cat > "$FX/chmod" <<STUB
#!/bin/bash
case "\$2" in *"/\${CHMOD_FAIL_ON:-NONE}"|*".\${CHMOD_FAIL_ON:-NONE}."*) echo "chmod: injected failure" >&2; exit 1 ;; esac
exec "$REAL_CHMOD" "\$@"
STUB
cat > "$FX/mv" <<STUB
#!/bin/bash
case "\${@: -1}" in *"/\${MV_FAIL_ON:-NONE}") echo "mv: injected failure" >&2; exit 1 ;; esac
exec "$REAL_MV" "\$@"
STUB
"$REAL_CHMOD" +x "$FX/cp" "$FX/chmod" "$FX/mv"

CASEN=0
new_dest() { CASEN=$((CASEN + 1)); DEST="$WORK/dest-$CASEN"; mkdir -p "$DEST"; }
stage() {  # VAR=val... : run stage-bin.sh into $DEST under the wrappers; sets RC and OUT
    OUT="$(env "$@" PATH="$FX:$PATH" bash "$STAGE_SH" "$DEST" 2>&1)"; RC=$?
}
# Whether this platform keeps a chmod 600 (git-bash on Windows does not): the 0600 check runs only where it does.
: > "$WORK/modeprobe"; chmod 600 "$WORK/modeprobe"
if [ "$(stat -c %a "$WORK/modeprobe" 2>/dev/null || stat -f %Lp "$WORK/modeprobe")" = 600 ]; then MODES_KEPT=1; else MODES_KEPT=0; fi
mode_of() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }
leftovers() { find "$DEST" -name '.stage.*' | wc -l | tr -d ' '; }

case_a_clean_stage_sets_the_modes_and_takes_no_source_permissions() {
    new_dest; stage CP_REJECT_P=1
    [ "$RC" -eq 0 ] || { echo "  rc $RC: $OUT"; return 1; }
    [ "$(mode_of "$DEST/a.sh")" = 755 ] && [ "$(mode_of "$DEST/tool")" = 755 ] && { [ "$MODES_KEPT" = 0 ] || [ "$(mode_of "$DEST/c.txt")" = 600 ]; } \
        || { echo "  modes: a.sh $(mode_of "$DEST/a.sh") tool $(mode_of "$DEST/tool") c.txt $(mode_of "$DEST/c.txt")"; return 1; }
    [ ! -e "$DEST/CLAUDE.md" ] && [ -f "$DEST/d.txt" ] && [ "$(leftovers)" = 0 ] || { echo "  layout: $(ls -A "$DEST" | tr '\n' ' ')"; return 1; }
}

case_the_public_file_keeps_all_its_old_bytes_until_the_rename() {
    new_dest; printf 'OLD-BYTES\n' > "$DEST/a.sh"
    local fifo="$WORK/release-$CASEN" started="$WORK/started-$CASEN"; mkfifo "$fifo"
    ( stage CP_PAUSE_ON=a.sh CP_STARTED="$started" CP_RELEASE="$fifo"; echo "$RC" > "$WORK/rc-$CASEN"; echo "$OUT" > "$WORK/out-$CASEN" ) &
    local pid=$!
    await test -e "$started" || { echo "  the paused copy never started"; kill "$pid" 2>/dev/null; return 1; }
    local during; during="$(cat "$DEST/a.sh")"
    printf 'go\n' > "$fifo"; wait "$pid"
    [ "$during" = "OLD-BYTES" ] || { echo "  while the copy was half done a.sh held '$during', want its complete old bytes"; return 1; }
    [ "$(cat "$WORK/rc-$CASEN")" = 0 ] && [ "$(sed -n 2p "$DEST/a.sh")" = "echo a-new" ] || { echo "  after release: rc $(cat "$WORK/rc-$CASEN") a.sh: $(cat "$DEST/a.sh")"; return 1; }
}

failure_leaves_old_and_stops() {  # VAR : the injected failure is on b.sh
    new_dest; printf 'OLD-A\n' > "$DEST/a.sh"; printf 'OLD-B\n' > "$DEST/b.sh"
    stage "$1=b.sh"
    [ "$RC" -ne 0 ] || { echo "  staging exited 0 past an injected failure"; return 1; }
    case "$OUT" in *"stage-bin: FATAL"*) ;; *) echo "  no visible error: '$OUT'"; return 1 ;; esac
    [ "$(cat "$DEST/b.sh")" = "OLD-B" ] || { echo "  b.sh was changed: '$(cat "$DEST/b.sh")'"; return 1; }
    [ "$(sed -n 2p "$DEST/a.sh")" = "echo a-new" ] || { echo "  the file before the failure was not published"; return 1; }
    [ ! -e "$DEST/c.txt" ] && [ ! -e "$DEST/d.txt" ] || { echo "  later files were published"; return 1; }
    [ "$(leftovers)" = 0 ] || { echo "  leaked temporaries: $(ls -A "$DEST" | tr '\n' ' ')"; return 1; }
}
case_a_failed_copy_leaves_the_old_file_and_stops() { failure_leaves_old_and_stops CP_FAIL_ON; }
case_a_failed_mode_leaves_the_old_file_and_stops() { failure_leaves_old_and_stops CHMOD_FAIL_ON; }
case_a_failed_rename_leaves_the_old_file_and_stops() { failure_leaves_old_and_stops MV_FAIL_ON; }

case_duplicate_missing_and_empty_folders_are_refused() {
    local save; save="$(cat "$SRC/comm/bin-folders.txt")"
    printf 'dup\n' > "$SRC/comm/two/a.sh"; new_dest; stage X=1
    [ "$RC" -ne 0 ] && case "$OUT" in *"shipped by two folders"*) true ;; *) false ;; esac || { rm -f "${SRC:?}/comm/two/a.sh"; echo "  duplicate: rc $RC '$OUT'"; return 1; }
    rm -f "${SRC:?}/comm/two/a.sh"
    printf 'comm/one\ncomm/none\n' > "$SRC/comm/bin-folders.txt"; new_dest; stage X=1
    [ "$RC" -ne 0 ] && case "$OUT" in *"not a folder"*) true ;; *) false ;; esac || { echo "  missing: rc $RC '$OUT'"; return 1; }
    printf 'comm/one\ncomm/empty\n' > "$SRC/comm/bin-folders.txt"; new_dest; stage X=1
    [ "$RC" -ne 0 ] && case "$OUT" in *"ships no file"*) true ;; *) false ;; esac || { echo "  empty: rc $RC '$OUT'"; return 1; }
    printf '%s\n' "$save" > "$SRC/comm/bin-folders.txt"
}

check "a clean stage sets the modes and the copy takes no source permissions" case_a_clean_stage_sets_the_modes_and_takes_no_source_permissions
check "the public file keeps all its old bytes until the rename" case_the_public_file_keeps_all_its_old_bytes_until_the_rename
check "a failed copy leaves the old file whole, no temporary, and stops staging" case_a_failed_copy_leaves_the_old_file_and_stops
check "a failed mode change leaves the old file whole, no temporary, and stops staging" case_a_failed_mode_leaves_the_old_file_and_stops
check "a failed rename leaves the old file whole, no temporary, and stops staging" case_a_failed_rename_leaves_the_old_file_and_stops
check "a duplicate name, a missing folder and an empty folder are refused" case_duplicate_missing_and_empty_folders_are_refused

echo "---"
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ]
