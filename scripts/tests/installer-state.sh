#!/usr/bin/env bash
# installer-state.sh — the decision matrix for "what does this install do".
#
# install.json used to record a `role` that nothing ever read back, so a
# re-run on a machine with a live backend could silently re-role it. That
# manifest-role gate is gone: the declared topology (hosts.toml) is now the
# source of truth for what a listed host installs and enables, and a
# listless box falls back to its flags — nothing is persisted or compared
# across runs any more. What IS still worth guarding is a live process: a
# shared-home cluster (four boxes, one NFS $HOME) sees every host's
# sotd.service unit FILE, so the guard has to ask systemd whether one is
# actually RUNNING here, not just whether a file exists.
#
# Run: scripts/tests/installer-state.sh

set -euo pipefail

SOT_INSTALL_SOURCE_ONLY=1
export SOT_INSTALL_SOURCE_ONLY
# shellcheck source=../install.sh
. "$(dirname "$0")/../install.sh"

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
starts_with() {  # <description> <prefix> <actual>
    case "$3" in
        "$2"*) printf '    ok   %s\n' "$1" ;;
        *) printf '    FAIL %s\n      expected prefix: %s\n      actual:          %s\n' "$1" "$2" "$3"
           fails=$((fails + 1)) ;;
    esac
}
case_start() { printf '  %s\n' "$1"; }

# ---------------------------------------------------------------------------
case_start "deriving role from the declared topology (installer_topology_role)"
# Plain-line `sotd topology status` output (rust/protocol/src/topology.rs
# status_table) — this reads THAT table, not hosts.toml itself; the one
# parser stays the one parser.
STATUS_TABLE="$(printf 'HOST DECLARED\nhub-box hub,daemon\nhost-4 daemon,frontend\nhost-2 daemon\nhost-3 shell\nlaptop frontend\n')"

check "a host listed daemon-only" \
    "daemon:1 frontend:0" "$(installer_topology_role "$STATUS_TABLE" host-2)"
check "a host listed frontend-only" \
    "daemon:0 frontend:1" "$(installer_topology_role "$STATUS_TABLE" laptop)"
check "a host listed as neither (shell)" \
    "daemon:0 frontend:0" "$(installer_topology_role "$STATUS_TABLE" host-3)"
check "a host listed daemon and frontend" \
    "daemon:1 frontend:1" "$(installer_topology_role "$STATUS_TABLE" host-4)"
check "a host not named in the table" \
    "none" "$(installer_topology_role "$STATUS_TABLE" nowhere)"
check "no table at all (no hosts.toml yet)" \
    "none" "$(installer_topology_role "" laptop)"

# ---------------------------------------------------------------------------
case_start "the flags fallback when there is no topology entry (installer_role_from_flags)"

check "--local"            "daemon:1 frontend:1" "$(installer_role_from_flags local)"
check "--be-only"          "daemon:1 frontend:0" "$(installer_role_from_flags be-only)"
check "--backend <alias>"  "daemon:0 frontend:1" "$(installer_role_from_flags remote)"

# ---------------------------------------------------------------------------
case_start "install.json: hub + daemon/frontend recorded, no role field (installer_manifest_json)"

json="$(installer_manifest_json "$WORK/prefix" "$WORK/config" systemd 0.6.0 v0.6.0 abc123 2026-09-15T00:00:00Z myhub 1 0)"
check "hub recorded"          1 "$(printf '%s' "$json" | grep -c '"hub": "myhub"')"
check "no role key"           0 "$(printf '%s' "$json" | grep -c '"role"')"
check "other fields kept"     1 "$(printf '%s' "$json" | grep -c '"schema": 1')"
check "daemon recorded true"  1 "$(printf '%s' "$json" | grep -c '"daemon": true')"
check "frontend recorded false" 1 "$(printf '%s' "$json" | grep -c '"frontend": false')"

nohub="$(installer_manifest_json "$WORK/prefix" "$WORK/config" none 0.6.0 v0.6.0 abc123 2026-09-15T00:00:00Z "" 0 1)"
check "hub is empty when not given" 1 "$(printf '%s' "$nohub" | grep -c '"hub": ""')"
check "daemon recorded false"       1 "$(printf '%s' "$nohub" | grep -c '"daemon": false')"
check "frontend recorded true"      1 "$(printf '%s' "$nohub" | grep -c '"frontend": true')"

