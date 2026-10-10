#!/usr/bin/env bash
# installer-state.sh — the decision matrix for "what does this install do".
#
# The declared topology (hosts.toml) decides what a listed host installs and
# enables, and a listless box falls back to its flags; no role is read back
# from install.json. Two things are guarded. A live process: a shared home
# shows every host the same sotd.service FILE, so the guard asks systemd
# whether one RUNS here. And, under a declared topology, the install record:
# a run that would record no daemon may not rewrite one that records a daemon
# (installer_record_decision, tested in installer-shared-home.sh), because a
# shared home's one install serves the hosts that run sotd. A refused run
# writes nothing under the prefix: the gates come before its layout.
#
# Run: scripts/tests/installer-state.sh

set -euo pipefail

# shellcheck source=installer-support.sh
. "$(dirname "$0")/installer-support.sh"

# Offline selection; the broader caller matrix runs in trust_premises.rs.
if [ "${1:-}" = --trust-only ]; then
    d="$WORK/trust-delegation"
    mkdir -p "$d/home" "$d/config/sot"
    settings="$d/config/sot/settings.toml"
    printf '# retained\n[layout]\npreset = "auto"\n' > "$settings"
    HOME="$d/home" XDG_CONFIG_HOME="$d/config" installer_declare_trust "${SOT_TEST_TRUST_SOTD:?real offline owner required}" "$d/home" || true
    check 'W1 C4 Unix headerless declaration' yes "$(grep -q '^root_prefix = ' "$settings" && echo yes || echo no)"
    [ "$fails" -eq 0 ] || exit 1
    printf 'W1 C4 Unix delegation PASS: real owner; headerless settings\n'
    exit 0
fi

# ---------------------------------------------------------------------------
case_start "deriving role from the declared topology (installer_topology_role)"
# Plain-line `sotd topology status` output (rust/protocol/src/topology/mod.rs
# status_table) — this reads THAT table, not hosts.toml itself; the one
# parser stays the one parser.
STATUS_TABLE="$(printf 'HOST DECLARED\nhub-box hub,daemon\nhost-4 daemon,frontend\nhost-2 daemon\nhost-3 shell\nlaptop frontend\n')"

check "a host listed daemon-only" \
    "daemon:1 frontend:0" "$(installer_topology_role "$STATUS_TABLE" host-2)"
check "a host listed frontend-only installs its own local daemon" \
    "daemon:1 frontend:1" "$(installer_topology_role "$STATUS_TABLE" laptop)"
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
case_start "a frontend-only box runs its own private local daemon, as on Windows"
# The defect: a host the topology lists `frontend` alone resolved daemon:0,
# so the install wrote no unit and disabled an existing sotd.service, and
# the window on that box had no daemon of its own to open a session on.
# STATUS_TABLE above is the topology stub; systemctl and loginctl are
# logging stubs and HOME is scratch, so nothing here touches a live unit.
fe_install() {  # <dir> <want-daemon 0|1> <unit enabled 0|1> <be-alias or "">: steps 6-8
    local d="$1"
    mkdir -p "$d/home/.local/bin" "$d/prefix" "$d/stubs"
    : > "$d/log"
    cat > "$d/stubs/systemctl" <<STUBEOF
#!/bin/sh
printf 'systemctl %s\n' "\$*" >> "$d/log"
case "\$*" in *is-enabled*) [ "$3" = 1 ] ;; esac
STUBEOF
    cat > "$d/stubs/loginctl" <<STUBEOF
#!/bin/sh
printf 'loginctl %s\n' "\$*" >> "$d/log"
STUBEOF
    chmod +x "$d/stubs/systemctl" "$d/stubs/loginctl"
    (
        HOME="$d/home"; PATH="$d/stubs:$PATH"
        installer_retire_local_service "$2" "$d/prefix" 0
        [ "$2" = 0 ] || installer_enable_local_service "$d/prefix" "$(dirname "$0")/../../deploy/sotd.service" "$d/sot.sock"
        installer_render_wrapper "$d/prefix" testtarget "$4" "$d/home/.local/bin/sot-launch"
    ) >/dev/null 2>&1 || true
}
fe_role="$(installer_topology_role "$STATUS_TABLE" laptop)"
fe_want=0
case "$fe_role" in *"daemon:1"*) fe_want=1 ;; esac

d="$WORK/fe-only"; fe_install "$d" "$fe_want" 0 ""
check "the unit is rendered for this prefix" \
    "$d/prefix/bin/sotd" "$({ sot_unit_owner_path < "$d/home/.config/systemd/user/sotd.service"; } 2>/dev/null)"
check "the unit is enabled and started" \
    "1" "$(grep -c -- '^systemctl --user enable --now sotd.service$' "$d/log" || true)"
check "the wrapper ensures this box's own daemon before every window" \
    "1" "$(grep -c 'sot_daemon_ensure ' "$d/home/.local/bin/sot-launch" 2>/dev/null || true)"
check "the window dials that daemon's own socket" \
    "1" "$(grep -c -- '/bin/sot" --socket "\$SOCKET"$' "$d/home/.local/bin/sot-launch" 2>/dev/null || true)"
check "no disable is ever issued" "0" "$(grep -c 'disable' "$d/log" || true)"
check "install.json records daemon true" \
    "1" "$(installer_manifest_json "$d/prefix" "$WORK/config" systemd 0.6.0 v0.6.0 abc123 2026-09-15T00:00:00Z "" "$fe_want" 1 | grep -c '"daemon": true' || true)"

d="$WORK/fe-only-enabled"; fe_install "$d" "$fe_want" 1 ""
check "an existing enabled sotd.service is never disabled" "0" "$(grep -c 'disable' "$d/log" || true)"
check "and stays enabled" \
    "1" "$(grep -c -- '^systemctl --user enable --now sotd.service$' "$d/log" || true)"

