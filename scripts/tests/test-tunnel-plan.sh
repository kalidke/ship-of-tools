#!/usr/bin/env bash
# test-tunnel-plan.sh -- regression harness for scripts/sot-hosts.sh's
# tunnel-plan builder (ADR 0042 L2b design E: one tunnel per configured
# remote, both launchers).
#
# Pure text processing (no ssh, no network) against a fixture hosts.toml,
# mirroring test-sot-apply.ps1/test-local-daemon.ps1's own "fake files are
# never executed" convention and installer-state.sh's `check`/`case_start`
# shape (this file is the bash-side sibling of
# scripts/tests/test-tunnel-plan.ps1, which exercises the same host set --
# see FIXTURE below -- through Get-TunnelPlan).
#
# Duplicate tcp_port across hosts is NOT tested here (owner ruling, codex
# follow-up round 2): sot_tunnel_plan doesn't detect it -- that check lives
# once, in rust/frontend/src/hosts.rs's resolve_connections (its own test).
# A second `ssh -fN -L` on an already-bound port fails to bind on its own,
# which sot_ensure_remote_host already treats as an ordinary nonfatal
# failure, so the launcher needs nothing else here.
#
# Run: scripts/tests/test-tunnel-plan.sh

set -euo pipefail

# shellcheck source=../sot-hosts.sh
. "$(dirname "$0")/../sot-hosts.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
fails=0

check() {  # <description> <expected> <actual>
    if [ "$2" = "$3" ]; then
        printf '    ok   %s\n' "$1"
    else
        printf '    FAIL %s\n      expected: %s\n      actual:   %s\n' "$1" "$2" "$3"
        fails=$((fails + 1))
    fi
}
case_start() { printf '  %s\n' "$1"; }

# Shared with test-tunnel-plan.ps1 -- same host set, same names, so a
# fixture change on either side is easy to keep in sync by eye. Kept as two
# copies (one per language's own fixture-writing idiom -- heredoc vs.
# here-string) rather than one shared file: neither test harness has a
# reason to read the OTHER language's directory layout, and a single-file
# share would add that coupling for no real gain at this size.
FIXTURE="$WORK/hosts.toml"
cat > "$FIXTURE" <<'EOF'
default_host = "myserver"

[host.myserver]
ssh_alias = "myserver-alias"
remote_repo = "/home/me/project"
# tcp_port omitted -- it is the default host (by KEY, not ssh_alias), so
# it falls back to whatever default_port sot_tunnel_plan is given

[host.otherbox]
ssh_alias = "otherbox"
remote_repo = "/home/me/project"
tcp_port = 18744
remote_socket = "/run/user/1000/sot/sessions/sot.sock"

[host.thirdbox]
ssh_alias = "thirdbox"
remote_repo = "/home/me/project3"
# tcp_port deliberately omitted -- not the default host, so this must error

[host.local]
ssh_alias = "myserver-alias"
tcp_port = 18743
EOF

echo "=== sot_hosts_default_host / sot_hosts_table ==="
case_start "default_host"
check "default_host parsed" "myserver" "$(sot_hosts_default_host "$FIXTURE")"

case_start "hosts_table"
table="$(sot_hosts_table "$FIXTURE")"
check "four sections captured" "4" "$(printf '%s\n' "$table" | wc -l | tr -d ' ')"
check "myserver row" "myserver|myserver-alias|/home/me/project|||" \
    "$(printf '%s\n' "$table" | awk -F'|' '$1=="myserver"')"
check "local row carries whatever it was given (filtering is sot_tunnel_plan's job)" \
    "local|myserver-alias||18743||" \
    "$(printf '%s\n' "$table" | awk -F'|' '$1=="local"')"

echo "=== sot_tunnel_plan (default_host=myserver, default_port=18743) ==="
plan="$(sot_tunnel_plan "$FIXTURE" myserver 18743)"

case_start "local is never in the plan (socket-only, regardless of ssh_alias/tcp_port on it)"
check "no local row" "" "$(printf '%s\n' "$plan" | awk -F'|' '$1=="local"')"

case_start "myserver (the default, identified by hosts.toml KEY not ssh_alias) keeps its own tcp_port"
check "myserver local_port" "18743" "$(printf '%s\n' "$plan" | awk -F'|' '$1=="myserver"{print $3}')"
check "myserver has no error" "" "$(printf '%s\n' "$plan" | awk -F'|' '$1=="myserver"{print $6}')"

case_start "otherbox: its own tcp_port and remote_socket override, no error"
check "otherbox local_port" "18744" "$(printf '%s\n' "$plan" | awk -F'|' '$1=="otherbox"{print $3}')"
check "otherbox remote override" "/run/user/1000/sot/sessions/sot.sock" \
    "$(printf '%s\n' "$plan" | awk -F'|' '$1=="otherbox"{print $5}')"
check "otherbox has no error" "" "$(printf '%s\n' "$plan" | awk -F'|' '$1=="otherbox"{print $6}')"

case_start "thirdbox: missing tcp_port, NOT the default -- nonfatal error, empty local_port"
check "thirdbox local_port empty" "" "$(printf '%s\n' "$plan" | awk -F'|' '$1=="thirdbox"{print $3}')"
check "thirdbox names the host and field" "host 'thirdbox' has no tcp_port (required for a remote tunnel)" \
    "$(printf '%s\n' "$plan" | awk -F'|' '$1=="thirdbox"{print $6}')"

case_start "a default_host that matches no KEY never fabricates a default (and never matches by alias)"
plan_no_default="$(sot_tunnel_plan "$FIXTURE" nonexistent-key 18743)"
check "thirdbox still errors (no default fallback applies)" "host 'thirdbox' has no tcp_port (required for a remote tunnel)" \
    "$(printf '%s\n' "$plan_no_default" | awk -F'|' '$1=="thirdbox"{print $6}')"
plan_by_alias="$(sot_tunnel_plan "$FIXTURE" myserver-alias 18743)"
check "myserver-alias (the ssh_alias, not the key) does NOT count as the default" \
    "host 'myserver' has no tcp_port (required for a remote tunnel)" \
    "$(printf '%s\n' "$plan_by_alias" | awk -F'|' '$1=="myserver"{print $6}')"

echo
if [ "$fails" -eq 0 ]; then
    echo "test-tunnel-plan: all checks passed"
else
    echo "test-tunnel-plan: $fails check(s) FAILED"
    exit 1
fi