# ---------------------------------------------------------------------------
case_start "unit ownership: ExecStart path extraction (old + wrapped forms)"
# Codex round on PR #164: installer_unit_owner_path used to take the FIRST
# WORD of ExecStart, which was the sotd path in the old direct form but
# became "/bin/bash" once ExecStart wraps the daemon in a shell that sources
# ~/.bashrc (deploy/sotd.service). A reinstall then treated its own unit as
# foreign and aborted (or demanded --force-role-change) every time. Pin both
# shapes so the regex never regresses to first-word-only again.

old_unit="$(cat <<'UNITEOF'
ExecStartPre=-/opt/sot/bin/sot-apply
ExecStart=/opt/sot/bin/sotd --project-root /home/u --label sot
Restart=always
UNITEOF
)"
check "old direct ExecStart form" \
    "/opt/sot/bin/sotd" "$(printf '%s\n' "$old_unit" | installer_unit_owner_path)"

wrapped_unit="$(cat <<'UNITEOF'
ExecStartPre=-/opt/sot/bin/sot-apply
ExecStart=/bin/bash -c '[ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"; exec "/opt/sot/bin/sotd" --project-root "/home/u" --label sot'
Restart=always
UNITEOF
)"
check "wrapped bash -c ExecStart form (current unit)" \
    "/opt/sot/bin/sotd" "$(printf '%s\n' "$wrapped_unit" | installer_unit_owner_path)"

no_execstart="$(cat <<'UNITEOF'
[Unit]
Description=something else entirely
UNITEOF
)"
check "no ExecStart line yields empty" \
    "" "$(printf '%s\n' "$no_execstart" | installer_unit_owner_path)"

# ---------------------------------------------------------------------------
case_start "the running-daemon guard: is-active, not just a unit file (installer_running_daemon_bin)"
# The incident this replaces: a shared-home box saw ANOTHER host's
# sotd.service FILE over NFS (both hosts' ~/.config/systemd/user is the same
# directory) and refused every fresh install there. `systemctl --user cat`
# alone can't tell the difference — only `is-active`, kept per-host outside
# the shared file, can.

stubbin="$WORK/stubbin"
mkdir -p "$stubbin"
active_flag="$WORK/active"
unit_text="$WORK/unit.txt"
printf 'ExecStart=/opt/other-prefix/bin/sotd --project-root /home/u --label sot\n' > "$unit_text"
cat > "$stubbin/systemctl" <<STUBEOF
#!/usr/bin/env bash
case "\$*" in
    *"is-active --quiet sotd.service"*) [ -f "$active_flag" ] && exit 0 || exit 3 ;;
    *"cat sotd.service"*) cat "$unit_text" 2>/dev/null ;;
    *) exit 0 ;;
esac
STUBEOF
chmod +x "$stubbin/systemctl"

rm -f "$active_flag"
check "a unit file for another host, not active, is not a daemon here" \
    "" "$(PATH="$stubbin:$PATH" installer_running_daemon_bin)"

: > "$active_flag"
check "active names the binary it runs" \
    "/opt/other-prefix/bin/sotd" "$(PATH="$stubbin:$PATH" installer_running_daemon_bin)"
rm -f "$active_flag"

emptypath="$WORK/empty-path"
mkdir -p "$emptypath"
check "no systemctl on PATH at all is not a daemon here" \
    "" "$(PATH="$emptypath" installer_running_daemon_bin)"

# ---------------------------------------------------------------------------
case_start "the running-daemon decision (installer_running_daemon_decision)"

check "nothing running installs" \
    "allow" "$(installer_running_daemon_decision "" /opt/sot 0)"
check "a daemon from THIS prefix is an upgrade" \
    "allow" "$(installer_running_daemon_decision /opt/sot/bin/sotd /opt/sot 0)"
starts_with "a daemon from a different prefix refuses" \
    "refuse:" "$(installer_running_daemon_decision /opt/other/bin/sotd /opt/sot 0)"
check "--force-role-change overrides it" \
    "allow" "$(installer_running_daemon_decision /opt/other/bin/sotd /opt/sot 1)"