# The one exception: --backend <alias> names a remote backend explicitly.
d="$WORK/fe-backend"; fe_install "$d" "$(case "$(installer_role_from_flags remote)" in *"daemon:1"*) echo 1 ;; *) echo 0 ;; esac)" 1 be-alias
check "--backend: an enabled local unit is still disabled, none enabled" \
    "1 0" "$(grep -c -- '--user disable --now sotd.service' "$d/log" || true) $(grep -c 'enable --now' "$d/log" || true)"
check "--backend: the wrapper names the remote backend and carries no ensure" \
    "1 0" "$(grep -c '^export SOT_HOST="be-alias"$' "$d/home/.local/bin/sot-launch" 2>/dev/null || true) $(grep -c 'sot_daemon_ensure' "$d/home/.local/bin/sot-launch" 2>/dev/null || true)"

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

rm -f "${active_flag:?}"
check "a unit file for another host, not active, is not a daemon here" \
    "" "$(PATH="$stubbin:$PATH" installer_running_daemon_bin)"

: > "$active_flag"
check "active names the binary it runs" \
    "/opt/other-prefix/bin/sotd" "$(PATH="$stubbin:$PATH" installer_running_daemon_bin)"
rm -f "${active_flag:?}"

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

rm -rf "${GHOME:?}"; mkdir -p "$GHOME"
check "empty sentinel HOME + no running daemon integrates" \
    "allow" "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"
check "be-only (want_frontend=0) never looks at FE files" \
    "allow" "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 0 0)"

rm -rf "${GHOME:?}"; mkdir -p "$GHOME/.local/bin"
printf 'PENDING="%s/updates/pending-linux-x86_64.json"\n' "$OTHER_PREFIX" > "$GHOME/.local/bin/sot-launch"
before="$(snapshot "$GHOME")"
check "a wrapper owned by another prefix stops the install" \
    "refuse:$GHOME/.local/bin/sot-launch belongs to the install at $OTHER_PREFIX; this install targets $GPREFIX" \
    "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"
after="$(snapshot "$GHOME")"
check "nothing under HOME changed while refusing" "$before" "$after"
check "--force-role-change overrides the same wrapper conflict" \
    "allow" "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 1)"

rm -rf "${GHOME:?}"; mkdir -p "$GHOME/.local/bin"
printf 'export SOT_FRONTEND_BIN="%s/bin/sot"\n' "$GPREFIX" > "$GHOME/.local/bin/sot-launch"
check "a wrapper already owned by this prefix is an upgrade" \
    "allow" "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"

rm -rf "${GHOME:?}"; mkdir -p "$GHOME/.local/bin"
printf 'exec "%s/bin/sot" "\$@"\n' "$OTHER_PREFIX" > "$GHOME/.local/bin/sot-launch"
check "the legacy one-liner wrapper is recognized and refuses when foreign" \
    "refuse:$GHOME/.local/bin/sot-launch belongs to the install at $OTHER_PREFIX; this install targets $GPREFIX" \
    "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"

rm -rf "${GHOME:?}"; mkdir -p "$GHOME/.local/bin"
printf '#!/usr/bin/env bash\necho not a known wrapper shape\n' > "$GHOME/.local/bin/sot-launch"
check "an unrecognized wrapper refuses even with --force-role-change" \
    "unresolvable:$GHOME/.local/bin/sot-launch exists but its owner could not be determined — move it aside and re-run" \
    "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 1)"

rm -rf "${GHOME:?}"; mkdir -p "$GHOME/.local/share/applications"
printf '[Desktop Entry]\nExec=%s/bin/sot\n' "$OTHER_PREFIX" > "$GHOME/.local/share/applications/ship-of-tools.desktop"
check "a desktop entry not launching this install's wrapper is unresolvable" \
    "unresolvable:$GHOME/.local/share/applications/ship-of-tools.desktop exists but does not launch this install's wrapper — move it aside and re-run" \
    "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"

rm -rf "${GHOME:?}"; mkdir -p "$GHOME/.local/share/applications"
printf '[Desktop Entry]\nExec=%s/.local/bin/sot-launch\n' "$GHOME" > "$GHOME/.local/share/applications/ship-of-tools.desktop"
check "a desktop entry launching the (absent) wrapper defers to it and allows" \
    "allow" "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"

rm -rf "${GHOME:?}"; mkdir -p "$GHOME/Applications/Ship of Tools.app/Contents/MacOS"
printf '#!/usr/bin/env bash\nexec "%s/bin/sot"\n' "$OTHER_PREFIX" > "$GHOME/Applications/Ship of Tools.app/Contents/MacOS/sot-launch"
check "a macOS app not launching this install's wrapper is unresolvable" \
    "unresolvable:$GHOME/Applications/Ship of Tools.app/Contents/MacOS/sot-launch exists but does not launch this install's wrapper — move it aside and re-run" \
    "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Darwin 1 0)"
check "the same app is ignored entirely on Linux" \
    "allow" "$(installer_ownership_gate "" "$GHOME" "$GPREFIX" Linux 1 0)"

rm -rf "${GHOME:?}"; mkdir -p "$GHOME"
check "a live daemon from another prefix refuses before any FE file is even looked at" \
    "refuse:the sotd.service running for this user runs $OTHER_PREFIX/bin/sotd; this install targets $GPREFIX/bin/sotd" \
    "$(installer_ownership_gate "$OTHER_PREFIX/bin/sotd" "$GHOME" "$GPREFIX" Linux 1 0)"

