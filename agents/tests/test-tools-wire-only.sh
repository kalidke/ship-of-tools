#!/usr/bin/env bash
# test-tools-wire-only.sh — guarantee 9 of agents/CLAUDE.md: a shell tool reaches the daemon only through the wire, an
# op over an endpoint or a `sotd` subcommand, and reads nothing the daemon owns. The form: the registry and the inbox
# folder the tools are told of do not exist, and a stand-in `sotd` answers `comm registry read` and `comm unread` from canned text; a tool
# that opens the registry or an inbox by path finds nothing there and gives a different answer, and one that writes
# there leaves a file the suite finds. Every other `sotd` call goes to the built sotd (GUARD_SOTD).
#
# HERMETIC: its own HOME and comm folder, no daemon.
#
# Usage: agents/tests/test-tools-wire-only.sh     Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/../../comm/tests/lib-home-guard.sh" || exit 2   # never the live comm home

WORK="$(mktemp -d "${TMPDIR:-/tmp}/tools-wire-only-test-XXXXXX")"
if [ -z "$WORK" ] || [ ! -d "$WORK" ]; then
    echo "FATAL: mktemp did not produce a usable work directory (got: '$WORK')" >&2
    exit 1
fi
WORK="$(cd "$WORK" && pwd -P)" || exit 1
[ -n "${KEEP:-}" ] || trap 'rm -rf "${WORK:?}"' EXIT

guard_fresh_home "$WORK"
export SOT_COMM_HOME="$WORK/comm"
guard_refuse_live_home "$SOT_COMM_HOME"
SCRIPTS_DIR="$(guard_stage_bin "$WORK")" || exit 2
[ -n "${GUARD_SOTD:-}" ] || { echo "FATAL: no built sotd (set SOT_TEST_SOTD or CARGO_TARGET_DIR)" >&2; exit 2; }

# The stand-in sotd: the registry and the unread count from canned text, each call logged; the rest is the built sotd's.
NEVER="$WORK/never"   # where the daemon's registry and inbox are said to be; no such folder
STUB="$WORK/stub"; mkdir -p "$STUB"
CALLS="$WORK/calls.log"; : > "$CALLS"
TOTAL_FILE="$WORK/unread-total"; echo 3 > "$TOTAL_FILE"
cat > "$STUB/sotd" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$CALLS"
case "\${1:-} \${2:-} \${3:-}" in
    "comm registry read")
        if [ -n "\${4:-}" ]; then
            case "\$4" in
                proj-wt-a) echo '{"state":"working","summary":"on it"}'; exit 0 ;;
                *) exit 1 ;;
            esac
        fi
        echo '{"agents":{"proj":{"state":"idle"},"proj-wt-a":{"state":"working","summary":"on it"}}}'; exit 0 ;;
    "comm context"*)
        "$GUARD_SOTD" "\$@" | sed -e 's|^REGISTRY=.*|REGISTRY=$NEVER/registry.json|' -e 's|^INBOX_DIR=.*|INBOX_DIR=$NEVER/inbox|'
        exit "\${PIPESTATUS[0]}" ;;
    "comm unread "*) printf '%s 0\n' "\$(cat "$TOTAL_FILE")"; exit 0 ;;
esac
$GUARD_EMPTY_WALK
exec "$GUARD_SOTD" "\$@"
EOF
chmod +x "$STUB/sotd"; cp "$STUB/sotd" "$STUB/sotd.exe"
export SOTD_BIN="$STUB/sotd"

PASS=0
FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then echo "PASS: $desc"; PASS=$((PASS + 1)); else echo "FAIL: $desc"; FAIL=$((FAIL + 1)); fi
}
contains() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }

# A repo `proj` with a worktree `proj-wt-a`.
git init -q -b main "$WORK/proj" && git -C "$WORK/proj" -c user.email=t@t -c user.name=t commit -q --allow-empty -m i \
    && git -C "$WORK/proj" worktree add -q "$WORK/proj-wt-a" -b a || { echo "FATAL: no test repo" >&2; exit 2; }

no_daemon_file_made() { [ ! -e "$NEVER" ] || { echo "  a tool made a daemon file: $(ls -A "$NEVER")"; return 1; }; }

case_worktree_status_reads_the_registry_through_the_daemon() {
    local out
    out="$(cd "$WORK/proj" && bash "$SCRIPTS_DIR/comm-worktree-status.sh" 2>&1)" || { echo "  rc=$? out=$out"; return 1; }
    contains "$out" "working · on it" || { echo "  the daemon's row is not shown: $out"; return 1; }
    no_daemon_file_made
}

case_worktree_sync_finds_its_family_through_the_daemon() {
    local out
    out="$(cd "$WORK/proj" && bash "$SCRIPTS_DIR/comm-worktree-sync.sh" 2>&1)" || true
    contains "$out" "worktree-sync: base 'proj', family [proj proj-wt-a ]" || { echo "  no family from the daemon: $out"; return 1; }
    no_daemon_file_made
}

case_probe_sees_new_mail_through_the_daemon() {
    local a b
    a="$(bash -c '. "$1"; probe_inbox_stamp proj-wt-a' _ "$SCRIPTS_DIR/comm-probe.sh" 2>&1)"
    echo 4 > "$TOTAL_FILE"
    b="$(bash -c '. "$1"; probe_inbox_stamp proj-wt-a' _ "$SCRIPTS_DIR/comm-probe.sh" 2>&1)"
    [ "$a" != "$b" ] || { echo "  the stamp did not follow the daemon's count: '$a' then '$b'"; return 1; }
    contains "$(cat "$CALLS")" "comm unread proj-wt-a" || { echo "  the daemon was not asked: $(cat "$CALLS")"; return 1; }
    no_daemon_file_made
}

check "comm-worktree-status.sh shows the daemon's registry row with no registry file to open" \
    case_worktree_status_reads_the_registry_through_the_daemon
check "comm-worktree-sync.sh finds its family through the daemon with no registry file to open" \
    case_worktree_sync_finds_its_family_through_the_daemon
check "comm-probe.sh follows the daemon's unread count, not an inbox file" case_probe_sees_new_mail_through_the_daemon

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