# ---------------------------------------------------------------------------
case_start "retiring the tmux keeper unit on upgrade"
# v0.6.0 deleted the tmux runtime; an upgrade must remove the old
# sot-tmux.service unit (ADR 0038, superseded) instead of leaving it behind.

sysdir="$WORK/systemd-user"
mkdir -p "$sysdir"
: > "$sysdir/sot-tmux.service"

tmuxstub="$WORK/tmuxstub"
mkdir -p "$tmuxstub"
systemctl_log="$WORK/systemctl.log"
: > "$systemctl_log"
cat > "$tmuxstub/systemctl" <<STUBEOF
#!/usr/bin/env bash
printf '%s\n' "\$*" >> "$systemctl_log"
STUBEOF
chmod +x "$tmuxstub/systemctl"

PATH="$tmuxstub:$PATH" installer_retire_tmux_unit "$sysdir"

check "the unit file is removed" \
    "0" "$([ -f "$sysdir/sot-tmux.service" ] && echo 1 || echo 0)"
check "systemctl was called to disable --now the unit" \
    "--user disable --now sot-tmux.service" "$(cat "$systemctl_log")"

# No unit present → no-op, no systemctl call.
: > "$systemctl_log"
installer_retire_tmux_unit "$WORK/no-such-dir"
check "a missing unit is a no-op" "" "$(cat "$systemctl_log")"

# ---------------------------------------------------------------------------
case_start "wrapper owner extraction (installer_wrapper_owner_prefix)"
check "today's all-in-one wrapper" \
    "/opt/sot-a" "$(printf 'PENDING="/opt/sot-a/updates/pending-linux-x86_64.json"\n' | installer_wrapper_owner_prefix)"
check "today's remote-backend wrapper" \
    "/opt/sot-b" "$(printf 'export SOT_FRONTEND_BIN="/opt/sot-b/bin/sot"\n' | installer_wrapper_owner_prefix)"
check "the legacy one-liner" \
    "/opt/sot-c" "$(printf 'exec "/opt/sot-c/bin/sot" "\$@"\n' | installer_wrapper_owner_prefix)"
check "unrecognized content yields no owner" \
    "" "$(printf '#!/usr/bin/env bash\necho hi\n' | installer_wrapper_owner_prefix)"

case_start "desktop/app exec target extraction (installer_integration_exec_target)"
check "a .desktop Exec= line" \
    "/home/user/.local/bin/sot-launch" "$(printf '[Desktop Entry]\nExec=/home/user/.local/bin/sot-launch\n' | installer_integration_exec_target)"
check "a macOS app's exec shim" \
    "/home/user/.local/bin/sot-launch" "$(printf '#!/usr/bin/env bash\nexec "/home/user/.local/bin/sot-launch"\n' | installer_integration_exec_target)"

case_start "one integration file's decision (installer_integration_decision)"
check "same owner as this prefix is an upgrade" \
    "allow" "$(installer_integration_decision /f/sot-launch /opt/sot-a /opt/sot-a 0)"
check "a different owner refuses" \
    "refuse:/f/sot-launch belongs to the install at /opt/sot-a; this install targets /opt/sot-b" \
    "$(installer_integration_decision /f/sot-launch /opt/sot-a /opt/sot-b 0)"
check "--force-role-change overrides a different owner" \
    "allow" "$(installer_integration_decision /f/sot-launch /opt/sot-a /opt/sot-b 1)"
check "no identifiable owner is unresolvable, force or not" \
    "unresolvable:/f/sot-launch exists but its owner could not be determined — move it aside and re-run" \
    "$(installer_integration_decision /f/sot-launch "" /opt/sot-b 1)"

case_start "the whole gate (installer_ownership_gate) — scratch HOME + prefix, no real systemctl"
GHOME="$WORK/gate-home"; GPREFIX="$WORK/gate-prefix-a"; OTHER_PREFIX="$WORK/gate-prefix-b"
snapshot() { find "$1" -printf '%y %p\n' 2>/dev/null | sort; find "$1" -type f -exec sha256sum {} + 2>/dev/null | sort; }

rm -rf "$GHOME"; mkdir -p "$GHOME"
check "empty sentinel HOME + no running daemon integrates" \
    "allow" "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"