case_start "ensure_never_removes_the_socket"
# A socket the bridge refuses is not this account's daemon: the ensure starts one and the socket stays.
LBIN="$WORK/launch-bin"; mkdir -p "$LBIN"
cat > "$LBIN/sotd" <<'SOTD'
#!/bin/sh
[ "$1" != stdio-bridge ] || exit 1
: > "$(dirname "$0")/sotd-started"
SOTD
chmod +x "$LBIN/sotd"
LSOCK="$WORK/sot.sock"
python3 -c 'import socket,sys,time; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.listen(1); time.sleep(60)' "$LSOCK" &
LPID=$!
for _ in {1..100}; do [ -S "$LSOCK" ] && break; sleep 0.05; done
[ -S "$LSOCK" ] || { echo 'FAIL socket fixture did not bind' >&2; kill "$LPID"; wait "$LPID" || true; exit 1; }
LAUNCH_RC="$(SOCKET="$LSOCK" PATH="$LBIN:$PATH" bash -c '. "'"$(dirname "$0")"'/../lib/sot-daemon.sh"
    sleep() { if [ "$1" = 1 ]; then command sleep 1; fi; }  # the probe waits for real, the retries do not
    sot_daemon_ensure "'"$WORK"'/launch" "'"$LBIN"'/sotd" "$SOCKET"; echo $?' 2>/dev/null)" || LAUNCH_RC=exited
kill "$LPID"; wait "$LPID" || true
check "a socket the bridge refuses stays while the ensure starts its own daemon" \
    "1 socket=yes started=yes" \
    "$LAUNCH_RC socket=$([ -S "$LSOCK" ] && echo yes || echo no) started=$([ -e "$LBIN/sotd-started" ] && echo yes || echo no)"

# ---------------------------------------------------------------------------
# Run sot_daemon_ensure under the sandboxed PATH. Sets ENS_RC, ENS_SECS, ENS_ERR.
run_ensure() {  # <dir> <prefix> <with-systemctl 1|0>
    local d="$1" prefix="$2" sc="$3" t0 t1 path="$1/stubs:$TOOLS"
    mkdir -p "$d/stubs"
    mk_stubs "$d/stubs"
    [ "$sc" = 1 ] || rm -f "${d:?}/stubs/systemctl"
    : > "$d/log"
    t0="$(date +%s)"
    ENS_RC=0
    ( HOME="$d/home" PATH="$path" STUB_LOG="$d/log" STUB_SOCKET="$d/sot.sock" STUB_PIDFILE="$d/pid" \
        STUB_DELAY="${STUB_DELAY:-0}" STUB_EXIT="${STUB_EXIT:-}" \
        bash -c '. "$1"; sot_daemon_ensure "$2" "$3" "$4"' ensure "$LIB" "$prefix" "$d/stubs/sotd" "$d/sot.sock" \
        2> "$d/err" ) || ENS_RC=$?
    t1="$(date +%s)"
    ENS_SECS=$((t1 - t0))
    ENS_ERR="$(cat "$d/err")"
}
reap_stub() {  # <dir>: signal only the pid the stub recorded
    local pid
    pid="$(cat "$1/pid" 2>/dev/null || true)"
    [ -z "$pid" ] || kill "$pid" 2>/dev/null || true
}
# The newest nohup daemon log in a logs dir, found by name, or nothing.
newest_log() {  # <logs-dir>
    local f n=""
    for f in "$1"/sotd.[0-9]*Z-*.log; do [ -f "$f" ] && n="$f"; done
    printf '%s' "$n"
}

# ---------------------------------------------------------------------------
case_start "owner_helpers_agree"
for fixture in "$old_unit" "$wrapped_unit" "$no_execstart"; do
    check "unit owner: install.sh and the library agree" \
        "$(printf '%s\n' "$fixture" | installer_unit_owner_path)" "$(printf '%s\n' "$fixture" | sot_unit_owner_path)"
done
for fixture in 'PENDING="/opt/sot-a/updates/pending-linux-x86_64.json"' \
               'export SOT_FRONTEND_BIN="/opt/sot-b/bin/sot"' \
               'exec "/opt/sot-c/bin/sot" "$@"' \
               '#!/usr/bin/env bash'; do
    check "wrapper owner: install.sh and the library agree" \
        "$(printf '%s\n' "$fixture" | installer_wrapper_owner_prefix)" "$(printf '%s\n' "$fixture" | sot_wrapper_owner_prefix)"
done

# ---------------------------------------------------------------------------
case_start "rendered_unit_restarts_on_failure"
RU="$WORK/rendered.service"
render_sotd_unit /opt/sot-r "$(dirname "$0")/../../deploy/sotd.service" "$RU"
check "the rendered unit restarts on failure" "1" "$(grep -c '^Restart=on-failure$' "$RU" || true)"
check "the rendered unit has no Restart=always" "0" "$(grep -c '^Restart=always$' "$RU" || true)"
check "the rendered unit's owner path is the prefix's sotd" "/opt/sot-r/bin/sotd" "$(sot_unit_owner_path < "$RU")"

# ---------------------------------------------------------------------------
case_start "ensure_choice_table"
# <manifest> <unit-owner> <systemctl on PATH> -> how the backend was started
choice_row() {  # <name> <manifest systemd|none> <owner this|other> <systemctl 1|0> <expected>
    local d="$WORK/choice-$1" prefix="$WORK/choice-$1/prefix" how
    mkdir -p "$d/home/.config/systemd/user" "$prefix"
    [ "$2" != systemd ] || printf '{"service": "systemd"}\n' > "$prefix/install.json"
    if [ "$3" = this ]; then render_sotd_unit "$prefix" "$(dirname "$0")/../../deploy/sotd.service" "$d/home/.config/systemd/user/sotd.service"
    else render_sotd_unit "$WORK/elsewhere" "$(dirname "$0")/../../deploy/sotd.service" "$d/home/.config/systemd/user/sotd.service"; fi
    STUB_DELAY=0 run_ensure "$d" "$prefix" "$4"
    if grep -q -- '--user start sotd.service' "$d/log"; then how="systemctl"
    elif grep -q '^sotd ' "$d/log"; then how="nohup"
    else how="none"; fi
    check "$1 (rc 0, started by $5)" "0 $5" "$ENS_RC $how"
    [ "$5" != nohup ] || check "$1: the log is under its prefix" "yes" "$([ -n "$(newest_log "$prefix/logs")" ] && echo yes || echo no)"
    reap_stub "$d"
}
choice_row no-manifest none this 1 nohup
choice_row other-prefix systemd other 1 nohup
choice_row owned systemd this 1 systemctl
choice_row no-systemctl systemd this 0 nohup

