#!/usr/bin/env bash
# test-ccb-agent-exec.sh — ADR 0046 decision 4: `ccb`/`ccbe` reduce to one
# line, `exec sotd agent-exec claude "$@"`, resolving `sotd` the same way
# `comm-lib.sh`'s `_try_sotd_socket_bin` candidates do ($SOTD_BIN, PATH, the
# two install paths). This suite proves the resolved `sotd` is invoked with
# `agent-exec claude` and the caller's own flags, IN ORDER, via a `sotd`
# STUB on PATH -- no real daemon, no real claude, and no dependency on
# whatever `sotd` (if any) happens to be installed on the machine running
# this suite (the "no sotd found" case pins a scratch $HOME so the install-
# path fallback can't accidentally find a REAL install there).
#
# Usage: comm/core/tests/test-ccb-agent-exec.sh
# Exit: 0 if every case PASSes, 1 if any FAILs.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DIR="$(cd "$SCRIPT_DIR/../../adapters/claude/bin" && pwd)"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/sot-ccb-agent-exec-test-XXXXXX")"
[ -n "$WORK" ] && [ -d "$WORK" ] || { echo "mktemp failed" >&2; exit 1; }
trap 'rm -rf "$WORK"' EXIT

PASS=0
FAIL=0
check() {
    local desc="$1" fn="$2"
    if "$fn"; then
        echo "PASS: $desc"; PASS=$((PASS + 1))
    else
        echo "FAIL: $desc"; FAIL=$((FAIL + 1))
    fi
}

# A `sotd` stub that records its own argv, one per line, and exits 0. `ccb`/
# `ccbe` themselves `exec` INTO whatever this resolves to, so the stub is
# the last thing that ever runs -- recording argv is enough to prove what
# `ccb` handed it.
STUB_DIR="$WORK/stubbin"
mkdir -p "$STUB_DIR"
ARGV_LOG="$WORK/argv.log"
cat > "$STUB_DIR/sotd" <<'EOF'
#!/bin/sh
: > "$ARGV_LOG_PATH"
for a in "$@"; do
    printf '%s\n' "$a" >> "$ARGV_LOG_PATH"
done
EOF
chmod +x "$STUB_DIR/sotd"

# A scratch $HOME with no sot install at all -- so `ccb`'s own install-path
# fallback ($HOME/.local/share/sot/bin/sotd, $HOME/.local/bin/sotd) can
# never find a real one, even on a machine that has Ship of Tools installed
# for real.
EMPTY_HOME="$WORK/empty-home"
mkdir -p "$EMPTY_HOME"

assert_argv_is() {  # <expected words...>  (reads $ARGV_LOG)
    [ -f "$ARGV_LOG" ] || { echo "  sotd stub never ran"; return 1; }
    local want line=1 got
    for want in "$@"; do
        got="$(sed -n "${line}p" "$ARGV_LOG")"
        [ "$got" = "$want" ] || { echo "  argv[$line]: expected '$want', got '$got'"; return 1; }
        line=$((line + 1))
    done
    local n; n="$(wc -l < "$ARGV_LOG")"
    [ "$n" -eq $((line - 1)) ] || { echo "  expected exactly $((line - 1)) argv words, got $n:"; cat "$ARGV_LOG"; return 1; }
    return 0
}

case_ccb_forwards_flags_in_order_to_agent_exec() {
    rm -f "$ARGV_LOG"
    ARGV_LOG_PATH="$ARGV_LOG" SOTD_BIN="" PATH="$STUB_DIR:$PATH" \
        "$BIN_DIR/ccb" --continue --x >/dev/null 2>"$WORK/stderr.log"
    assert_argv_is agent-exec claude --continue --x
}

case_ccbe_matches_ccbs_own_recipe_exactly() {
    rm -f "$ARGV_LOG"
    ARGV_LOG_PATH="$ARGV_LOG" SOTD_BIN="" PATH="$STUB_DIR:$PATH" \
        "$BIN_DIR/ccbe" --continue --x >/dev/null 2>"$WORK/stderr.log"
    assert_argv_is agent-exec claude --continue --x
}

case_a_bare_ccb_forwards_no_flags() {
    rm -f "$ARGV_LOG"
    ARGV_LOG_PATH="$ARGV_LOG" SOTD_BIN="" PATH="$STUB_DIR:$PATH" \
        "$BIN_DIR/ccb" >/dev/null 2>"$WORK/stderr.log"
    assert_argv_is agent-exec claude
}

case_no_sotd_found_fails_with_one_clear_error_line() {
    local out rc
    out="$(SOTD_BIN="" HOME="$EMPTY_HOME" PATH="$WORK/no-such-dir" "$BIN_DIR/ccb" 2>&1)"
    rc=$?
    [ "$rc" -ne 0 ] || { echo "  expected a non-zero exit with no sotd resolvable anywhere"; return 1; }
    printf '%s' "$out" | grep -q "sotd not found" || { echo "  expected a clear error line, got: $out"; return 1; }
    return 0
}

check "ccb forwards its flags, in order, to sotd agent-exec claude"       case_ccb_forwards_flags_in_order_to_agent_exec
check "ccbe matches ccb's own recipe exactly (ADR 0046: one recipe)"      case_ccbe_matches_ccbs_own_recipe_exactly
check "a bare ccb (no flags) still reaches agent-exec claude"             case_a_bare_ccb_forwards_no_flags
check "no sotd resolvable anywhere fails with one clear error line"       case_no_sotd_found_fails_with_one_clear_error_line

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