check "be-only (want_frontend=0) never looks at FE files" \
    "allow" "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 0 0)"

rm -rf "$GHOME"; mkdir -p "$GHOME/.local/bin"
printf 'PENDING="%s/updates/pending-linux-x86_64.json"\n' "$OTHER_PREFIX" > "$GHOME/.local/bin/sot-launch"
before="$(snapshot "$GHOME")"
check "a wrapper owned by another prefix stops the install" \
    "refuse:$GHOME/.local/bin/sot-launch belongs to the install at $OTHER_PREFIX; this install targets $GPREFIX" \
    "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"
after="$(snapshot "$GHOME")"
check "nothing under HOME changed while refusing" "$before" "$after"
check "--force-role-change overrides the same wrapper conflict" \
    "allow" "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 1)"

rm -rf "$GHOME"; mkdir -p "$GHOME/.local/bin"
printf 'export SOT_FRONTEND_BIN="%s/bin/sot"\n' "$GPREFIX" > "$GHOME/.local/bin/sot-launch"
check "a wrapper already owned by this prefix is an upgrade" \
    "allow" "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"

rm -rf "$GHOME"; mkdir -p "$GHOME/.local/bin"
printf 'exec "%s/bin/sot" "\$@"\n' "$OTHER_PREFIX" > "$GHOME/.local/bin/sot-launch"
check "the legacy one-liner wrapper is recognized and refuses when foreign" \
    "refuse:$GHOME/.local/bin/sot-launch belongs to the install at $OTHER_PREFIX; this install targets $GPREFIX" \
    "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"

rm -rf "$GHOME"; mkdir -p "$GHOME/.local/bin"
printf '#!/usr/bin/env bash\necho not a known wrapper shape\n' > "$GHOME/.local/bin/sot-launch"
check "an unrecognized wrapper refuses even with --force-role-change" \
    "unresolvable:$GHOME/.local/bin/sot-launch exists but its owner could not be determined — move it aside and re-run" \
    "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 1)"

rm -rf "$GHOME"; mkdir -p "$GHOME/.local/share/applications"
printf '[Desktop Entry]\nExec=%s/bin/sot\n' "$OTHER_PREFIX" > "$GHOME/.local/share/applications/ship-of-tools.desktop"
check "a desktop entry not launching this install's wrapper is unresolvable" \
    "unresolvable:$GHOME/.local/share/applications/ship-of-tools.desktop exists but does not launch this install's wrapper — move it aside and re-run" \
    "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"

rm -rf "$GHOME"; mkdir -p "$GHOME/.local/share/applications"
printf '[Desktop Entry]\nExec=%s/.local/bin/sot-launch\n' "$GHOME" > "$GHOME/.local/share/applications/ship-of-tools.desktop"
check "a desktop entry launching the (absent) wrapper defers to it and allows" \
    "allow" "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"

rm -rf "$GHOME"; mkdir -p "$GHOME/Applications/Ship of Tools.app/Contents/MacOS"
printf '#!/usr/bin/env bash\nexec "%s/bin/sot"\n' "$OTHER_PREFIX" > "$GHOME/Applications/Ship of Tools.app/Contents/MacOS/sot-launch"
check "a macOS app not launching this install's wrapper is unresolvable" \
    "unresolvable:$GHOME/Applications/Ship of Tools.app/Contents/MacOS/sot-launch exists but does not launch this install's wrapper — move it aside and re-run" \
    "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Darwin 1 0)"
check "the same app is ignored entirely on Linux" \
    "allow" "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"

rm -rf "$GHOME"; mkdir -p "$GHOME"
check "a live daemon from another prefix refuses before any FE file is even looked at" \
    "refuse:the sotd.service running for this user runs $OTHER_PREFIX/bin/sotd; this install targets $GPREFIX/bin/sotd" \
    "$(installer_ownership_gate "$OTHER_PREFIX/bin/sotd" "$GHOME" "$GPREFIX" Linux 1 0)"

# ---------------------------------------------------------------------------
printf '\n'
if [ "$fails" -eq 0 ]; then
    printf 'installer-state: all checks passed\n'
else
    printf 'installer-state: %d check(s) FAILED\n' "$fails" >&2
    exit 1
fi