# ---------------------------------------------------------------------------
case_start "ensure_waits_for_a_late_bind"
d="$WORK/late"; mkdir -p "$d/home"
STUB_DELAY=12 run_ensure "$d" "$d/prefix" 0
check "a daemon that binds after 12 s is waited for" "0" "$ENS_RC"
case "$ENS_ERR" in *"waiting for the backend"*) check "the waiting line is printed" ok ok ;; *) check "the waiting line is printed" "a waiting line" "$ENS_ERR" ;; esac
alive=no; kill -0 "$(cat "$d/pid")" 2>/dev/null && alive=yes
check "the started daemon is still alive" "yes" "$alive"
check "the log is under its prefix" "yes" "$([ -n "$(newest_log "$d/prefix/logs")" ] && echo yes || echo no)"
reap_stub "$d"

# ---------------------------------------------------------------------------
case_start "ensure_reports_a_dead_start"
d="$WORK/dead"; mkdir -p "$d/home"
STUB_EXIT=3 run_ensure "$d" "$d/prefix" 0
check "a start that exits without binding returns 1" "1" "$ENS_RC"
case "$ENS_ERR" in *"exited (3)"*) check "the exit code is named" ok ok ;; *) check "the exit code is named" "exited (3)" "$ENS_ERR" ;; esac
check "and does so in under 5 s" "yes" "$([ "$ENS_SECS" -lt 5 ] && echo yes || echo "no (${ENS_SECS}s)")"
check "the log is under its prefix" "yes" "$([ -n "$(newest_log "$d/prefix/logs")" ] && echo yes || echo no)"
log_now="$(newest_log "$d/prefix/logs")"
check "the error names this start's own log" "yes" \
    "$([ -n "$log_now" ] && case "$ENS_ERR" in *"see $log_now"*) echo yes ;; *) echo no ;; esac || echo no)"

# ---------------------------------------------------------------------------
case_start "ensure_keeps_old_logs"
# A previous start's log keeps its line, and this start writes a file of its own.
d="$WORK/logs-prev"; mkdir -p "$d/home" "$d/prefix/logs"
prev="$d/prefix/logs/sotd.20200101-000000-000Z-1.log"
printf 'known-line-prev\n' > "$prev"
STUB_DELAY=0 run_ensure "$d" "$d/prefix" 0
check "the previous log keeps its line" "known-line-prev" "$(cat "$prev")"
check "this start logs to a new file of its own" "yes" \
    "$(n="$(newest_log "$d/prefix/logs")"; [ -n "$n" ] && [ "$n" != "$prev" ] && echo yes || echo no)"
reap_stub "$d"
# A legacy sotd.log still held open by a daemon that is shutting down.
d="$WORK/logs-legacy"; mkdir -p "$d/home" "$d/prefix/logs"
legacy="$d/prefix/logs/sotd.log"
exec 7>>"$legacy"
printf 'known-line-legacy\n' >&7
STUB_DELAY=0 run_ensure "$d" "$d/prefix" 0
printf 'after-legacy\n' >&7
exec 7>&-
check "a held legacy sotd.log keeps its lines" "known-line-legacy after-legacy" "$(tr '\n' ' ' < "$legacy" | sed 's/ $//')"
reap_stub "$d"
# Eight old 6MB logs: over the count and over the cap, oldest first. Their
# pids are above any pid_max, so no live process protects them.
d="$WORK/logs-cap"; mkdir -p "$d/home" "$d/prefix/logs"
for i in 0 1 2 3 4 5 6 7; do truncate -s 6M "$d/prefix/logs/sotd.20200101-00000$i-000Z-$((99999990 + i)).log"; done
STUB_DELAY=0 run_ensure "$d" "$d/prefix" 0
check "the oldest five go and the newest three stay" "567" \
    "$(for i in 0 1 2 3 4 5 6 7; do [ -e "$d/prefix/logs/sotd.20200101-00000$i-000Z-$((99999990 + i)).log" ] && printf '%s' "$i"; done; true)"
reap_stub "$d"

# ---------------------------------------------------------------------------
case_start "ensure_log_folder_is_owner_only"
# The daemon copies every log line to its stdout log, and old logs hold unmasked secrets, so the folder is what keeps
# other accounts out: new, and a folder an earlier install made 755 with its files (ADR 0049, User isolation).
d="$WORK/logs-mode"; mkdir -p "$d/home"
( umask 022; STUB_DELAY=0 run_ensure "$d" "$d/prefix" 0 )
mode_of() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1" 2>/dev/null || echo unreadable; }
check "a new logs folder is owner-only under umask 022" "700" "$(mode_of "$d/prefix/logs")"
reap_stub "$d"
d="$WORK/logs-mode-old"; mkdir -p "$d/home" "$d/prefix/logs"
chmod 755 "$d/prefix/logs"
printf 'earlier\n' > "$d/prefix/logs/sotd.log"; chmod 644 "$d/prefix/logs/sotd.log"
( umask 022; STUB_DELAY=0 run_ensure "$d" "$d/prefix" 0 )
check "an existing 755 logs folder becomes owner-only" "700" "$(mode_of "$d/prefix/logs")"
check "its earlier log keeps its line" "earlier" "$(cat "$d/prefix/logs/sotd.log")"
reap_stub "$d"
# An install that ran nohup before and is systemd-owned now keeps a 755 folder with its old logs: the ensure
# secures it on the systemd path too.
d="$WORK/logs-mode-systemd"; mkdir -p "$d/home/.config/systemd/user" "$d/prefix/logs"
chmod 755 "$d/prefix/logs"
printf '{"service": "systemd"}\n' > "$d/prefix/install.json"
render_sotd_unit "$d/prefix" "$(dirname "$0")/../../deploy/sotd.service" "$d/home/.config/systemd/user/sotd.service"
( umask 022; STUB_DELAY=0 run_ensure "$d" "$d/prefix" 1 )
check "a systemd-owned install's existing logs folder becomes owner-only" "700" "$(mode_of "$d/prefix/logs")"
reap_stub "$d"

