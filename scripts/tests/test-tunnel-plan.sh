#!/usr/bin/env bash
# test-tunnel-plan.sh -- regression harness for scripts/sot-hosts.sh's
# tunnel-plan builder (ADR 0042 L2b design E: one tunnel per configured
# remote, both launchers).
#
# Pure text processing (no ssh, no network) against a fixture hosts.toml,
# mirroring test-sot-apply.ps1/test-local-daemon.ps1's own "fake files are
# never executed" convention and installer-state.sh's `check`/`case_start`
# shape (this file is the bash-side sibling of
# scripts/tests/test-tunnel-plan.ps1, which exercises the same fixture
# through Get-TunnelPlan).
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

FIXTURE="$WORK/hosts.toml"
cat > "$FIXTURE" <<'EOF'
default_host = "myserver"

[host.myserver]
ssh_alias = "myserver"
remote_repo = "/home/me/project"
tcp_port = 18743

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
socket = "\\.\pipe\sot-local"
EOF

echo "=== sot_hosts_default_host / sot_hosts_table ==="
case_start "default_host"
check "default_host parsed" "myserver" "$(sot_hosts_default_host "$FIXTURE")"

case_start "hosts_table"
table="$(sot_hosts_table "$FIXTURE")"
check "four sections captured" "4" "$(printf '%s\n' "$table" | wc -l | tr -d ' ')"
check "myserver row" "myserver|myserver|/home/me/project|18743|" \
    "$(printf '%s\n' "$table" | awk -F'|' '$1=="myserver"')"
check "local row has no ssh_alias (socket= isn't a tracked field)" "local||||" \
    "$(printf '%s\n' "$table" | awk -F'|' '$1=="local"')"

echo "=== sot_tunnel_plan (default_alias=myserver, default_port=18743) ==="
plan="$(sot_tunnel_plan "$FIXTURE" myserver 18743)"

case_start "local is never in the plan (no ssh_alias)"
check "no local row" "" "$(printf '%s\n' "$plan" | awk -F'|' '$1=="local"')"

case_start "myserver (the default) keeps its own tcp_port"
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

case_start "a default_alias that matches no ssh_alias never fabricates a default"
plan_no_default="$(sot_tunnel_plan "$FIXTURE" nonexistent-alias 18743)"
check "thirdbox still errors (no default fallback applies)" "host 'thirdbox' has no tcp_port (required for a remote tunnel)" \
    "$(printf '%s\n' "$plan_no_default" | awk -F'|' '$1=="thirdbox"{print $6}')"

echo
if [ "$fails" -eq 0 ]; then
    echo "test-tunnel-plan: all checks passed"
else
    echo "test-tunnel-plan: $fails check(s) FAILED"
    exit 1
fi
