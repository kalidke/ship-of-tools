#!/usr/bin/env bash
# hosts-toml-role.sh — regression tests for install.sh's hosts.toml editing.
#
# The case that earns this file is `preserves_user_config_on_a_role_change`.
# The installer used to back up and rewrite hosts.toml from a stub whenever an
# exact-line grep for `default_host` missed. A --be-only run did exactly that
# on a machine whose file carried a [monitor] table (ADR 0020): the table went
# to a .bak nobody read, the daemon booted, found no [monitor], and monitored
# only the local host — silently, because one monitored host is a legitimate
# configuration. Every assertion here fails against that old behavior.
#
# Run: scripts/tests/hosts-toml-role.sh

set -euo pipefail

SOT_INSTALL_SOURCE_ONLY=1
export SOT_INSTALL_SOURCE_ONLY
# shellcheck source=../install.sh
. "$(dirname "$0")/../install.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
fails=0
current=""

check() {  # <description> <condition-as-args>
    if "${@:2}"; then
        printf '    ok   %s\n' "$1"
    else
        printf '    FAIL %s\n' "$1"
        fails=$((fails + 1))
    fi
}
has()     { grep -qF -- "$2" "$1"; }
lacks()   { ! grep -qF -- "$2" "$1"; }
count_of() { [ "$(grep -cF -- "$2" "$1")" = "$3" ]; }
case_start() { current="$1"; printf '  %s\n' "$1"; }

LOCAL_ENTRY="$(printf '# Local backend on the per-user socket — no SSH involved for the same-machine role.\n[host.local]\nsocket = "/run/user/1000/sot/sessions/sot.sock"')"

# A file shaped like the one the incident destroyed.
populated() {
    cat > "$1" <<'EOF'
# My hand-written notes about this fleet.
default_host = "boxa"

[host.boxa]
ssh_alias = "boxa"
remote_repo = "/home/me/project"
tcp_port = 18743

[monitor]
boxa = "boxa"
boxb = "boxb"
boxc = "boxc"
EOF
}

# ---------------------------------------------------------------------------
case_start "writes a usable stub when there is no file"
f="$WORK/new.toml"
hosts_toml_apply_role "$f" local "$LOCAL_ENTRY" >/dev/null
check "default_host set"     has "$f" 'default_host = "local"'
check "entry written"        has "$f" '[host.local]'
check "socket written"       has "$f" 'socket = "/run/user/1000/sot/sessions/sot.sock"'
check "default_host is first" test "$(grep -vE '^\s*(#|$)' "$f" | head -1)" = 'default_host = "local"'

# ---------------------------------------------------------------------------
case_start "preserves user config on a role change (the incident)"
f="$WORK/role-change.toml"; populated "$f"
hosts_toml_apply_role "$f" local "$LOCAL_ENTRY" >/dev/null
check "default_host repointed"   has "$f" 'default_host = "local"'
check "old default gone"         lacks "$f" 'default_host = "boxa"'
check "new entry added"          has "$f" '[host.local]'
check "[monitor] SURVIVED"       has "$f" '[monitor]'
check "monitor entries survived" has "$f" 'boxc = "boxc"'
check "other host survived"      has "$f" '[host.boxa]'
check "its keys survived"        has "$f" 'remote_repo = "/home/me/project"'
check "comments survived"        has "$f" '# My hand-written notes'

# ---------------------------------------------------------------------------
case_start "is idempotent"
before="$(cat "$f")"
hosts_toml_apply_role "$f" local "$LOCAL_ENTRY" >/dev/null
check "second run is a no-op"      test "$before" = "$(cat "$f")"
check "no duplicate entry"         count_of "$f" '[host.local]' 1
check "no leftover temp file"      test ! -e "$f.new"

# ---------------------------------------------------------------------------
case_start "inserts a missing default_host ABOVE the first table"
f="$WORK/no-default.toml"
printf '[monitor]\nboxa = "boxa"\n' > "$f"
hosts_toml_apply_role "$f" local "$LOCAL_ENTRY" >/dev/null
check "default_host on line 1" test "$(head -1 "$f")" = 'default_host = "local"'
check "[monitor] survived"     has "$f" '[monitor]'

# ---------------------------------------------------------------------------
case_start "rewrites whitespace and single-quoted forms"
f="$WORK/quoted.toml"
printf "   default_host   =   'boxa'\n\n[monitor]\nboxa = \"boxa\"\n" > "$f"
hosts_toml_apply_role "$f" local "$LOCAL_ENTRY" >/dev/null
check "replaced, not duplicated" count_of "$f" 'default_host' 1
check "value is the role's"      has "$f" 'default_host = "local"'

# ---------------------------------------------------------------------------
case_start "leaves a default_host that belongs to a table alone"
f="$WORK/keyed.toml"
printf '[host.boxa]\ndefault_host = "confusing"\n' > "$f"
hosts_toml_apply_role "$f" local "$LOCAL_ENTRY" >/dev/null
check "table key untouched"   has "$f" 'default_host = "confusing"'
check "real one prepended"    test "$(head -1 "$f")" = 'default_host = "local"'

# ---------------------------------------------------------------------------
case_start "treats an alias as a string, not a regex"
f="$WORK/regex.toml"
printf 'default_host = "x"\n\n[host.axb]\nssh_alias = "axb"\n' > "$f"
entry="$(printf '[host.a.b]\nssh_alias = "a.b"')"
hosts_toml_apply_role "$f" "a.b" "$entry" >/dev/null
check "[host.a.b] added"           has "$f" '[host.a.b]'
check "[host.axb] not mistaken"    has "$f" '[host.axb]'
# And the reverse: a second run must find [host.a.b] by exact match.
hosts_toml_apply_role "$f" "a.b" "$entry" >/dev/null
check "no duplicate on re-run"     count_of "$f" '[host.a.b]' 1

# ---------------------------------------------------------------------------
printf '\n'
if [ "$fails" -eq 0 ]; then
    printf 'hosts-toml-role: all checks passed\n'
else
    printf 'hosts-toml-role: %d check(s) FAILED\n' "$fails" >&2
    exit 1
fi