# ---------------------------------------------------------------------------
case_start "a_live_writers_log_is_kept"
# The oldest log names this shell's own live pid, as a daemon still shutting
# down names its own; six newer dead ones put the dir over the count.
d="$WORK/logs-live"; mkdir -p "$d"
live="$d/sotd.20200101-000000-000Z-$$.log"
printf 'known-line-live\n' > "$live"
for i in 1 2 3 4 5 6; do printf 'x\n' > "$d/sotd.20200101-00000$i-000Z-$((99999990 + i)).log"; done
sot_prune_logs "$d"
check "a log whose writer lives keeps its line" "known-line-live" "$(cat "$live" 2>/dev/null || echo deleted)"
check "the two oldest dead logs go in its place" "3456" \
    "$(for i in 1 2 3 4 5 6; do [ -e "$d/sotd.20200101-00000$i-000Z-$((99999990 + i)).log" ] && printf '%s' "$i"; done; true)"
# Pin: bash below 4.1 exits a set -u shell on "$@" with no positional
# parameters; this host's bash cannot show that red.
mkdir -p "$WORK/logs-empty"
check "an empty log dir prunes under set -u" "ok" "$(set -u; sot_prune_logs "$WORK/logs-empty" && echo ok)"

# ---------------------------------------------------------------------------
case_start "no_shared_tmp_log"
# Built in two pieces so this file's own text never matches.
shared="/tmp/sotd"; shared="$shared.log"
for f in "$LIB" "$0"; do
    check "$(basename "$f") names no shared $shared" "0" "$(grep -cF "$shared" "$f" || true)"
done

# ---------------------------------------------------------------------------
# Wrapper fixture: the rendered sot-launch under the sandboxed PATH. Event log
# lines: probe (the bridge), spawn (sot), rollback / apply (sot-apply), hop (a re-exec'd
# wrapper), plus the systemctl and pkill argv.
# <dir> <sot exit codes> <owned 1|0> <pending 1|0> <apply: rollback-only|consume>
mk_wrapper() {
    local d="$1" codes="$2" owned="$3" pending="$4" apply="${5:-log}" prefix="$1/prefix" lib
    lib="$prefix/repo/current/scripts/lib"
    mkdir -p "$d/stubs" "$d/home/.local/bin" "$d/home/.config/systemd/user" "$prefix/bin" "$prefix/updates" "$lib"
    mk_stubs "$d/stubs"
    cat > "$d/stubs/systemctl" <<'SC'
#!/bin/sh
printf 'systemctl %s\n' "$*" >> "$STUB_LOG"
exit 0
SC
    cat > "$d/stubs/pkill" <<'PK'
#!/bin/sh
echo pkill >> "$STUB_LOG"
exit 0
PK
    chmod +x "$d/stubs/systemctl" "$d/stubs/pkill"
    [ "$owned" = 1 ] || rm -f "${d:?}/stubs/systemctl"
    cp "$LIB" "$lib/sot-daemon.sh"
    printf '%s\n' "$codes" > "$d/codes"
    : > "$d/log"
    python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$d/sot.sock"
    cat > "$prefix/bin/sotd" <<SD
#!/bin/sh
case "\$1 \$2 \$3" in
    "stdio-bridge --endpoint unix:"*) echo probe >> "\$STUB_LOG"; exit 0 ;;
esac
[ "\$1" = session-socket-path ] && echo "$d/sot.sock"
exit 0
SD
    cat > "$prefix/bin/sot" <<'SOT'
#!/bin/sh
echo spawn >> "$STUB_LOG"
d="$(dirname "$0")/../.."
code="$(head -1 "$d/codes")"; sed -i 1d "$d/codes"
exit "${code:-0}"
SOT
    case "$apply" in
        consume)
            cat > "$prefix/bin/sot-apply" <<SA
#!/bin/sh
echo apply >> "\$STUB_LOG"
rm -f "${prefix:?}/updates/pending-testtarget.json"
printf '#!/bin/sh\necho hop >> "\$STUB_LOG"\n' > "$d/home/.local/bin/sot-launch.new"
chmod +x "$d/home/.local/bin/sot-launch.new"
mv "$d/home/.local/bin/sot-launch.new" "$d/home/.local/bin/sot-launch"
SA
            ;;
        *)
            cat > "$prefix/bin/sot-apply" <<'SA'
#!/bin/sh
case "$*" in *--rollback*) echo rollback >> "$STUB_LOG" ;; *) echo apply >> "$STUB_LOG" ;; esac
SA
            ;;
    esac
    chmod +x "$prefix/bin/sotd" "$prefix/bin/sot" "$prefix/bin/sot-apply"
    if [ "$owned" = 1 ]; then
        printf '{"service": "systemd"}\n' > "$prefix/install.json"
        render_sotd_unit "$prefix" "$(dirname "$0")/../../deploy/sotd.service" "$d/home/.config/systemd/user/sotd.service"
    fi
    [ "$pending" != 1 ] || printf '{}\n' > "$prefix/updates/pending-testtarget.json"
    : > "$prefix/updates/just-applied-testtarget"
    render_sot_launch "$prefix" testtarget "$d/home/.local/bin/sot-launch"
}
run_wrapper() {  # <dir>: sets WR_RC
    local d="$1"
    WR_RC=0
    ( HOME="$d/home" PATH="$d/stubs:$TOOLS" STUB_LOG="$d/log" "$d/home/.local/bin/sot-launch" ) >/dev/null 2>&1 || WR_RC=$?
}
events() { grep -E '^(probe|spawn|rollback|hop|started|fe )' "$1/log" | tr '\n' ' ' | sed 's/ $//'; }

# ---------------------------------------------------------------------------
case_start "wrapper_ensures_before_every_spawn"
d="$WORK/w-ensure"; mk_wrapper "$d" "$(printf '1\n1\n0')" 0 0
run_wrapper "$d"
check "the wrapper probes the socket before every spawn, rolls back after two fast crashes" \
    "0 probe spawn probe spawn rollback probe spawn" "$WR_RC $(events "$d")"

# ---------------------------------------------------------------------------
case_start "wrapper_service_ops_follow_ownership"
d="$WORK/w-own-apply"; mk_wrapper "$d" "0" 1 1
run_wrapper "$d"
check "an owned unit: apply is a try-restart, no sot-apply, no pkill" \
    "1 0 0" "$(grep -c -- '--user try-restart sotd.service' "$d/log" || true) $(grep -c '^apply$' "$d/log" || true) $(grep -c '^pkill$' "$d/log" || true)"
d="$WORK/w-own-rb"; mk_wrapper "$d" "$(printf '1\n1\n0')" 1 0
run_wrapper "$d"
check "an owned unit: rollback stops it through systemd" \
    "1 0" "$(grep -c -- '--user stop sotd.service' "$d/log" || true) $(grep -c '^pkill$' "$d/log" || true)"
d="$WORK/w-for-apply"; mk_wrapper "$d" "0" 0 1
printf '#!/bin/sh\nexit 0\n' > "$d/stubs/systemctl"; chmod +x "$d/stubs/systemctl"
run_wrapper "$d"
check "a foreign unit: apply is pkill plus sot-apply, no try-restart" \
    "0 1 1" "$(grep -c 'try-restart' "$d/log" || true) $(grep -c '^apply$' "$d/log" || true) $(grep -c '^pkill$' "$d/log" || true)"
d="$WORK/w-for-rb"; mk_wrapper "$d" "$(printf '1\n1\n0')" 0 0
printf '#!/bin/sh\nprintf "systemctl %%s\\n" "$*" >> "$STUB_LOG"\nexit 0\n' > "$d/stubs/systemctl"; chmod +x "$d/stubs/systemctl"
run_wrapper "$d"
check "a foreign unit: rollback is pkill, no systemd stop" \
    "0 1" "$(grep -c 'sotd.service' "$d/log" || true) $(grep -c '^pkill$' "$d/log" || true)"
# A real foreign unit, as sot_service_owned defines one: systemctl is here and
# the manifest says systemd, but the unit runs another prefix's sotd.
foreign_unit() {  # <dir>
    printf '{"service": "systemd"}\n' > "$1/prefix/install.json"
    render_sotd_unit "$WORK/elsewhere" "$(dirname "$0")/../../deploy/sotd.service" "$1/home/.config/systemd/user/sotd.service"
    printf '#!/bin/sh\nprintf "systemctl %%s\\n" "$*" >> "$STUB_LOG"\nexit 0\n' > "$1/stubs/systemctl"; chmod +x "$1/stubs/systemctl"
}
d="$WORK/w-real-for-apply"; mk_wrapper "$d" "0" 0 1; foreign_unit "$d"
run_wrapper "$d"
check "a real foreign unit: apply runs no service op, only pkill plus sot-apply" \
    "0 1 1" "$(grep -c 'sotd.service' "$d/log" || true) $(grep -c '^apply$' "$d/log" || true) $(grep -c '^pkill$' "$d/log" || true)"
d="$WORK/w-real-for-rb"; mk_wrapper "$d" "$(printf '1\n1\n0')" 0 0; foreign_unit "$d"
run_wrapper "$d"
check "a real foreign unit: rollback runs no service op, only pkill" \
    "0 1" "$(grep -c 'sotd.service' "$d/log" || true) $(grep -c '^pkill$' "$d/log" || true)"

# ---------------------------------------------------------------------------
case_start "wrapper_reexecs_after_apply"
d="$WORK/w-hop"; mk_wrapper "$d" "0" 0 1 consume
run_wrapper "$d"
check "an apply that consumes the pointer re-execs the wrapper and spawns nothing" \
    "0 hop" "$WR_RC $(events "$d")"
d="$WORK/w-nohop"; mk_wrapper "$d" "0" 0 1
run_wrapper "$d"
check "an apply that leaves the pointer re-execs nothing and spawns once" \
    "0 0 1" "$WR_RC $(grep -c '^hop$' "$d/log" || true) $(grep -c '^spawn$' "$d/log" || true)"

# ---------------------------------------------------------------------------
case_start "dev_launcher_ensures"
# The dev launcher (pipefail) under the sandboxed PATH. The bridge probe connects
# for real, and the sotd stub, like the daemon, unlinks a stale socket before it binds.
dev_row() {  # <dir> <stale socket 0|1> <description>
    local d="$1"
    mkdir -p "$d/repo/scripts/lib" "$d/home/.local/share/sot/bin" "$d/stubs"
    mk_stubs "$d/stubs"; rm -f "${d:?}/stubs/systemctl" "${d:?}/stubs/sotd"
    cp "$(dirname "$0")/../launch-sot.sh" "$d/repo/scripts/launch-sot.sh"
    cp "$(dirname "$0")/../lib/sot-hosts.sh" "$d/repo/scripts/lib/sot-hosts.sh"
    cp "$LIB" "$d/repo/scripts/lib/sot-daemon.sh"
    : > "$d/log"
    cat > "$d/home/.local/share/sot/bin/sotd" <<SD
#!/bin/sh
case "\$1 \$2 \$3" in
    "stdio-bridge --endpoint unix:"*)
        exec python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).connect(sys.argv[1])' "\${3#unix:}" ;;
esac
case "\$1 \$2" in
    "topology sync") exit 0 ;;
    "topology plan") printf 'self testbox\ndial testbox unix:$d/sot.sock\n'; exit 0 ;;
esac
echo "started \$0" >> "$d/log"
echo \$\$ > "$d/pid"
exec python3 -c 'import os,socket,sys,time
p = sys.argv[1]
if os.path.exists(p): os.unlink(p)
s = socket.socket(socket.AF_UNIX); s.bind(p); s.listen(1); time.sleep(30)' "$d/sot.sock"
SD
    cat > "$d/fe" <<FE
#!/bin/sh
echo "fe \$([ -S "$d/sot.sock" ] && echo yes || echo no)" >> "$d/log"
FE
    chmod +x "$d/home/.local/share/sot/bin/sotd" "$d/fe"
    [ "$2" != 1 ] || python3 -c 'import socket,sys; socket.socket(socket.AF_UNIX).bind(sys.argv[1])' "$d/sot.sock"
    ( HOME="$d/home" PATH="$d/stubs:$TOOLS" SOT_PREFIX= SOT_NO_UPDATE=1 SOT_FRONTEND_BIN="$d/fe" bash "$d/repo/scripts/launch-sot.sh" ) >/dev/null 2>&1 || true
    check "$3" "started $d/home/.local/share/sot/bin/sotd fe yes" "$(events "$d")"
    check "$3: the log is under its prefix" "yes" "$([ -n "$(newest_log "$d/home/.local/share/sot/logs")" ] && echo yes || echo no)"
    reap_stub "$d"
}
dev_row "$WORK/dev" 0 "the dev launcher starts the plan's own local backend before the frontend"
dev_row "$WORK/dev-stale" 1 "a stale socket file is not a live daemon: the dev launcher starts one"

# ---------------------------------------------------------------------------
case_start "launcher_bounds_match_ops"
OPS_WAIT="$(sed -n 's/.*pub const LAUNCH_WAIT: Duration = Duration::from_secs(\([0-9]*\)).*/\1/p' "$(dirname "$0")/../../rust/protocol/src/ops/lease.rs")"
LIB_WAIT="$(sed -n 's/^SOT_LAUNCH_WAIT_S=\([0-9]*\).*/\1/p' "$LIB")"
check "the library's launch wait is ops/lease.rs LAUNCH_WAIT" "$OPS_WAIT" "$LIB_WAIT"
PS_DAEMON="$(dirname "$0")/../sot-local-daemon.ps1"
OPS_LOCK="$(sed -n 's/.*pub const DAEMON_LOCK_WAIT: Duration = Duration::from_secs(\([0-9]*\)).*/\1/p' "$(dirname "$0")/../../rust/protocol/src/ops/lease.rs")"
PS_WAIT="$(sed -n 's/^\$LaunchWaitSeconds = \([0-9]*\).*/\1/p' "$PS_DAEMON")"
PS_LOCK="$(sed -n 's/^\$DaemonLockWaitSeconds = \([0-9]*\).*/\1/p' "$PS_DAEMON")"
check "sot-local-daemon.ps1 launch wait is ops/lease.rs LAUNCH_WAIT" "$OPS_WAIT" "$PS_WAIT"
check "sot-local-daemon.ps1 daemon-lock wait is ops/lease.rs DAEMON_LOCK_WAIT" "$OPS_LOCK" "$PS_LOCK"
OPS_RS="$(dirname "$0")/../../rust/protocol/src/ops/lease.rs"
OPS_NAMES="$(dirname "$0")/../../rust/protocol/src/ops/mod.rs"
PS_LAUNCH="$(dirname "$0")/../launch-sot.ps1"
PS_LEASE="$(dirname "$0")/../sot-lease.ps1"
OPS_LEASE_MS="$(sed -n 's/.*pub const LEASE_REPLY_WAIT: Duration = Duration::from_secs(\([0-9]*\)).*/\1/p' "$OPS_RS")"
OPS_HANDOVER="$(sed -n 's/.*pub const HANDOVER_BOUND: Duration = Duration::from_secs(\([0-9]*\)).*/\1/p' "$OPS_RS")"
PS_LEASE_MS="$(sed -n 's/^\$LeaseReplyWaitMs = \([0-9]*\).*/\1/p' "$PS_LAUNCH")"
PS_HANDOVER="$(sed -n 's/^\$HandoverBoundSeconds = \([0-9]*\).*/\1/p' "$PS_LAUNCH")"
check "launch-sot.ps1 lease reply wait is ops/lease.rs LEASE_REPLY_WAIT in ms" "$((OPS_LEASE_MS * 1000))" "$PS_LEASE_MS"
check "launch-sot.ps1 handover bound is ops/lease.rs HANDOVER_BOUND" "$OPS_HANDOVER" "$PS_HANDOVER"
OPS_FE_LEASE="$(sed -n 's/.*pub const FE_LEASE: &str = "\([^"]*\)".*/\1/p' "$OPS_NAMES")"
OPS_FE_LEAVING="$(sed -n 's/.*pub const FE_LEAVING: &str = "\([^"]*\)".*/\1/p' "$OPS_NAMES")"
check "sot-lease.ps1 names the fe.lease op" "yes" "$(grep -qF "\"op\":\"$OPS_FE_LEASE\"" "$PS_LEASE" && echo yes || echo no)"
check "sot-lease.ps1 names the fe.leaving op" "yes" "$(grep -qF "\"op\":\"$OPS_FE_LEAVING\"" "$PS_LEASE" && echo yes || echo no)"
# The golden lease line: the launcher's literal and ops/lease.rs's test line (backslashes stripped)
# share the prefix and the sorted payload keys, in order. The frame's "v" is the wire protocol
# (`Frame::req` stamps `PROTOCOL_VERSION`), so a bump that misses the launcher fails here.
WIRE_PROTO="$(sed -n 's/^pub const PROTOCOL_VERSION: u32 = \([0-9]*\);.*/\1/p' "$(dirname "$0")/../../rust/protocol/src/lib.rs")"
check "lib.rs names one wire protocol" "yes" "$([ -n "$WIRE_PROTO" ] && echo yes || echo no)"
check "sot-lease.ps1's fe.leaving frame speaks the wire protocol" "yes" "$(grep -qF "{\"v\":$WIRE_PROTO,\"id\":2,\"kind\":\"req\",\"op\":\"$OPS_FE_LEAVING\"" "$PS_LEASE" && echo yes || echo no)"
# The launcher's hello (ADR 0049, User isolation) speaks the wire protocol and names the handoff role.
HANDOFF_ROLE="$(sed -n 's/.*pub const HANDOFF_ROLE: &str = "\([^"]*\)".*/\1/p' "$(dirname "$0")/../../rust/protocol/src/ops/session.rs")"
check "session.rs names the handoff role" "yes" "$([ -n "$HANDOFF_ROLE" ] && echo yes || echo no)"
check "sot-lease.ps1's hello speaks the wire protocol" "yes" "$(grep -qF "\"op\":\"hello\",\"payload\":{\"client_id\":\"sot-launcher\",\"protocol\":$WIRE_PROTO," "$PS_LEASE" && echo yes || echo no)"
check "sot-lease.ps1's hello is a handoff" "yes" "$(grep -qF "\"role\":\"$HANDOFF_ROLE\"}}" "$PS_LEASE" && echo yes || echo no)"

# ---------------------------------------------------------------------------
printf '\n'
# ---------------------------------------------------------------------------
case_start "restart_backend_judges_this_accounts_daemon"
d="$WORK/restart-own"; mkdir -p "$d/repo/scripts/lib" "$d/repo/rust/target/release" "$d/stubs"
cp "$(dirname "$0")/../restart-backend.sh" "$d/repo/scripts/"
cp "$(dirname "$0")/../lib/sot-daemon.sh" "$d/repo/scripts/lib/"
cat > "$d/repo/rust/target/release/sotd" <<SD
#!/bin/sh
case "\$1 \$2 \$3" in
    "stdio-bridge --endpoint unix:"*) exit 0 ;;
esac
[ "\$1" = session-socket-path ] && echo "$d/sot.sock"
exit 0
SD
cat > "$d/stubs/ps" <<PS
#!/bin/sh
case "\$1" in
    -u) cat "$d/ps-own" ;;
    -eo) printf '111 /other/bin/sotd --project-root /other --label sot\n333 /x/bin/sotd --project-root /x --label sot\n222 /x/bin/sotd --project-root /x --label sot --socket $d/sot.sock\n' ;;
    -p) echo 5 ;;
esac
PS
# This account's processes as `ps -o pid=,comm=,args=` lists them. Before the daemon on exactly this socket: a process
# that is not sotd but names the socket, a socket that only begins with this one, one that matches it only as a
# pattern, and a daemon known by its label alone.
cat > "$d/ps-own" <<ROWS
666 less less /x/sotd --socket $d/sot.sock
444 sotd /x/bin/sotd --project-root /x --label sot --socket $d/sot.sock.old
777 sotd /x/bin/sotd --project-root /x --label sot --socket $d/sotXsock
333 sotd /x/bin/sotd --project-root /x --label sot
222 sotd /x/bin/sotd --project-root /x --label sot --socket $d/sot.sock
ROWS
printf '#!/bin/sh\nexit 1\n' > "$d/stubs/systemctl"
chmod +x "$d/repo/rust/target/release/sotd" "$d/stubs/ps" "$d/stubs/systemctl"
python3 -c 'import socket,sys,time; s=socket.socket(socket.AF_UNIX); s.bind(sys.argv[1]); s.listen(1); time.sleep(60)' "$d/sot.sock" &
RPID=$!
for _ in {1..100}; do [ -S "$d/sot.sock" ] && break; sleep 0.05; done
[ -S "$d/sot.sock" ] || { echo 'FAIL restart socket fixture did not bind' >&2; kill "$RPID"; wait "$RPID" || true; exit 1; }
restart_line="$(env -u SOT_SOCKET -u SOT_BACKEND_LABEL PATH="$d/stubs:$PATH" bash "$d/repo/scripts/restart-backend.sh" --check)" || true
# No daemon on this socket: the one whose label is exactly this one, never one whose label only begins with it.
printf '555 sotd /x/bin/sotd --project-root /x --label sot-dev\n333 sotd /x/bin/sotd --project-root /x --label sot\n' > "$d/ps-own"
label_line="$(env -u SOT_SOCKET -u SOT_BACKEND_LABEL PATH="$d/stubs:$PATH" bash "$d/repo/scripts/restart-backend.sh" --check)" || true
kill "$RPID"; wait "$RPID" || true
starts_with "restart judges this account's daemon on this socket" "running daemon pid 222 " "$restart_line"
starts_with "restart judges this account's daemon by its exact label" "running daemon pid 333 " "$label_line"

if [ "$fails" -eq 0 ]; then
    printf 'installer-state: all checks passed\n'
else
    printf 'installer-state: %d check(s) FAILED\n' "$fails" >&2
    exit 1
fi
